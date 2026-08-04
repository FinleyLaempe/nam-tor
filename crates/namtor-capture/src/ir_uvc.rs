// SPDX-License-Identifier: GPL-3.0-only
//! `IrUvc` capture source: V4L2 MMAP streaming for the IR sensor, with
//! optional metadata-node pairing and emitter-state tagging by frame parity.
//!
//! Spec §4.4: on MSXU-mode devices the firmware pairs frames (strict 1:1
//! alternation), so emitter state is frame parity — even = On, odd = Off. On
//! vendor-XU devices the userspace strobe tags via `strobe::classify`, but
//! that lives in `namtor-uvc`; here we accept an `EmitterPolicy` so the
//! caller selects the tagging strategy without this crate depending on
//! `namtor-uvc` (which already depends on `namtor-capture` for `Frame`).

use std::path::Path;
use std::time::Duration;

use crate::frame::{EmitterState, Frame, PixFmt};
use crate::source::{CaptureError, CaptureSource, SourceCaps};
use crate::v4l2::{V4l2Format, V4l2Stream, V4L2_PIX_FMT_GREY};

/// A raw frame from the streaming layer: owned bytes, kernel timestamp,
/// optional USB SOF counter, and a monotonic frame index for parity.
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub data: Vec<u8>,
    pub ts: Duration,
    pub sof: Option<u16>,
    pub frame_index: u64,
}

/// Abstracts the streaming layer so `IrUvc` is unit-testable without a device.
pub trait Streamer {
    fn next_raw(&mut self) -> Result<RawFrame, CaptureError>;
}

/// Tags each frame's emitter state. `Parity` for MSXU-mode firmware strobing;
/// `Constant(Unknown)` when the caller has not classified the strobe yet.
pub trait EmitterPolicy {
    fn tag(&self, frame_index: u64) -> EmitterState;
}

/// MSXU-mode parity: even frames are illuminated, odd frames are dark.
pub struct ParityPolicy;

impl EmitterPolicy for ParityPolicy {
    fn tag(&self, frame_index: u64) -> EmitterState {
        if frame_index.is_multiple_of(2) {
            EmitterState::On
        } else {
            EmitterState::Off
        }
    }
}

/// No strobe information yet — the pipeline has not classified the device.
pub struct UnknownPolicy;

impl EmitterPolicy for UnknownPolicy {
    fn tag(&self, _frame_index: u64) -> EmitterState {
        EmitterState::Unknown
    }
}

/// IR UVC capture source, generic over the streaming layer and the emitter
/// tagging policy.
pub struct IrUvc<S: Streamer, P: EmitterPolicy> {
    streamer: S,
    policy: P,
    caps: SourceCaps,
    started: bool,
    width: u32,
    height: u32,
}

impl<S: Streamer, P: EmitterPolicy> IrUvc<S, P> {
    pub fn new(streamer: S, policy: P, caps: SourceCaps) -> Self {
        let width = caps.width;
        let height = caps.height;
        Self {
            streamer,
            policy,
            caps,
            started: false,
            width,
            height,
        }
    }

    fn build_frame(&mut self, raw: RawFrame) -> Frame {
        Frame {
            data: raw.data,
            format: PixFmt::Grey8,
            width: self.width,
            height: self.height,
            ts_mono: raw.ts,
            sof: raw.sof,
            emitter: self.policy.tag(raw.frame_index),
            quality: None,
        }
    }
}

impl<S: Streamer, P: EmitterPolicy> CaptureSource for IrUvc<S, P> {
    fn caps(&self) -> SourceCaps {
        self.caps
    }

    fn start(&mut self) -> Result<(), CaptureError> {
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
        self.started = false;
    }
}

/// Production streaming layer: V4L2 MMAP on the IR node, with an optional
/// second V4L2 stream on the metadata node for SOF extraction.
pub struct V4l2IrStream {
    ir: V4l2Stream,
    meta: Option<V4l2Stream>,
    frame_index: u64,
    #[allow(dead_code)]
    format: V4l2Format,
}

impl V4l2IrStream {
    /// Opens the IR node and negotiates GREY format. If `metadata_node` is
    /// given, opens it too for per-frame SOF pairing.
    pub fn open(
        ir_node: &Path,
        metadata_node: Option<&Path>,
        width: u32,
        height: u32,
    ) -> Result<Self, CaptureError> {
        let mut ir = V4l2Stream::open(ir_node).map_err(v4l2_err_to_capture)?;
        let format = ir
            .negotiate_format(width, height, V4L2_PIX_FMT_GREY)
            .map_err(v4l2_err_to_capture)?;

        let meta = match metadata_node {
            Some(p) => {
                let m = V4l2Stream::open(p).map_err(v4l2_err_to_capture)?;
                Some(m)
            }
            None => None,
        };

        Ok(Self {
            ir,
            meta,
            frame_index: 0,
            format,
        })
    }

