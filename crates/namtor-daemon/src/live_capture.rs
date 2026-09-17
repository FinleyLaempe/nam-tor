// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::daemon::CaptureFactory;
use crate::DaemonError;
use namtor_camera::always_on::ProtectionLevel;
use namtor_camera::emitter_ctrl::{ApplyOutcome, EmitterController, EnvConsent};
use namtor_camera::emitter_state::EmitterState;
use namtor_camera::hotplug::{DeviceEvent, HotplugState};
use namtor_camera::inventory::{
    select_camera_from_inventory, CameraInventory, DiscoveredCamera, SysfsInventory,
};
use namtor_camera::resume::{ReconcileOutcome, Reconciler, SleepEvent};
use namtor_camera::watchdog::force_off_lib;
use namtor_capture::source::CaptureSource;
use namtor_hwdb::quirks::EmitterCfg;

/// Keeps "another program has the camera" typed on its way out of the
/// capture layer. Everything else flattens to [`DaemonError::Capture`].
fn capture_err(e: namtor_capture::source::CaptureError) -> crate::DaemonError {
    match e {
        namtor_capture::source::CaptureError::Busy(detail) => {
            crate::DaemonError::CameraBusy(detail)
        }
        other => crate::DaemonError::Capture(other.to_string()),
    }
}

pub fn factory_with_configured(configured: Option<String>) -> CaptureFactory {
    factory_with_inventory(Box::new(SysfsInventory::default()), configured, None)
}

pub fn factory_with_inventory(
    inventory: Box<dyn CameraInventory + Send>,
    configured: Option<String>,
    enrolled: Option<String>,
) -> CaptureFactory {
    factory_with_inventory_at(
        inventory,
        configured,
        enrolled,
        PathBuf::from(namtor_camera::emitter_state::DEFAULT_PATH),
    )
}

/// As [`factory_with_inventory`], with the emitter-state record written to an
/// explicit path.
///
/// The path is a parameter rather than an environment variable on purpose.
/// An env override would let anything able to set the daemon's environment
/// redirect the record, so `ExecStopPost=/usr/bin/namtor emitter-off` would
/// find nothing at the default path, take its "nothing was armed" branch and
/// exit 0 — silently disarming the SIGKILL backstop. That is the same threat
/// model that removed `NAMTOR_GROUP_FILE` from this daemon, and it gets the
/// same answer.
pub fn factory_with_inventory_at(
    inventory: Box<dyn CameraInventory + Send>,
    configured: Option<String>,
    enrolled: Option<String>,
    state_path: PathBuf,
) -> CaptureFactory {
    Box::new(move || {
        let cameras = inventory.enumerate().map_err(crate::DaemonError::Capture)?;
        let selected =
            select_camera_from_inventory(&cameras, configured.as_deref(), enrolled.as_deref())
                .ok_or_else(|| {
                    crate::DaemonError::Capture("no IR camera found via topology selection".into())
                })?;
        open_selected_camera(&selected.camera, &state_path)
    })
}

fn open_selected_camera(
    cam: &DiscoveredCamera,
    state_path: &Path,
) -> Result<Box<dyn CaptureSource>, crate::DaemonError> {
    let dev = &cam.ir_node.dev_path;
    let meta_dev = cam.metadata_node.as_ref().map(|n| &n.dev_path);

    let emitter_cfg = cam.quirks_entry.as_ref().map(|e| &e.emitter);
    let emitter = activate_emitter(dev, emitter_cfg, state_path).unwrap_or_else(|e| {
        tracing::warn!(error = %e, dev = %dev.display(), "emitter activation failed");
        EmitterGuard::failed(emitter_cfg.cloned())
    });
    tracing::info!(protection = ?emitter.protection_level(), "emitter ready");

    let streamer = namtor_capture::ir_uvc::V4l2IrStream::open(
        dev,
        meta_dev.map(|p| p.as_path()),
        cam.width,
        cam.height,
    )
    .map_err(capture_err)?;
    let caps = namtor_capture::source::SourceCaps {
        spectrum: namtor_capture::source::Spectrum::Ir,
        format: namtor_capture::frame::PixFmt::Grey8,
        width: cam.width,
        height: cam.height,
        fps: cam.fps,
        has_metadata: meta_dev.is_some(),
    };
    let mut source =
        namtor_capture::ir_uvc::IrUvc::new(streamer, namtor_capture::ir_uvc::ParityPolicy, caps);
    source.start().map_err(capture_err)?;
    Ok(Box::new(EmitterSource {
        source,
        _emitter: emitter,
    }) as Box<dyn CaptureSource>)
}

