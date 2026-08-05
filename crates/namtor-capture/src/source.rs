// SPDX-License-Identifier: GPL-3.0-only
//! The capture source abstraction.

use crate::frame::{Frame, PixFmt};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Which part of the spectrum a source observes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Spectrum {
    Ir,
    Rgb,
    Depth,
}

/// Static description of what a source produces.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SourceCaps {
    pub spectrum: Spectrum,
    pub format: PixFmt,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Whether a UVC metadata node is available for this source.
    pub has_metadata: bool,
}

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("capture source was not started")]
    NotStarted,
    #[error("device error: {0}")]
    Device(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed recording: {0}")]
    Recording(String),
}

/// A source of frames.
///
/// Pull-based: callers drive the stream with [`CaptureSource::next_frame`],
/// which returns `Ok(None)` once the stream is exhausted. Live device sources
/// block until the next frame arrives and never return `Ok(None)` until stopped.
pub trait CaptureSource: Send {
    fn caps(&self) -> SourceCaps;
    fn start(&mut self) -> Result<(), CaptureError>;
    fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError>;
    fn stop(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{EmitterState, Frame, PixFmt};
    use std::time::Duration;

    /// Minimal in-test implementation, proving the trait is implementable
    /// and that end-of-stream is expressible.
    struct TwoFrameSource {
        emitted: u32,
        started: bool,
    }

    impl CaptureSource for TwoFrameSource {
        fn caps(&self) -> SourceCaps {
            SourceCaps {
                spectrum: Spectrum::Ir,
                format: PixFmt::Grey8,
                width: 640,
                height: 360,
                fps: 30,
                has_metadata: true,
            }
        }

        fn start(&mut self) -> Result<(), CaptureError> {
            self.started = true;
            Ok(())
        }

        fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError> {
            if !self.started {
                return Err(CaptureError::NotStarted);
            }
            if self.emitted >= 2 {
                return Ok(None);
            }
            let f = Frame {
                data: vec![0u8; 230_400],
                format: PixFmt::Grey8,
                width: 640,
                height: 360,
                ts_mono: Duration::from_millis(33 * self.emitted as u64),
                sof: Some(self.emitted as u16),
                emitter: if self.emitted.is_multiple_of(2) {
                    EmitterState::On
                } else {
                    EmitterState::Off
                },
                quality: None,
            };
            self.emitted += 1;
            Ok(Some(f))
        }

        fn stop(&mut self) {
            self.started = false;
        }
    }

    #[test]
    fn source_reports_its_capabilities() {
        let s = TwoFrameSource {
            emitted: 0,
            started: false,
        };
        assert_eq!(s.caps().spectrum, Spectrum::Ir);
        assert_eq!(s.caps().width, 640);
    }

    #[test]
    fn next_frame_before_start_is_an_error() {
        let mut s = TwoFrameSource {
            emitted: 0,
            started: false,
        };
        assert!(matches!(s.next_frame(), Err(CaptureError::NotStarted)));
    }

    #[test]
    fn stream_terminates_with_ok_none() {
        let mut s = TwoFrameSource {
            emitted: 0,
            started: false,
        };
        s.start().unwrap();
        assert!(s.next_frame().unwrap().is_some());
        assert!(s.next_frame().unwrap().is_some());
        assert!(s.next_frame().unwrap().is_none());
    }
}
