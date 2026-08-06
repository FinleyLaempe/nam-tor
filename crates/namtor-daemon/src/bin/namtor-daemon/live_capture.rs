// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;

use namtor_camera::inventory::{
    select_camera_from_inventory, CameraInventory, DiscoveredCamera, SysfsInventory,
};
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
        EmitterGuard {
            xu: None,
            cfg: emitter_cfg.cloned(),
        }
    });

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
}

impl Drop for EmitterGuard {
    fn drop(&mut self) {
        if let (Some(mut xu), Some(cfg)) = (self.xu.take(), self.cfg.as_ref()) {
            let unit = cfg.unit.unwrap_or(0);
            let selector = cfg.selector.unwrap_or(0);
            let _ = xu.try_write(unit, selector, &cfg.payload_off);
        }
    }
}

fn activate_emitter(dev: &Path, emitter_cfg: Option<&EmitterCfg>) -> Result<EmitterGuard, String> {
    use namtor_uvc::device::V4l2Device;
    use namtor_uvc::probe::SafeXu;

    let cfg = emitter_cfg.ok_or_else(|| "no emitter config for selected device".to_string())?;
    let v4l2_dev = V4l2Device::open(dev).map_err(|e| format!("open {}: {e}", dev.display()))?;
    let mut xu = Box::new(SafeXu::new(v4l2_dev));
    let unit = cfg.unit.ok_or("emitter cfg missing unit")?;
    let selector = cfg.selector.ok_or("emitter cfg missing selector")?;
    xu.probe_control(unit, selector)
        .map_err(|e| format!("emitter probe failed: {e}"))?;
    let restorer = xu
        .try_write(unit, selector, &cfg.payload_on)
        .map_err(|e| format!("emitter write failed: {e}"))?;
    restorer.keep();
    Ok(EmitterGuard {
        xu: Some(xu),
        cfg: emitter_cfg.cloned(),
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
        self.source.next_frame()
    }

    fn stop(&mut self) {
        self.source.stop();
    }
}