struct EmitterGuard {
    xu: Option<Box<namtor_uvc::probe::SafeXu<namtor_uvc::device::V4l2Device>>>,
    cfg: Option<EmitterCfg>,
    outcome: ApplyOutcome,
    armed: bool,
    armed_at: Option<Instant>,
    max_on_ms: u32,
}

impl EmitterGuard {
    fn failed(cfg: Option<EmitterCfg>) -> Self {
        Self {
            xu: None,
            cfg,
            outcome: ApplyOutcome::SkippedAlwaysOn,
            armed: false,
            armed_at: None,
            max_on_ms: 2000,
        }
    }

    fn protection_level(&self) -> ProtectionLevel {
        match self.outcome {
            ApplyOutcome::Activated => ProtectionLevel::Full,
            ApplyOutcome::SkippedVendorXuNoConsent | ApplyOutcome::SkippedAlwaysOn => {
                ProtectionLevel::Reduced
            }
        }
    }

    /// Per-frame watchdog enforcement (spec §4.10.2). If the emitter has been
    /// on longer than `max_on_ms`, force it off and disarm. Called before
    /// each frame pull so a stuck capture loop cannot leave the emitter on.
    fn check_watchdog_and_force_off(&mut self) {
        if !self.armed {
            return;
        }
        let Some(t0) = self.armed_at else { return };
        if t0.elapsed() > Duration::from_millis(self.max_on_ms as u64) {
            if let (Some(xu), Some(cfg)) = (self.xu.as_mut(), self.cfg.as_ref()) {
                let _ = force_off_lib(xu.as_mut(), cfg);
            }
            self.armed = false;
            self.armed_at = None;
        }
    }
}

impl EmitterGuard {
    /// Darken the emitter without giving up the device, so a later
    /// [`Self::rearm`] can turn it back on.
    ///
    /// Needed because a stopped capture is not always a dropped one. An
    /// enrollment session keeps its source across Stop → Review → Resume, and
    /// `EmitterSource::stop` used to stop only the stream. The per-frame
    /// watchdog cannot help — it runs before a frame pull, and a stopped
    /// source pulls none — so the IR emitter stayed lit for as long as the
    /// user sat on the Review screen. Measured on the reference camera:
    /// still `payload_on` 1500 ms after `stop()`.
    fn disarm(&mut self) {
        if !self.armed {
            return;
        }
        if let (Some(xu), Some(cfg)) = (self.xu.as_mut(), self.cfg.as_ref()) {
            if let Err(e) = force_off_lib(xu.as_mut(), cfg) {
                // The write failed, so the emitter is still lit. Do NOT clear
                // `armed`: that is the flag `check_watchdog_and_force_off`
                // tests first, so clearing it here would switch the watchdog
                // off for the rest of this guard's life and leave `Drop` as
                // the only thing that could ever darken the LED — and the
                // Stop -> Review -> Resume flow deliberately does not drop.
                // Staying armed keeps the watchdog trying.
                tracing::warn!(error = %e, "emitter force-off failed; leaving the watchdog armed");
                return;
            }
        }
        self.armed = false;
        self.armed_at = None;
    }

    /// Turn the emitter back on after [`Self::disarm`], if it was on before.
    ///
    /// Only re-applies when the original activation succeeded; an emitter that
    /// was skipped (always-on hardware, or a vendor XU without consent) stays
    /// as it was. The watchdog budget restarts from now.
    fn rearm(&mut self) {
        if self.armed || self.outcome != ApplyOutcome::Activated {
            return;
        }
        let (Some(xu), Some(cfg)) = (self.xu.as_mut(), self.cfg.as_ref()) else {
            return;
        };
        let mut ctrl = EmitterController::new(xu.as_mut(), cfg);
        match ctrl.apply_on_gated(&EnvConsent) {
            Ok(ApplyOutcome::Activated) => {
                ctrl.release_to_guard();
                self.armed = true;
                self.armed_at = Some(Instant::now());
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "emitter re-arm on resume failed"),
        }
    }
}