    /// Requests and mmap's `count` buffers on both streams, queues them, and
    /// starts streaming. Must be called before `next_raw`.
    pub fn start_streaming(&mut self, count: u32) -> Result<(), CaptureError> {
        let n = self
            .ir
            .request_buffers(count)
            .map_err(v4l2_err_to_capture)?;
        self.ir.mmap_buffers().map_err(v4l2_err_to_capture)?;
        for i in 0..n {
            self.ir.requeue(i as usize).map_err(v4l2_err_to_capture)?;
        }
        self.ir.streamon().map_err(v4l2_err_to_capture)?;

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

impl Streamer for V4l2IrStream {
    fn next_raw(&mut self) -> Result<RawFrame, CaptureError> {
        let (idx, ts) = self.ir.dequeue().map_err(v4l2_err_to_capture)?;
        let buf = self.ir.read_buffer(idx).map_err(v4l2_err_to_capture)?;
        let data = buf.to_vec();
        self.ir.requeue(idx).map_err(v4l2_err_to_capture)?;

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
    use std::cell::RefCell;

    /// Mock streamer that yields canned frames of alternating brightness.
    struct MockStreamer {
        frames: Vec<RawFrame>,
        next: RefCell<usize>,
    }

    impl MockStreamer {
        fn alternating(n: usize, width: u32, height: u32) -> Self {
            let len = (width * height) as usize;
            let frames = (0..n)
                .map(|i| RawFrame {
                    data: vec![if i % 2 == 0 { 200 } else { 0 }; len],
                    ts: Duration::from_millis(33 * i as u64),
                    sof: Some(i as u16),
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

    fn ir_caps() -> SourceCaps {
        SourceCaps {
            spectrum: crate::source::Spectrum::Ir,
            format: PixFmt::Grey8,
            width: 64,
            height: 64,
            fps: 30,
            has_metadata: true,
        }
    }

    #[test]
    fn next_frame_tags_emitter_by_parity() {
        let mock = MockStreamer::alternating(4, 64, 64);
        let mut ir = IrUvc::new(mock, ParityPolicy, ir_caps());
        ir.start().unwrap();

        let f0 = ir.next_frame().unwrap().unwrap();
        assert_eq!(f0.emitter, EmitterState::On, "frame 0 is even -> On");
        assert_eq!(f0.format, PixFmt::Grey8);
        assert_eq!(f0.width, 64);
        assert_eq!(f0.sof, Some(0));

        let f1 = ir.next_frame().unwrap().unwrap();
        assert_eq!(f1.emitter, EmitterState::Off, "frame 1 is odd -> Off");
        assert_eq!(f1.sof, Some(1));

        let f2 = ir.next_frame().unwrap().unwrap();
        assert_eq!(f2.emitter, EmitterState::On);

        let f3 = ir.next_frame().unwrap().unwrap();
        assert_eq!(f3.emitter, EmitterState::Off);
    }

    #[test]
    fn caps_report_ir_spectrum() {
        let mock = MockStreamer::alternating(1, 64, 64);
        let ir = IrUvc::new(mock, ParityPolicy, ir_caps());
        assert_eq!(ir.caps().spectrum, crate::source::Spectrum::Ir);
        assert!(ir.caps().has_metadata);
    }

    #[test]
    fn next_frame_before_start_is_not_started_error() {
        let mock = MockStreamer::alternating(1, 64, 64);
        let mut ir = IrUvc::new(mock, ParityPolicy, ir_caps());
        assert!(matches!(ir.next_frame(), Err(CaptureError::NotStarted)));
    }

    #[test]
    fn stop_then_next_is_not_started() {
        let mock = MockStreamer::alternating(2, 64, 64);
        let mut ir = IrUvc::new(mock, ParityPolicy, ir_caps());
        ir.start().unwrap();
        ir.stop();
        assert!(matches!(ir.next_frame(), Err(CaptureError::NotStarted)));
    }

    #[test]
    fn validate_passes_on_well_formed_frame() {
        let mock = MockStreamer::alternating(1, 64, 64);
        let mut ir = IrUvc::new(mock, ParityPolicy, ir_caps());
        ir.start().unwrap();
        let f = ir.next_frame().unwrap().unwrap();
        f.validate().expect("64x64 Grey8 frame must validate");
    }

    #[test]
    fn unknown_policy_tags_all_frames_unknown() {
        let mock = MockStreamer::alternating(2, 64, 64);
        let mut ir = IrUvc::new(mock, UnknownPolicy, ir_caps());
        ir.start().unwrap();
        for _ in 0..2 {
            let f = ir.next_frame().unwrap().unwrap();
            assert_eq!(f.emitter, EmitterState::Unknown);
        }
    }

    #[test]
    #[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
    fn live_ir_stream_dequeues_alternating_frames() {
        if std::env::var("NAMTOR_HW_TEST").is_err() {
            eprintln!("NAMTOR_HW_TEST not set; skipping");
            return;
        }
        let mut stream = V4l2IrStream::open(
            Path::new("/dev/video2"),
            Some(Path::new("/dev/video3")),
            640,
            360,
        )
        .expect("open /dev/video2 + /dev/video3");
        stream.start_streaming(4).expect("start streaming");

        let mut on_seen = false;
        let mut off_seen = false;
        for _ in 0..4 {
            let raw = stream.next_raw().expect("next_raw");
            assert_eq!(raw.data.len(), 230_400);
            if raw.frame_index.is_multiple_of(2) {
                on_seen = true;
            } else {
                off_seen = true;
            }
            if raw.sof.is_some() {
                eprintln!("frame {} sof={:?}", raw.frame_index, raw.sof);
            }
        }
        assert!(on_seen && off_seen, "must see both parity phases");
    }
}
