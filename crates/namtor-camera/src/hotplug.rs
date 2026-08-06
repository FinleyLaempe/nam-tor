// SPDX-License-Identifier: GPL-3.0-only
//! Hotplug detection via udev: add/remove events for V4L2 video nodes.
//!
//! Spec §4.10.4: cameras may appear or disappear at runtime. The daemon
//! tracks the set of IR-capable nodes and re-runs selection on add/remove.
//! The event source is abstracted so the state machine is testable without
//! a real udev socket.
//!
//! Spec §4.10.3 + R10: on suspend the emitter must be forced off and the
//! capture closed; on resume the camera is re-discovered (the device may
//! renumber) and the emitter re-applied before auth resumes. [`HotplugState`]
//! extends presence tracking with the selected-node path, stale-node
//! invalidation, and a `reopen_needed` flag driven by both hotplug remove
//! and suspend/resume events.

use std::path::{Path, PathBuf};

use namtor_hwdb::nodes::V4l2Node;

use crate::resume::SleepEvent;

/// A device add or remove event for one V4L2 node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    Add(V4l2Node),
    Remove(V4l2Node),
}

/// Abstracts the event source so [`HotplugState`] is unit-testable.
pub trait DeviceEventSource {
    fn next_event(&mut self) -> Option<DeviceEvent>;
}

/// Live udev monitor bound to the video4linux subsystem.
pub struct UdevMonitor {
    socket: udev::MonitorSocket,
}

impl UdevMonitor {
    pub fn new() -> Result<Self, String> {
        let socket = udev::MonitorBuilder::new()
            .map_err(|e| format!("udev monitor builder: {e}"))?
            .match_subsystem_devtype("video4linux", "")
            .map_err(|e| format!("udev match subsystem: {e}"))?
            .listen()
            .map_err(|e| format!("udev listen: {e}"))?;
        Ok(Self { socket })
    }
}

impl DeviceEventSource for UdevMonitor {
    fn next_event(&mut self) -> Option<DeviceEvent> {
        let dev = self.socket.iter().next()?;
        let dev_path = PathBuf::from(format!("/dev/{}", dev.sysname().to_string_lossy()));
        let interface = dev
            .attribute_value("bInterfaceNumber")
            .and_then(|s| s.to_str())
            .and_then(|s| u8::from_str_radix(s, 16).ok())
            .unwrap_or(0);
        let name = dev
            .attribute_value("name")
            .map(|s| s.to_string_lossy().trim().to_string())
            .unwrap_or_default();
        let is_metadata = name.to_lowercase().contains("metadata");
        let node = V4l2Node {
            dev_path,
            interface,
            is_metadata,
            name,
        };
        match dev
            .action()
            .map(|s| s.to_string_lossy().into_owned())
            .as_deref()
        {
            Some("add") => Some(DeviceEvent::Add(node)),
            Some("remove") => Some(DeviceEvent::Remove(node)),
            _ => None,
        }
    }
}

/// Tracks whether the currently-selected device is still present and whether
/// the capture needs to be reopened after a remove or a suspend/resume cycle.
///
/// `reopen_needed` is set by both [`DeviceEvent::Remove`] and
/// [`SleepEvent::Suspend`] / [`SleepEvent::Resume`]; it is cleared by
/// [`mark_reopened`](Self::mark_reopened) once the owner has reopened a
/// fresh capture stream.
#[derive(Debug, Clone, Default)]
pub struct HotplugState {
    has_device: bool,
    selected_path: Option<PathBuf>,
    reopen_needed: bool,
}