impl Drop for EmitterGuard {
    fn drop(&mut self) {
        if let (Some(mut xu), Some(cfg)) = (self.xu.take(), self.cfg.as_ref()) {
            let _ = force_off_lib(xu.as_mut(), cfg);
        }
    }
}

/// Write the `namtor emitter-off` record for `dev`.
///
/// The device's USB identity is recovered by walking sysfs for the USB device
/// that owns this node, so the backstop can re-resolve the node after a
/// renumber instead of trusting a `/dev/videoN` path that the kernel is free
/// to reassign on the next boot.
fn record_emitter_state(dev: &Path, cfg: &EmitterCfg, state_path: &Path) -> Result<(), String> {
    let unit = cfg.unit.ok_or("emitter config has no unit")?;
    let selector = cfg.selector.ok_or("emitter config has no selector")?;
    if cfg.payload_off.is_empty() {
        return Err("emitter config has an empty payload_off".into());
    }

    let sysfs_root = std::env::var("NAMTOR_SYSFS_ROOT").unwrap_or_else(|_| "/sys".to_string());
    let (vid, pid, interface) = usb_identity_of(Path::new(&sysfs_root), dev)
        .ok_or_else(|| format!("no USB device in sysfs owns {}", dev.display()))?;

    let state = EmitterState {
        vid,
        pid,
        interface,
        device: dev.to_path_buf(),
        unit,
        selector,
        payload_off: cfg.payload_off.clone(),
    };
    state.write(state_path)
}

/// Find the USB vendor/product and UVC interface number that own `node`.
fn usb_identity_of(sysfs_root: &Path, node: &Path) -> Option<(u16, u16, u8)> {
    let devices = namtor_hwdb::sysfs::enumerate_usb(sysfs_root).ok()?;
    for usb in &devices {
        let Ok(nodes) = namtor_hwdb::nodes::correlate_nodes(usb) else {
            continue;
        };
        if let Some(n) = nodes.iter().find(|n| n.dev_path == node) {
            return Some((usb.vid, usb.pid, n.interface));
        }
    }
    None
}

fn activate_emitter(
    dev: &Path,
    emitter_cfg: Option<&EmitterCfg>,
    state_path: &Path,
) -> Result<EmitterGuard, String> {
    use namtor_uvc::device::V4l2Device;
    use namtor_uvc::probe::SafeXu;

    let cfg = emitter_cfg.ok_or_else(|| "no emitter config for selected device".to_string())?;

    // Record how to darken this emitter BEFORE turning it on, so the record
    // exists even if we die between here and the first frame. `ExecStopPost`
    // reads it after SIGKILL, when nothing can be asked of this process. A
    // failure to write is logged, not fatal: refusing to authenticate because
    // a backstop file could not be written would be the worse trade.
    if let Err(e) = record_emitter_state(dev, cfg, state_path) {
        tracing::warn!(
            error = %e,
            "could not record emitter state; the ExecStopPost backstop will have nothing to read"
        );
    }

    let v4l2_dev = V4l2Device::open(dev).map_err(|e| format!("open {}: {e}", dev.display()))?;
    let mut xu = Box::new(SafeXu::new(v4l2_dev));
    let max_on_ms = cfg.lifecycle.as_ref().map(|l| l.max_on_ms).unwrap_or(2000);

    let outcome = {
        let mut ctrl = EmitterController::new(&mut xu, cfg);
        let r = ctrl.apply_on_gated(&EnvConsent);
        match r {
            Ok(o) => {
                if o == ApplyOutcome::Activated {
                    ctrl.release_to_guard();
                }
                o
            }
            Err(e) => return Err(format!("emitter gated apply failed: {e}")),
        }
    };

    let armed = outcome == ApplyOutcome::Activated;
    let armed_at = if armed { Some(Instant::now()) } else { None };
    Ok(EmitterGuard {
        xu: Some(xu),
        cfg: emitter_cfg.cloned(),
        outcome,
        armed,
        armed_at,
        max_on_ms,
    })
}

