// SPDX-License-Identifier: GPL-3.0-only
//! `RgbUvc` capture source: V4L2 MMAP streaming for the colour sensor.
//!
//! The RGB sensor has no emitter, so `emitter` is always `Unknown`. SOF is
//! extracted from the metadata node when available, same inline technique as
//! `IrUvc` (bytes 8-9 of the `V4L2_META_FMT_UVC` buffer) to avoid a cyclic
//! dependency on `namtor-uvc`.

use std::path::Path;

use crate::frame::{EmitterState, Frame};
use crate::source::{CaptureError, CaptureSource, SourceCaps};
use crate::v4l2::{V4l2Format, V4l2Stream, V4L2_PIX_FMT_GREY, V4L2_PIX_FMT_YUYV};

use crate::ir_uvc::{RawFrame, Streamer};

/// RGB UVC capture source, generic over the streaming layer.
pub struct RgbUvc<S: Streamer> {
    streamer: S,
    caps: SourceCaps,
    started: bool,
    width: u32,
    height: u32,
}

impl<S: Streamer> RgbUvc<S> {
    pub fn new(streamer: S, caps: SourceCaps) -> Self {
        let width = caps.width;
        let height = caps.height;
        Self {
            streamer,
            caps,
            started: false,
            width,
            height,
        }
    }

    fn build_frame(&self, raw: RawFrame) -> Frame {
        Frame {
            data: raw.data,
            format: self.caps.format,
            width: self.width,
            height: self.height,
            ts_mono: raw.ts,
            sof: raw.sof,
            emitter: EmitterState::Unknown,
            quality: None,
        }
    }
}

impl<S: Streamer> CaptureSource for RgbUvc<S> {
    fn caps(&self) -> SourceCaps {
        self.caps
    }

    fn start(&mut self) -> Result<(), CaptureError> {
        self.streamer.start(4)?;
        self.started = true;
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError> {
        if !self.started {
            return Err(CaptureError::NotStarted);
        }
        let raw = self.streamer.next_raw()?;
        Ok(Some(self.build_frame(raw)))
    }

    fn stop(&mut self) {
        self.streamer.stop();
        self.started = false;
    }
}

/// Production streaming layer for the RGB node.
pub struct V4l2RgbStream {
    rgb: V4l2Stream,
    meta: Option<V4l2Stream>,
    frame_index: u64,
    #[allow(dead_code)]
    format: V4l2Format,
}

impl V4l2RgbStream {
    /// Opens the RGB node and negotiates YUYV (or GREY fallback). If
    /// `metadata_node` is given, opens it too for SOF pairing.
    pub fn open(
        rgb_node: &Path,
        metadata_node: Option<&Path>,
        width: u32,
        height: u32,
    ) -> Result<Self, CaptureError> {
        let mut rgb = V4l2Stream::open(rgb_node).map_err(v4l2_err_to_capture)?;
        let format = rgb
            .negotiate_format(width, height, V4L2_PIX_FMT_YUYV)
            .or_else(|_| {
                rgb = V4l2Stream::open(rgb_node).map_err(v4l2_err_to_capture)?;
                rgb.negotiate_format(width, height, V4L2_PIX_FMT_GREY)
                    .map_err(v4l2_err_to_capture)
            })?;

        let meta = metadata_node
            .map(|p| V4l2Stream::open_meta(p).map_err(v4l2_err_to_capture))
            .transpose()?;

        Ok(Self {
            rgb,
            meta,
            frame_index: 0,
            format,
        })
    }

    pub fn start_streaming(&mut self, count: u32) -> Result<(), CaptureError> {
        let n = self
            .rgb
            .request_buffers(count)
            .map_err(v4l2_err_to_capture)?;
        self.rgb.mmap_buffers().map_err(v4l2_err_to_capture)?;
        for i in 0..n {
            self.rgb.requeue(i as usize).map_err(v4l2_err_to_capture)?;
        }
        self.rgb.streamon().map_err(v4l2_err_to_capture)?;

        if let Some(m) = self.meta.as_mut() {
            let mn = m.request_buffers(count).map_err(v4l2_err_to_capture)?;
            m.mmap_buffers().map_err(v4l2_err_to_capture)?;
            for i in 0..mn {
                m.requeue(i as usize).map_err(v4l2_err_to_capture)?;
            }
            m.streamon().map_err(v4l2_err_to_capture)?;
        }
        Ok(())
    }

