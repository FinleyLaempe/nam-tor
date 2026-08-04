// SPDX-License-Identifier: GPL-3.0-only
//! `CaptureSession`: RAII container for a paired IR + optional RGB capture
//! source. Drop stops both streams.
//!
//! The classify-and-open factory (`open(usb, db)`) lives in `namtor-camera`,
//! not here: `namtor-capture` is the base layer and cannot depend on
//! `namtor-hwdb` (cycle: hwdb → uvc → capture). This module owns only the
//! session lifecycle.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::source::{CaptureError, CaptureSource};

/// A paired IR + optional RGB capture session. RAII: `Drop` stops both
/// streams. Emitter-off is `namtor-camera`'s job, not here.
pub struct CaptureSession {
    ir: Box<dyn CaptureSource>,
    rgb: Option<Box<dyn CaptureSource>>,
}

impl CaptureSession {
    pub fn new(ir: Box<dyn CaptureSource>, rgb: Option<Box<dyn CaptureSource>>) -> Self {
        Self { ir, rgb }
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.ir.stop();
        if let Some(rgb) = self.rgb.as_mut() {
            rgb.stop();
        }
    }
}

/// Measures the fixed cost of opening a V4L2 stream: open → negotiate →
/// request buffers → mmap → streamon → dequeue one frame → streamoff.
/// This is the in-process re-measurement of risk R11 (spec §5.5.1).
#[cfg(target_os = "linux")]
pub fn measure_open_cost(node: &Path) -> Result<Duration, CaptureError> {
    use crate::v4l2::{V4l2Stream, V4L2_PIX_FMT_GREY};

    let started = Instant::now();
    let mut stream = V4l2Stream::open(node).map_err(|e| CaptureError::Device(e.to_string()))?;
    stream
        .negotiate_format(640, 360, V4L2_PIX_FMT_GREY)
        .map_err(|e| CaptureError::Device(e.to_string()))?;
    let n = stream
        .request_buffers(4)
        .map_err(|e| CaptureError::Device(e.to_string()))?;
    stream
        .mmap_buffers()
        .map_err(|e| CaptureError::Device(e.to_string()))?;
    for i in 0..n {
        stream
            .requeue(i as usize)
            .map_err(|e| CaptureError::Device(e.to_string()))?;
    }
    stream
        .streamon()
        .map_err(|e| CaptureError::Device(e.to_string()))?;
    let _ = stream
        .dequeue()
        .map_err(|e| CaptureError::Device(e.to_string()))?;
    stream
        .streamoff()
        .map_err(|e| CaptureError::Device(e.to_string()))?;
    Ok(started.elapsed())
}

/// Measures elapsed time around a closure. Exposed so the R11 measurement
/// logic is testable without a device.
pub fn measure_open_cost_with(f: impl FnOnce()) -> Duration {
    let started = Instant::now();
    f();
    started.elapsed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Frame, PixFmt};
    use crate::source::{CaptureError, CaptureSource, SourceCaps, Spectrum};
    use std::cell::RefCell;
    use std::time::Duration;

    #[test]
    fn drop_stops_both_streams_via_rc_flag() {
        use std::rc::Rc;

        struct RcMock {
            stopped: Rc<RefCell<bool>>,
        }
        impl CaptureSource for RcMock {
            fn caps(&self) -> SourceCaps {
                SourceCaps {
                    spectrum: Spectrum::Ir,
                    format: PixFmt::Grey8,
                    width: 64,
                    height: 64,
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
                *self.stopped.borrow_mut() = true;
            }
        }

        let ir_flag = Rc::new(RefCell::new(false));
        let rgb_flag = Rc::new(RefCell::new(false));

        {
            let ir = RcMock {
                stopped: Rc::clone(&ir_flag),
            };
            let rgb = RcMock {
                stopped: Rc::clone(&rgb_flag),
            };
            let _session = CaptureSession::new(Box::new(ir), Some(Box::new(rgb)));
        }

        assert!(*ir_flag.borrow(), "IR stream must be stopped on Drop");
        assert!(*rgb_flag.borrow(), "RGB stream must be stopped on Drop");
    }

    #[test]
    fn drop_stops_ir_only_when_rgb_is_none() {
        use std::rc::Rc;

        struct RcMock {
            stopped: Rc<RefCell<bool>>,
        }
        impl CaptureSource for RcMock {
            fn caps(&self) -> SourceCaps {
                SourceCaps {
                    spectrum: Spectrum::Ir,
                    format: PixFmt::Grey8,
                    width: 64,
                    height: 64,
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
                *self.stopped.borrow_mut() = true;
            }
        }

        let ir_flag = Rc::new(RefCell::new(false));
        {
            let ir = RcMock {
                stopped: Rc::clone(&ir_flag),
            };
            let _session = CaptureSession::new(Box::new(ir), None);
        }
        assert!(*ir_flag.borrow(), "IR stream must be stopped on Drop");
    }

    #[test]
    fn measure_open_cost_with_returns_positive_duration() {
        let d = measure_open_cost_with(|| {
            std::thread::sleep(Duration::from_millis(2));
        });
        assert!(d > Duration::ZERO, "measured duration must be positive");
    }

    #[test]
    #[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
    fn measure_open_cost_on_reference_hw_prints_elapsed() {
        if std::env::var("NAMTOR_HW_TEST").is_err() {
            eprintln!("NAMTOR_HW_TEST not set; skipping");
            return;
        }
        #[cfg(target_os = "linux")]
        {
            let d = measure_open_cost(Path::new("/dev/video2"))
                .expect("measure_open_cost on /dev/video2");
            eprintln!("open cost: {:.1} ms", d.as_secs_f64() * 1000.0);
            assert!(d.as_millis() < 5000, "open cost sanity: {d:?}");
        }
    }
}