struct EmitterSource<S: namtor_capture::ir_uvc::Streamer, P: namtor_capture::ir_uvc::EmitterPolicy>
{
    source: namtor_capture::ir_uvc::IrUvc<S, P>,
    _emitter: EmitterGuard,
}

impl<S: namtor_capture::ir_uvc::Streamer, P: namtor_capture::ir_uvc::EmitterPolicy> CaptureSource
    for EmitterSource<S, P>
{
    fn caps(&self) -> namtor_capture::source::SourceCaps {
        self.source.caps()
    }

    fn start(&mut self) -> Result<(), namtor_capture::source::CaptureError> {
        // Stream first, emitter second. `source.start()` can fail (REQBUFS,
        // mmap, STREAMON); arming before it meant a failed start returned Err
        // with the emitter lit and no stream, so `next_frame` — the only thing
        // that runs the watchdog — would never be called.
        self.source.start()?;
        // Resume after a Stop: `stop` darkened the emitter, so light it again
        // now that frames are actually coming. A no-op on first start.
        self._emitter.rearm();
        Ok(())
    }

    fn next_frame(
        &mut self,
    ) -> Result<Option<namtor_capture::frame::Frame>, namtor_capture::source::CaptureError> {
        self._emitter.check_watchdog_and_force_off();
        self.source.next_frame()
    }

    fn stop(&mut self) {
        self.source.stop();
        // A stop must darken the emitter, not only halt the stream: the source
        // may be kept for a later Resume. See `EmitterGuard::disarm`.
        self._emitter.disarm();
    }
}

/// Opens a discovered camera into a capture source. Production uses
/// [`open_selected_camera`]; tests inject a stub.
type OpenFn = Box<dyn FnMut(&DiscoveredCamera) -> Result<Box<dyn CaptureSource>, String> + Send>;

/// Wires hotplug + suspend/resume reconciliation into the capture lifecycle.
///
/// On device removal of the selected node, or on suspend, the open capture is
/// closed (which forces the emitter off via [`EmitterGuard`]'s `Drop`). On
/// resume or re-add, the inventory is re-enumerated; if the camera renumbered
/// the stale path is invalidated and the new path is selected. A bounded
/// pending-request queue (via [`Reconciler`]) holds auth requests during
/// reconciliation and drops them if the queue overflows.
//
// allow(dead_code): NOT wired into the daemon, and not going to be as-is.
//
// This comment used to call wiring "a separate integration step", which read
// as though suspend and hotplug were handled and merely pending. They were
// not handled at all. The daemon now covers them with less machinery:
//
// - Hotplug and renumbering: both capture factories (`factory_with_*`)
//   re-enumerate sysfs on every open, and the auth capture is opened per PAM
//   request, so the next auth after a replug or a resume finds the camera
//   fresh without a long-lived reconciler.
// - Suspend: `app.rs` subscribes to logind's `PrepareForSleep` under a delay
//   inhibitor and calls `DaemonDispatcher::release_for_suspend` and
//   `Manager3Service::cancel_all_sessions`, which close every capture so the
//   IR emitter is dark before sleep.
//
// What this guard adds beyond that is draining auth requests queued across a
// resume, which the per-request model does not need because PAM clients do not
// queue. It is kept, with its tests, as the design reference for that
// behaviour; if a long-lived capture is ever introduced, this is where to
// start.
#[allow(dead_code)]
pub struct ReconcileGuard {
    hotplug: HotplugState,
    reconciler: Reconciler<fn()>,
    inventory: Box<dyn CameraInventory + Send>,
    configured: Option<String>,
    enrolled: Option<String>,
    open_fn: OpenFn,
    capture: Option<Box<dyn CaptureSource>>,
}

#[allow(dead_code)]
impl ReconcileGuard {
    pub fn new(
        inventory: Box<dyn CameraInventory + Send>,
        configured: Option<String>,
        enrolled: Option<String>,
        open_fn: OpenFn,
    ) -> Self {
        Self {
            hotplug: HotplugState::new(),
            reconciler: Reconciler::new(noop),
            inventory,
            configured,
            enrolled,
            open_fn,
            capture: None,
        }
    }

    pub fn capture_ready(&self) -> bool {
        self.hotplug.capture_ready() && self.capture.is_some()
    }

    pub fn last_outcome(&self) -> Option<&ReconcileOutcome> {
        self.reconciler.last_outcome()
    }