    fn dequeue_sof(&mut self) -> Option<u16> {
        let m = self.meta.as_mut()?;
        let (idx, _) = m.dequeue().ok()?;
        let buf = m.read_buffer(idx).ok()?;
        let sof = if buf.len() >= 10 {
            Some(u16::from_ne_bytes([buf[8], buf[9]]))
        } else {
            None
        };
        let _ = m.requeue(idx);
        sof
    }
}

impl Streamer for V4l2RgbStream {
    fn start(&mut self, buffer_count: u32) -> Result<(), CaptureError> {
        self.start_streaming(buffer_count)
    }

    fn stop(&mut self) {
        let _ = self.rgb.streamoff().map_err(v4l2_err_to_capture);
        if let Some(m) = self.meta.as_mut() {
            let _ = m.streamoff().map_err(v4l2_err_to_capture);
        }
    }

    fn next_raw(&mut self) -> Result<RawFrame, CaptureError> {
        let (idx, ts) = self.rgb.dequeue().map_err(v4l2_err_to_capture)?;
        let buf = self.rgb.read_buffer(idx).map_err(v4l2_err_to_capture)?;
        let data = buf.to_vec();
        self.rgb.requeue(idx).map_err(v4l2_err_to_capture)?;

        let sof = self.dequeue_sof();
        let frame_index = self.frame_index;
        self.frame_index += 1;

        Ok(RawFrame {
            data,
            ts,
            sof,
            frame_index,
        })
    }
}

fn v4l2_err_to_capture(e: crate::v4l2::V4l2Error) -> CaptureError {
    CaptureError::Device(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::PixFmt;
    use crate::source::Spectrum;
    use std::cell::RefCell;
    use std::time::Duration;

    struct MockStreamer {
        frames: Vec<RawFrame>,
        next: RefCell<usize>,
    }

    impl MockStreamer {
        fn yuyv_frames(n: usize, width: u32, height: u32) -> Self {
            let len = (width * height * 2) as usize;
            let frames = (0..n)
                .map(|i| RawFrame {
                    data: vec![128; len],
                    ts: Duration::from_millis(33 * i as u64),
                    sof: None,
                    frame_index: i as u64,
                })
                .collect();
            Self {
                frames,
                next: RefCell::new(0),
            }
        }
    }

    impl Streamer for MockStreamer {
        fn next_raw(&mut self) -> Result<RawFrame, CaptureError> {
            let mut n = self.next.borrow_mut();
            if *n >= self.frames.len() {
                return Err(CaptureError::Device("stream exhausted".into()));
            }
            let f = self.frames[*n].clone();
            *n += 1;
            Ok(f)
        }
    }

    fn rgb_caps() -> SourceCaps {
        SourceCaps {
            spectrum: Spectrum::Rgb,
            format: PixFmt::Yuyv,
            width: 64,
            height: 64,
            fps: 30,
            has_metadata: false,
        }
    }

    #[test]
    fn caps_report_rgb_spectrum() {
        let mock = MockStreamer::yuyv_frames(1, 64, 64);
        let rgb = RgbUvc::new(mock, rgb_caps());
        assert_eq!(rgb.caps().spectrum, Spectrum::Rgb);
    }

    #[test]
    fn emitter_is_always_unknown() {
        let mock = MockStreamer::yuyv_frames(3, 64, 64);
        let mut rgb = RgbUvc::new(mock, rgb_caps());
        rgb.start().unwrap();
        for _ in 0..3 {
            let f = rgb.next_frame().unwrap().unwrap();
            assert_eq!(f.emitter, EmitterState::Unknown);
        }
    }

    #[test]
    fn next_frame_before_start_is_error() {
        let mock = MockStreamer::yuyv_frames(1, 64, 64);
        let mut rgb = RgbUvc::new(mock, rgb_caps());
        assert!(matches!(rgb.next_frame(), Err(CaptureError::NotStarted)));
    }

    #[test]
    fn validate_passes_on_yuyv_frame() {
        let mock = MockStreamer::yuyv_frames(1, 64, 64);
        let mut rgb = RgbUvc::new(mock, rgb_caps());
        rgb.start().unwrap();
        let f = rgb.next_frame().unwrap().unwrap();
        f.validate().expect("64x64 YUYV frame must validate");
    }

    #[test]
    #[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
    fn live_rgb_stream_dequeues_frames() {
        if std::env::var("NAMTOR_HW_TEST").is_err() {
            eprintln!("NAMTOR_HW_TEST not set; skipping");
            return;
        }
        let mut stream = V4l2RgbStream::open(Path::new("/dev/video0"), None, 640, 480)
            .expect("open /dev/video0");
        stream.start_streaming(4).expect("start streaming");
        for _ in 0..2 {
            let raw = stream.next_raw().expect("next_raw");
            eprintln!("rgb frame {} {} bytes", raw.frame_index, raw.data.len());
        }
    }
}