impl HotplugState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_device(&self) -> bool {
        self.has_device
    }

    /// Records the path of the currently-selected device. Required before
    /// [`capture_ready`](Self::capture_ready) can return `true`.
    pub fn set_selected(&mut self, path: PathBuf) {
        self.selected_path = Some(path);
    }

    /// The path of the currently-selected device, if any.
    pub fn selected_path(&self) -> Option<&Path> {
        self.selected_path.as_deref()
    }

    /// True when a device is present, a selection has been recorded, and no
    /// reopen is pending. The daemon may service auth requests only when this
    /// is `true`.
    pub fn capture_ready(&self) -> bool {
        self.has_device && self.selected_path.is_some() && !self.reopen_needed
    }

    /// True when the capture must be reopened before it can be used again.
    pub fn reopen_needed(&self) -> bool {
        self.reopen_needed
    }

    /// Clears the reopen flag after the owner has reopened the capture and
    /// re-applied the emitter mode.
    pub fn mark_reopened(&mut self) {
        self.reopen_needed = false;
    }

    /// True when `path` does not match the recorded selection — the device
    /// has renumbered across a suspend/resume cycle and the selection is
    /// stale. Returns `false` when no selection has been recorded.
    pub fn is_stale(&self, path: &Path) -> bool {
        match &self.selected_path {
            Some(selected) => selected != path,
            None => false,
        }
    }

    /// Applies one device event, updating presence. Returns whether the
    /// currently selected device is still present after the event. A
    /// `Remove` sets `reopen_needed` in addition to clearing presence.
    pub fn apply(&mut self, event: DeviceEvent) -> bool {
        match event {
            DeviceEvent::Add(_) => self.has_device = true,
            DeviceEvent::Remove(_) => {
                self.has_device = false;
                self.reopen_needed = true;
            }
        }
        self.has_device
    }

    /// Feeds a stream of events from the source until it returns `None`,
    /// applying each. Returns the final presence flag.
    pub fn drain<S: DeviceEventSource>(&mut self, source: &mut S) -> bool {
        while let Some(event) = source.next_event() {
            self.apply(event);
        }
        self.has_device
    }

    /// Handles a suspend/resume event. Both transitions invalidate the
    /// capture: on suspend the emitter must be forced off and the stream
    /// closed; on resume the device may have renumbered and the emitter
    /// must be re-applied. Returns [`capture_ready`](Self::capture_ready)
    /// so the caller can observe whether the state is servicable.
    pub fn handle_sleep(&mut self, event: SleepEvent) -> bool {
        match event {
            SleepEvent::Suspend | SleepEvent::Resume => self.reopen_needed = true,
        }
        self.capture_ready()
    }
}

/// Fake event source for tests.
pub struct FakeEventSource {
    events: std::collections::VecDeque<DeviceEvent>,
}

impl FakeEventSource {
    pub fn new(events: Vec<DeviceEvent>) -> Self {
        Self {
            events: events.into(),
        }
    }
}

impl DeviceEventSource for FakeEventSource {
    fn next_event(&mut self) -> Option<DeviceEvent> {
        self.events.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, dev: &str) -> V4l2Node {
        V4l2Node {
            dev_path: PathBuf::from(dev),
            interface: 3,
            is_metadata: false,
            name: name.into(),
        }
    }

    #[test]
    fn initial_state_has_no_device() {
        let s = HotplugState::new();
        assert!(!s.has_device());
    }

    #[test]
    fn add_event_sets_presence_true() {
        let mut s = HotplugState::new();
        s.apply(DeviceEvent::Add(node("IR Camera", "/dev/video2")));
        assert!(s.has_device());
    }

    #[test]
    fn remove_event_sets_presence_false() {
        let mut s = HotplugState::new();
        s.apply(DeviceEvent::Add(node("IR Camera", "/dev/video2")));
        assert!(s.has_device());
        s.apply(DeviceEvent::Remove(node("IR Camera", "/dev/video2")));
        assert!(!s.has_device());
    }

    #[test]
    fn fake_source_feeds_add_then_remove() {
        let mut s = HotplugState::new();
        let mut src = FakeEventSource::new(vec![
            DeviceEvent::Add(node("IR Camera", "/dev/video2")),
            DeviceEvent::Remove(node("IR Camera", "/dev/video2")),
        ]);
        let present = s.drain(&mut src);
        assert!(!present, "after remove, no device present");
    }