    #[allow(dead_code)]
    pub fn pending_count(&self) -> usize {
        self.reconciler.pending_count()
    }

    #[allow(dead_code)]
    pub fn capture_mut(&mut self) -> Option<&mut Box<dyn CaptureSource>> {
        self.capture.as_mut()
    }

    /// Closes the open capture (if any), which forces the emitter off via
    /// [`EmitterGuard`]'s `Drop`. Used on removal, suspend, and before a
    /// re-open on a renumbered path.
    pub fn close_capture(&mut self) {
        if let Some(mut src) = self.capture.take() {
            src.stop();
        }
    }

    /// Applies a hotplug device event. If the selected node was removed, the
    /// open capture is closed so the emitter is forced off.
    pub fn handle_device_event(&mut self, event: DeviceEvent) {
        let was_ready = self.hotplug.capture_ready();
        self.hotplug.apply(event);
        if was_ready && !self.hotplug.capture_ready() {
            self.close_capture();
        }
    }

    /// Applies a sleep event. On suspend the capture is closed (emitter off).
    /// On resume the inventory is re-enumerated, the camera re-selected
    /// (invalidating a stale path if renumbered), and — if found — re-opened;
    /// queued auth requests are drained for the caller to service. If no
    /// camera is found post-resume, `MissingCamera` is recorded and the
    /// caller must fall back to password.
    pub fn handle_sleep_event(&mut self, event: SleepEvent) {
        match event {
            SleepEvent::Suspend => {
                self.reconciler.handle(SleepEvent::Suspend);
                self.hotplug.handle_sleep(SleepEvent::Suspend);
                self.close_capture();
            }
            SleepEvent::Resume => {
                self.reconciler.handle(SleepEvent::Resume);
                self.hotplug.handle_sleep(SleepEvent::Resume);
                self.reopen_after_resume();
            }
        }
    }

    fn reopen_after_resume(&mut self) {
        let old_path = self.hotplug.selected_path().map(|p| p.to_path_buf());
        let discovered = self.rediscover();
        match discovered {
            Some((cam, path)) => {
                let outcome = match &old_path {
                    Some(old) if *old != path => ReconcileOutcome::StaleNode {
                        old: old.clone(),
                        new: path.clone(),
                    },
                    _ => ReconcileOutcome::Rediscovered { path: path.clone() },
                };
                self.reconciler.record_rediscovery(outcome);
                self.close_capture();
                match (self.open_fn)(&cam) {
                    Ok(src) => {
                        self.hotplug.apply(DeviceEvent::Add(cam.ir_node.clone()));
                        self.hotplug.set_selected(path);
                        self.hotplug.mark_reopened();
                        self.capture = Some(src);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "post-resume re-open failed");
                        self.reconciler
                            .record_rediscovery(ReconcileOutcome::MissingCamera);
                    }
                }
                let _ = self.reconciler.drain_ready();
            }
            None => {
                self.reconciler
                    .record_rediscovery(ReconcileOutcome::MissingCamera);
                let _ = self.reconciler.drain_ready();
            }
        }
    }

    fn rediscover(&self) -> Option<(DiscoveredCamera, PathBuf)> {
        let cameras = self.inventory.enumerate().ok()?;
        let sel = select_camera_from_inventory(
            &cameras,
            self.configured.as_deref(),
            self.enrolled.as_deref(),
        )?;
        Some((sel.camera.clone(), sel.camera.ir_node.dev_path.clone()))
    }

    /// Ensures a capture is open and ready. If the stream was invalidated
    /// (removal/suspend), re-discovers and re-opens. Returns an error if no
    /// camera can be found so the caller can fall back to password.
    pub fn prepare_capture(&mut self) -> Result<(), DaemonError> {
        if self.capture_ready() {
            return Ok(());
        }
        if self.capture.is_some() {
            self.close_capture();
        }
        let (cam, path) = self
            .rediscover()
            .ok_or_else(|| DaemonError::Capture("no IR camera found during reconcile".into()))?;
        let src = (self.open_fn)(&cam).map_err(DaemonError::Capture)?;
        self.hotplug.apply(DeviceEvent::Add(cam.ir_node.clone()));
        self.hotplug.set_selected(path);
        self.hotplug.mark_reopened();
        self.capture = Some(src);
        Ok(())
    }
}

