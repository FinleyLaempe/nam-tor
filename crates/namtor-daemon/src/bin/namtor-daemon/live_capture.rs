// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;
use std::time::{Duration, Instant};

use namtor_camera::always_on::ProtectionLevel;
use namtor_camera::emitter_ctrl::{ApplyOutcome, EmitterController, EnvConsent};
use namtor_camera::inventory::{
    select_camera_from_inventory, CameraInventory, DiscoveredCamera, SysfsInventory,
};
use namtor_camera::watchdog::force_off_lib;
use namtor_capture::source::CaptureSource;
use namtor_daemon::daemon::CaptureFactory;
use namtor_hwdb::quirks::EmitterCfg;

pub fn factory_with_configured(configured: Option<String>) -> CaptureFactory {
    factory_with_inventory(Box::new(SysfsInventory::default()), configured, None)
}

pub fn factory_with_inventory(
    inventory: Box<dyn CameraInventory + Send>,
    configured: Option<String>,
    enrolled: Option<String>,
) -> CaptureFactory {
    Box::new(move || {
        let cameras = inventory
            .enumerate()
            .map_err(namtor_daemon::DaemonError::Capture)?;
        let selected =
            select_camera_from_inventory(&cameras, configured.as_deref(), enrolled.as_deref())
                .ok_or_else(|| {
                    namtor_daemon::DaemonError::Capture(
                        "no IR camera found via topology selection".into(),
                    )
                })?;
        open_selected_camera(&selected.camera)
    })
}

fn open_selected_camera(
    cam: &DiscoveredCamera,
) -> Result<Box<dyn CaptureSource>, namtor_daemon::DaemonError> {
    let dev = &cam.ir_node.dev_path;
    let meta_dev = cam.metadata_node.as_ref().map(|n| &n.dev_path);

    let emitter_cfg = cam.quirks_entry.as_ref().map(|e| &e.emitter);
    let emitter = activate_emitter(dev, emitter_cfg).unwrap_or_else(|e| {
        eprintln!("namtor-daemon: warning — emitter activation failed: {e}");
        EmitterGuard::failed(emitter_cfg.cloned())
    });
    eprintln!(
        "namtor-daemon: emitter protection level: {}",
        emitter.protection_level()
    );

    let streamer = namtor_capture::ir_uvc::V4l2IrStream::open(
        dev,
        meta_dev.map(|p| p.as_path()),
        cam.width,
        cam.height,
    )
    .map_err(|e| namtor_daemon::DaemonError::Capture(e.to_string()))?;
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
    source
        .start()
        .map_err(|e| namtor_daemon::DaemonError::Capture(e.to_string()))?;
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

impl Drop for EmitterGuard {
    fn drop(&mut self) {
        if let (Some(mut xu), Some(cfg)) = (self.xu.take(), self.cfg.as_ref()) {
            let _ = force_off_lib(xu.as_mut(), cfg);
        }
    }
}

fn activate_emitter(dev: &Path, emitter_cfg: Option<&EmitterCfg>) -> Result<EmitterGuard, String> {
    use namtor_uvc::device::V4l2Device;
    use namtor_uvc::probe::SafeXu;

    let cfg = emitter_cfg.ok_or_else(|| "no emitter config for selected device".to_string())?;
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
        self.source.start()
    }

    fn next_frame(
        &mut self,
    ) -> Result<Option<namtor_capture::frame::Frame>, namtor_capture::source::CaptureError> {
        self._emitter.check_watchdog_and_force_off();
        self.source.next_frame()
    }

    fn stop(&mut self) {
        self.source.stop();
    }
}
