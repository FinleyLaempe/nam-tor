// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use namtor_capture::source::CaptureSource;
use namtor_daemon::daemon::CaptureFactory;

pub fn factory() -> CaptureFactory {
    Box::new(|| {
        let dev = Path::new("/dev/video2");
        let meta_dev = Path::new("/dev/video3");
        let emitter = activate_emitter(dev).unwrap_or_else(|e| {
            eprintln!("namtor-daemon: warning — emitter activation failed: {e}");
            EmitterGuard { xu: None }
        });
        let streamer = namtor_capture::ir_uvc::V4l2IrStream::open(
            dev,
            meta_dev.exists().then_some(meta_dev),
            640,
            360,
        )
        .map_err(|e| namtor_daemon::DaemonError::Capture(e.to_string()))?;
        let caps = namtor_capture::source::SourceCaps {
            spectrum: namtor_capture::source::Spectrum::Ir,
            format: namtor_capture::frame::PixFmt::Grey8,
            width: 640,
            height: 360,
            fps: 30,
            has_metadata: meta_dev.exists(),
        };
        let mut source = namtor_capture::ir_uvc::IrUvc::new(
            streamer,
            namtor_capture::ir_uvc::ParityPolicy,
            caps,
        );
        source
            .start()
            .map_err(|e| namtor_daemon::DaemonError::Capture(e.to_string()))?;
        Ok(Box::new(EmitterSource {
            source,
            _emitter: emitter,
        }) as Box<dyn CaptureSource>)
    })
}

struct EmitterGuard {
    xu: Option<Box<namtor_uvc::probe::SafeXu<namtor_uvc::device::V4l2Device>>>,
}

impl Drop for EmitterGuard {
    fn drop(&mut self) {
        if let (Some(mut xu), Some(cfg)) = (self.xu.take(), emitter_cfg()) {
            let unit = cfg.unit.unwrap_or(0);
            let selector = cfg.selector.unwrap_or(0);
            let _ = xu.try_write(unit, selector, &cfg.payload_off);
        }
    }
}

fn emitter_cfg() -> Option<namtor_hwdb::quirks::EmitterCfg> {
    let candidates = [
        PathBuf::from("/usr/share/namtor/quirks/devices"),
        PathBuf::from("quirks/devices"),
    ];
    candidates.iter().find_map(|dir| {
        namtor_hwdb::quirks::load_quirks_dir(dir)
            .ok()
            .and_then(|db| {
                db.entries
                    .get("3277:0018")
                    .map(|entry| entry.emitter.clone())
            })
    })
}

fn activate_emitter(dev: &Path) -> Result<EmitterGuard, String> {
    use namtor_uvc::device::V4l2Device;
    use namtor_uvc::probe::SafeXu;

    let cfg = emitter_cfg().ok_or_else(|| "no quirk entry for 3277:0018".to_string())?;
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
    Ok(EmitterGuard { xu: Some(xu) })
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