#[allow(dead_code)]
fn noop() {}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_camera::hotplug::{DeviceEventSource, FakeEventSource};
    use namtor_camera::inventory::FakeInventory;
    use namtor_capture::frame::{Frame, PixFmt};
    use namtor_capture::source::{CaptureError, SourceCaps, Spectrum};
    use namtor_hwdb::nodes::V4l2Node;
    use namtor_hwdb::sysfs::UsbDevice;

    type OpenFn =
        Box<dyn FnMut(&DiscoveredCamera) -> Result<Box<dyn CaptureSource>, String> + Send>;

    struct StubSource {
        stopped: bool,
    }

    impl CaptureSource for StubSource {
        fn caps(&self) -> SourceCaps {
            SourceCaps {
                spectrum: Spectrum::Ir,
                format: PixFmt::Grey8,
                width: 8,
                height: 8,
                fps: 30,
                has_metadata: false,
            }
        }
        fn start(&mut self) -> Result<(), CaptureError> {
            Ok(())
        }
        fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError> {
            Ok(None)
        }
        fn stop(&mut self) {
            self.stopped = true;
        }
    }

    fn mk_usb() -> UsbDevice {
        UsbDevice {
            sysfs_path: PathBuf::from("/sys/bus/usb/devices/test"),
            vid: 0x3277,
            pid: 0x0018,
            bcd_device: 0x0001,
            descriptors_path: PathBuf::from("/nonexistent"),
        }
    }

    fn mk_node(dev: &str, iface: u8, meta: bool) -> V4l2Node {
        V4l2Node {
            dev_path: PathBuf::from(dev),
            interface: iface,
            is_metadata: meta,
            name: format!("node {dev}"),
        }
    }

    fn mk_camera(dev: &str) -> DiscoveredCamera {
        DiscoveredCamera {
            ir_node: mk_node(dev, 3, false),
            metadata_node: Some(mk_node("/dev/video3", 3, true)),
            rgb_node: None,
            usb: mk_usb(),
            quirks_entry: None,
            width: 640,
            height: 360,
            fps: 30,
            is_internal: true,
        }
    }

    fn open_fn() -> OpenFn {
        Box::new(|_cam| Ok(Box::new(StubSource { stopped: false }) as Box<dyn CaptureSource>))
    }

    fn guard(cameras: Vec<DiscoveredCamera>) -> ReconcileGuard {
        ReconcileGuard::new(Box::new(FakeInventory::new(cameras)), None, None, open_fn())
    }

    #[test]
    fn prepare_capture_opens_from_inventory() {
        let mut g = guard(vec![mk_camera("/dev/video2")]);
        assert!(!g.capture_ready());
        g.prepare_capture().unwrap();
        assert!(g.capture_ready());
        assert_eq!(g.hotplug.selected_path(), Some(Path::new("/dev/video2")));
    }

    #[test]
    fn prepare_capture_errors_when_no_camera() {
        let mut g = guard(vec![]);
        let err = g.prepare_capture().unwrap_err();
        assert!(matches!(err, DaemonError::Capture(_)));
        assert!(!g.capture_ready());
    }

    #[test]
    fn remove_of_selected_closes_capture() {
        let mut g = guard(vec![mk_camera("/dev/video2")]);
        g.prepare_capture().unwrap();
        assert!(g.capture_ready());
        g.handle_device_event(DeviceEvent::Remove(mk_node("/dev/video2", 3, false)));
        assert!(!g.capture_ready());
        assert!(g.capture.is_none(), "capture dropped on removal");
        assert!(g.hotplug.reopen_needed());
    }

    #[test]
    fn remove_of_any_node_invalidates_conservatively() {
        let mut g = guard(vec![mk_camera("/dev/video2")]);
        g.prepare_capture().unwrap();
        assert!(g.capture_ready());
        g.handle_device_event(DeviceEvent::Remove(mk_node("/dev/video9", 0, false)));
        assert!(
            !g.capture_ready(),
            "any remove invalidates — the selected node cannot be distinguished from a sibling in udev ordering"
        );
        assert!(g.capture.is_none(), "capture closed on any remove");
        assert!(g.hotplug.reopen_needed());
    }

    #[test]
    fn fake_event_remove_add_remove_add_cycles() {
        let mut g = guard(vec![mk_camera("/dev/video2")]);
        g.prepare_capture().unwrap();
        let mut src = FakeEventSource::new(vec![
            DeviceEvent::Remove(mk_node("/dev/video2", 3, false)),
            DeviceEvent::Add(mk_node("/dev/video2", 3, false)),
            DeviceEvent::Remove(mk_node("/dev/video2", 3, false)),
            DeviceEvent::Add(mk_node("/dev/video2", 3, false)),
        ]);
        while let Some(ev) = src.next_event() {
            g.handle_device_event(ev);
        }
        assert!(
            g.hotplug.reopen_needed(),
            "Add does not clear reopen_needed; only mark_reopened does"
        );
        assert!(!g.capture_ready());
        g.prepare_capture().unwrap();
        assert!(g.capture_ready());
        assert!(!g.hotplug.reopen_needed());
    }

    #[test]
    fn suspend_closes_capture_and_resume_reopens_same_path() {
        let mut g = guard(vec![mk_camera("/dev/video2")]);
        g.prepare_capture().unwrap();
        g.handle_sleep_event(SleepEvent::Suspend);
        assert!(!g.capture_ready());
        assert!(g.capture.is_none(), "suspend closed capture");
        g.handle_sleep_event(SleepEvent::Resume);
        assert!(g.capture_ready(), "resume re-opened");
        assert_eq!(
            g.last_outcome(),
            Some(&ReconcileOutcome::Rediscovered {
                path: "/dev/video2".into()
            })
        );
    }

    #[test]
    fn suspend_then_resume_renumber_records_stale_node() {
        let pre = vec![mk_camera("/dev/video2")];
        let post = vec![mk_camera("/dev/video4")];
        let inv = SwitchingInventory::new(pre, post);
        let mut g = ReconcileGuard::new(Box::new(inv), None, None, open_fn());
        g.prepare_capture().unwrap();
        assert_eq!(g.hotplug.selected_path(), Some(Path::new("/dev/video2")));
        g.handle_sleep_event(SleepEvent::Suspend);
        g.handle_sleep_event(SleepEvent::Resume);
        assert!(g.capture_ready(), "re-opened on renumbered path");
        assert_eq!(
            g.last_outcome(),
            Some(&ReconcileOutcome::StaleNode {
                old: "/dev/video2".into(),
                new: "/dev/video4".into()
            })
        );
        assert_eq!(g.hotplug.selected_path(), Some(Path::new("/dev/video4")));
    }

    #[test]
    fn resume_with_missing_camera_records_missing_and_falls_back() {
        let pre = vec![mk_camera("/dev/video2")];
        let inv = SwitchingInventory::new(pre, vec![]);
        let mut g = ReconcileGuard::new(Box::new(inv), None, None, open_fn());
        g.prepare_capture().unwrap();
        g.handle_sleep_event(SleepEvent::Suspend);
        g.handle_sleep_event(SleepEvent::Resume);
        assert!(!g.capture_ready(), "no camera after resume");
        assert!(g.capture.is_none());
        assert_eq!(g.last_outcome(), Some(&ReconcileOutcome::MissingCamera));
        let err = g.prepare_capture().unwrap_err();
        assert!(matches!(err, DaemonError::Capture(_)));
    }

    /// Inventory that returns `pre` for the first `enumerate` call and `post`
    /// for every call after — simulates a renumber across suspend/resume.
    struct SwitchingInventory {
        pre: Vec<DiscoveredCamera>,
        post: Vec<DiscoveredCamera>,
        first_taken: std::cell::Cell<bool>,
    }

    impl SwitchingInventory {
        fn new(pre: Vec<DiscoveredCamera>, post: Vec<DiscoveredCamera>) -> Self {
            Self {
                pre,
                post,
                first_taken: std::cell::Cell::new(false),
            }
        }
    }

    impl CameraInventory for SwitchingInventory {
        fn enumerate(&self) -> Result<Vec<DiscoveredCamera>, String> {
            if self.first_taken.get() {
                Ok(self.post.clone())
            } else {
                self.first_taken.set(true);
                Ok(self.pre.clone())
            }
        }
    }
}