    #[test]
    fn add_then_add_keeps_present() {
        let mut s = HotplugState::new();
        s.apply(DeviceEvent::Add(node("IR Camera", "/dev/video2")));
        s.apply(DeviceEvent::Add(node("IR Camera 2", "/dev/video4")));
        assert!(s.has_device());
    }

    #[test]
    fn set_selected_makes_capture_ready() {
        let mut s = HotplugState::new();
        s.apply(DeviceEvent::Add(node("IR Camera", "/dev/video2")));
        assert!(
            !s.capture_ready(),
            "without a recorded selection, capture is not ready"
        );
        s.set_selected(PathBuf::from("/dev/video2"));
        assert!(s.capture_ready(), "after set_selected, capture is ready");
        assert_eq!(s.selected_path(), Some(Path::new("/dev/video2")));
    }

    #[test]
    fn remove_sets_reopen_needed() {
        let mut s = HotplugState::new();
        s.apply(DeviceEvent::Add(node("IR Camera", "/dev/video2")));
        s.set_selected(PathBuf::from("/dev/video2"));
        assert!(s.capture_ready());
        s.apply(DeviceEvent::Remove(node("IR Camera", "/dev/video2")));
        assert!(!s.has_device());
        assert!(
            s.reopen_needed(),
            "remove must set reopen_needed so the owner reopens the stream"
        );
        assert!(!s.capture_ready());
    }

    #[test]
    fn suspend_sets_reopen_needed() {
        let mut s = HotplugState::new();
        s.apply(DeviceEvent::Add(node("IR Camera", "/dev/video2")));
        s.set_selected(PathBuf::from("/dev/video2"));
        assert!(s.capture_ready());
        let ready = s.handle_sleep(SleepEvent::Suspend);
        assert!(s.reopen_needed(), "suspend must invalidate the capture");
        assert!(
            !ready,
            "capture_ready must be false after suspend until mark_reopened"
        );
    }

    #[test]
    fn resume_sets_reopen_needed() {
        let mut s = HotplugState::new();
        s.apply(DeviceEvent::Add(node("IR Camera", "/dev/video2")));
        s.set_selected(PathBuf::from("/dev/video2"));
        // Simulate the full cycle: suspend then resume.
        s.handle_sleep(SleepEvent::Suspend);
        s.handle_sleep(SleepEvent::Resume);
        assert!(
            s.reopen_needed(),
            "resume must require a reopen (device may have renumbered)"
        );
        assert!(!s.capture_ready());
    }

    #[test]
    fn mark_reopened_clears_flag() {
        let mut s = HotplugState::new();
        s.apply(DeviceEvent::Add(node("IR Camera", "/dev/video2")));
        s.set_selected(PathBuf::from("/dev/video2"));
        s.handle_sleep(SleepEvent::Suspend);
        assert!(s.reopen_needed());
        // Owner re-detects the device and reopens the stream.
        s.apply(DeviceEvent::Add(node("IR Camera", "/dev/video2")));
        s.mark_reopened();
        assert!(
            !s.reopen_needed(),
            "mark_reopened must clear the reopen flag"
        );
        assert!(s.capture_ready(), "after reopen the capture is servicable");
    }

    #[test]
    fn is_stale_detects_renumber() {
        let mut s = HotplugState::new();
        s.apply(DeviceEvent::Add(node("IR Camera", "/dev/video2")));
        s.set_selected(PathBuf::from("/dev/video2"));
        assert!(
            !s.is_stale(Path::new("/dev/video2")),
            "same path is not stale"
        );
        assert!(
            s.is_stale(Path::new("/dev/video4")),
            "renumbered path is stale"
        );
        // No selection recorded → nothing to be stale against.
        let s2 = HotplugState::new();
        assert!(!s2.is_stale(Path::new("/dev/video2")));
    }
}
