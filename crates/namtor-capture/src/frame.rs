// SPDX-License-Identifier: GPL-3.0-only
//! Frame representation and pixel format geometry.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

/// Pixel formats Nam-tor can carry through the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixFmt {
    /// 8-bit greyscale. What `uvcvideo` reports for IR sensors.
    Grey8,
    /// YUYV 4:2:2, 16 bits per pixel.
    Yuyv,
    /// Motion-JPEG. Compressed, variable length.
    Mjpeg,
    /// 16-bit depth. Realsense-class hardware only.
    Depth16,
}

impl PixFmt {
    /// Bits per pixel, or `None` for compressed formats.
    pub fn bits_per_pixel(self) -> Option<u32> {
        match self {
            PixFmt::Grey8 => Some(8),
            PixFmt::Yuyv => Some(16),
            PixFmt::Depth16 => Some(16),
            PixFmt::Mjpeg => None,
        }
    }

    /// Expected buffer length for the given geometry, or `None` if variable.
    pub fn expected_len(self, width: u32, height: u32) -> Option<usize> {
        self.bits_per_pixel()
            .map(|bpp| (width as usize * height as usize * bpp as usize) / 8)
    }
}

/// State of the IR emitter when a frame was exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmitterState {
    /// Emitter was on for the whole exposure.
    On,
    /// Emitter was off for the whole exposure.
    Off,
    /// The emitter toggled mid-exposure. Unusable; must be discarded.
    Torn,
    /// Emitter state could not be determined.
    Unknown,
}

/// Per-frame quality signals used for enrollment filtering and fusion weighting.
///
/// Defined here so the [`Frame`] shape is stable, but **not populated during
/// M0** — `Frame::quality` is `None` throughout this milestone. M4 computes it
/// during enrollment (spec §6.6 Tier 1), and M6 uses it to weight multi-frame
/// fusion. Do not add computation for it here; there is nothing to consume it
/// yet, and the metric definitions depend on M1's measurements.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrameQuality {
    /// Variance of the Laplacian. Higher is sharper.
    pub blur: f32,
    /// Mean luminance, normalised to 0.0..=1.0.
    pub mean_luma: f32,
    /// Fraction of pixels at full scale.
    pub saturated_frac: f32,
}

/// A single captured frame.
#[derive(Debug, Clone)]
pub struct Frame {
    pub data: Vec<u8>,
    pub format: PixFmt,
    pub width: u32,
    pub height: u32,
    /// Elapsed time since the capture session started.
    pub ts_mono: Duration,
    /// USB start-of-frame counter, from UVC payload header metadata.
    pub sof: Option<u16>,
    pub emitter: EmitterState,
    pub quality: Option<FrameQuality>,
}

#[derive(Debug, Error, PartialEq)]
pub enum FrameError {
    #[error("frame buffer length mismatch: expected {expected}, got {actual}")]
    LengthMismatch { expected: usize, actual: usize },
}

impl Frame {
    /// Checks the buffer length against the declared geometry.
    ///
    /// Compressed formats have no fixed length and always validate.
    pub fn validate(&self) -> Result<(), FrameError> {
        match self.format.expected_len(self.width, self.height) {
            Some(expected) if expected != self.data.len() => Err(FrameError::LengthMismatch {
                expected,
                actual: self.data.len(),
            }),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey8_expected_len_matches_ir_node_geometry() {
        // The reference IR sensor: 640x360 GREY, one byte per pixel.
        assert_eq!(PixFmt::Grey8.expected_len(640, 360), Some(230_400));
    }

    #[test]
    fn yuyv_is_two_bytes_per_pixel() {
        assert_eq!(PixFmt::Yuyv.expected_len(640, 480), Some(614_400));
    }

    #[test]
    fn depth16_is_two_bytes_per_pixel() {
        assert_eq!(PixFmt::Depth16.expected_len(640, 360), Some(460_800));
    }

    #[test]
    fn compressed_formats_have_no_fixed_length() {
        assert_eq!(PixFmt::Mjpeg.bits_per_pixel(), None);
        assert_eq!(PixFmt::Mjpeg.expected_len(1920, 1080), None);
    }

    fn grey_frame(len: usize) -> Frame {
        Frame {
            data: vec![0u8; len],
            format: PixFmt::Grey8,
            width: 640,
            height: 360,
            ts_mono: Duration::from_millis(0),
            sof: None,
            emitter: EmitterState::Unknown,
            quality: None,
        }
    }

    #[test]
    fn correctly_sized_frame_validates() {
        assert!(grey_frame(230_400).validate().is_ok());
    }

    #[test]
    fn undersized_frame_is_rejected() {
        let err = grey_frame(1000).validate().unwrap_err();
        assert!(matches!(
            err,
            FrameError::LengthMismatch {
                expected: 230_400,
                actual: 1000
            }
        ));
    }

    #[test]
    fn compressed_frame_accepts_any_length() {
        let f = Frame {
            data: vec![0u8; 4242],
            format: PixFmt::Mjpeg,
            width: 1920,
            height: 1080,
            ts_mono: Duration::from_millis(0),
            sof: None,
            emitter: EmitterState::Unknown,
            quality: None,
        };
        assert!(f.validate().is_ok());
    }

    #[test]
    fn torn_frames_are_distinguishable() {
        // Torn frames must never be silently treated as usable.
        assert_ne!(EmitterState::Torn, EmitterState::On);
        assert_ne!(EmitterState::Torn, EmitterState::Off);
    }
}
