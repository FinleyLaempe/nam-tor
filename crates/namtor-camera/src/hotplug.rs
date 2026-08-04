// SPDX-License-Identifier: GPL-3.0-only
//! Hotplug detection via udev: add/remove events for V4L2 video nodes.
//!
//! Spec §4.10.4: cameras may appear or disappear at runtime. The daemon
//! tracks the set of IR-capable nodes and re-runs selection on add/remove.
//! The event source is abstracted so the state machine is testable without
//! a real udev socket.

use std::path::PathBuf;

use namtor_hwdb::nodes::V4l2Node;

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

/// Tracks whether the currently-selected device is still present.
#[derive(Debug, Clone, Default)]
pub struct HotplugState {
    has_device: bool,
}

impl HotplugState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_device(&self) -> bool {
        self.has_device
    }

    /// Applies one event, updating presence. Returns whether the currently
    /// selected device is still present after the event.
    pub fn apply(&mut self, event: DeviceEvent) -> bool {
        match event {
            DeviceEvent::Add(_) => self.has_device = true,
            DeviceEvent::Remove(_) => self.has_device = false,
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
}
