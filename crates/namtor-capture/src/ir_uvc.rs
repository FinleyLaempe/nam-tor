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
pub trait Streamer: Send {
    /// Frames the device produced that this streamer never dequeued, summed
    /// over the stream. Default `0` for mock streamers, which cannot drop.
    fn dropped_frames(&self) -> u64 {
        0
    }

    /// Requests and queues `buffer_count` MMAP buffers and starts streaming.
    /// Default is a no-op for mock streamers.
    fn start(&mut self, buffer_count: u32) -> Result<(), CaptureError> {
        let _ = buffer_count;
        Ok(())
    }

    /// Stops streaming and releases buffers. Default is a no-op.
    fn stop(&mut self) {}

    fn next_raw(&mut self) -> Result<RawFrame, CaptureError>;
}

/// Tags each frame's emitter state. `Parity` for MSXU-mode firmware strobing;
/// `Constant(Unknown)` when the caller has not classified the strobe yet.
pub trait EmitterPolicy: Send {
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

    fn dropped_frames(&self) -> u64 {
        self.streamer.dropped_frames()
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

/// Tracks the strobe phase of a V4L2 stream across dropped frames.
///
/// [`ParityPolicy`] reads emitter-on/emitter-off off the parity of the frame
/// index. A counter incremented once per dequeue assumes the consumer never
/// misses a frame; enrollment spends ~100 ms of ONNX per frame against a
/// sensor producing one every 33 ms, so the ring overruns and the driver
/// drops frames continuously. After a gap of odd length the label inverted
/// for the rest of the stream: every illuminated frame recorded as dark and
/// every dark one as illuminated.
///
/// Neither shipped consumer was harmed by that, and the reason is worth
/// stating so nobody relaxes it by accident. `VerifyEngine` pairs frames by
/// label, and an inverted label still alternates, so pairs still form; it
/// then calls `illuminated_first`, which reorders the pair by measured mean
/// brightness and discards the label. Enrollment's `PipelineEmbedSource`
/// ignores the label entirely and embeds every frame. So the label was
/// wrong, and nothing downstream currently depends on it being right —
/// which is exactly the state in which a later reader trusts it.
///
/// The driver's `sequence` counts frames it produced whether or not they
/// were dequeued, so stepping by its delta keeps the phase on the sensor. A
/// driver that reports no sequence leaves it pinned at 0; the delta floors
/// at 1 and the behaviour is the old one exactly.
#[derive(Debug, Default)]
struct PhaseTracker {
    index: u64,
    last: Option<u32>,
    dropped: u64,
}

impl PhaseTracker {
    /// Record a dequeued frame with driver sequence `sequence` and return
    /// the strobe phase index to label it with.
    fn observe(&mut self, sequence: u32) -> u64 {
        let step = match self.last {
            None => 0,
            // `wrapping_sub` because the driver's counter is u32 and wraps.
            // Flooring at 1 covers a driver that never moves it.
            Some(prev) => u64::from(sequence.wrapping_sub(prev)).max(1),
        };
        self.dropped += step.saturating_sub(1);
        self.last = Some(sequence);
        self.index += step;
        self.index
    }

    fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Production streaming layer: V4L2 MMAP on the IR node, with an optional
/// second V4L2 stream on the metadata node for SOF extraction.
pub struct V4l2IrStream {
    ir: V4l2Stream,
    meta: Option<V4l2Stream>,
    phase: PhaseTracker,
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
                let m = V4l2Stream::open_meta(p).map_err(v4l2_err_to_capture)?;
                Some(m)
            }
            None => None,
        };

        Ok(Self {
            ir,
            meta,
            phase: PhaseTracker::default(),
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

    /// Frames the driver produced that this stream never dequeued, summed
    /// over the life of the stream. Zero on a consumer that keeps up.
    pub fn dropped_frames(&self) -> u64 {
        self.phase.dropped()
    }

    fn dequeue_sof(&mut self) -> Option<u16> {
        let m = self.meta.as_mut()?;
        let (idx, _, _) = m.dequeue().ok()?;
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
    fn dropped_frames(&self) -> u64 {
        self.phase.dropped()
    }

    fn start(&mut self, buffer_count: u32) -> Result<(), CaptureError> {
        self.start_streaming(buffer_count)
    }

    fn stop(&mut self) {
        let _ = self.ir.streamoff().map_err(v4l2_err_to_capture);
        if let Some(m) = self.meta.as_mut() {
            let _ = m.streamoff().map_err(v4l2_err_to_capture);
        }
    }

    /// Dequeue one frame and advance the strobe phase by the driver's own
    /// frame-sequence delta.
    ///
    /// [`ParityPolicy`] labels a frame emitter-on or emitter-off from the
    /// parity of this counter. Counting one per dequeue silently assumed the
    /// consumer never misses a frame. It does — see [`PhaseTracker`], which
    /// also records why no shipped consumer was harmed by the resulting
    /// mislabelling, and why the label is worth getting right anyway.
    ///
    /// The driver's `sequence` counts frames it produced, dequeued or not,
    /// so stepping by its delta keeps the phase pinned to the sensor. A
    /// driver that does not report a sequence leaves it at 0, the delta
    /// floors at 1, and the behaviour is exactly the old one.
    fn next_raw(&mut self) -> Result<RawFrame, CaptureError> {
        let (idx, ts, sequence) = self.ir.dequeue().map_err(v4l2_err_to_capture)?;
        let buf = self.ir.read_buffer(idx).map_err(v4l2_err_to_capture)?;
        let data = buf.to_vec();
        self.ir.requeue(idx).map_err(v4l2_err_to_capture)?;

        let sof = self.dequeue_sof();
        let frame_index = self.phase.observe(sequence);

        Ok(RawFrame {
            data,
            ts,
            sof,
            frame_index,
        })
    }
}

fn v4l2_err_to_capture(e: crate::v4l2::V4l2Error) -> CaptureError {
    // `From<V4l2Error>` keeps EBUSY typed as `CaptureError::Busy`; everything
    // else flattens to `Device`.
    CaptureError::from(e)
}

#[cfg(test)]
mod phase_tests {
    use super::{EmitterPolicy, ParityPolicy, PhaseTracker};
    use crate::frame::EmitterState;

    fn label(seqs: &[u32]) -> Vec<EmitterState> {
        let mut t = PhaseTracker::default();
        seqs.iter()
            .map(|&s| ParityPolicy.tag(t.observe(s)))
            .collect()
    }

    /// With nothing dropped the labelling is unchanged: frame 0 is lit.
    #[test]
    fn an_unbroken_stream_alternates_from_on() {
        use EmitterState::{Off, On};
        assert_eq!(label(&[0, 1, 2, 3, 4]), vec![On, Off, On, Off, On]);
    }

    /// The regression. One dropped frame is an odd step, so a per-dequeue
    /// counter puts every later frame on the wrong side of the strobe:
    /// sensor frame 3 is emitter-off, but a counter that saw only three
    /// frames calls it index 2 and labels it on. PAD's differential is then
    /// computed backwards and enrollment searches the unlit frames for a
    /// face.
    #[test]
    fn one_dropped_frame_does_not_invert_the_rest_of_the_stream() {
        use EmitterState::{Off, On};
        // Dequeued sequences 0, 1, 3: the driver produced 2 and it was lost.
        assert_eq!(label(&[0, 1, 3]), vec![On, Off, Off]);
        // A whole run after the gap stays pinned to the sensor.
        assert_eq!(label(&[0, 1, 3, 4, 5, 6]), vec![On, Off, Off, On, Off, On]);
    }

    /// An even-length gap happens to preserve parity; it must still be
    /// counted as lost frames.
    #[test]
    fn an_even_gap_keeps_the_phase_and_is_still_counted() {
        use EmitterState::{Off, On};
        assert_eq!(label(&[0, 1, 4]), vec![On, Off, On]);
        let mut t = PhaseTracker::default();
        for s in [0, 1, 4] {
            t.observe(s);
        }
        assert_eq!(t.dropped(), 2, "frames 2 and 3 were produced and lost");
    }

    /// A driver that does not fill in `sequence` leaves it at 0 forever. The
    /// step floors at 1, so such a device behaves exactly as it did before
    /// the sequence was consulted at all.
    #[test]
    fn a_driver_with_no_sequence_falls_back_to_one_per_frame() {
        use EmitterState::{Off, On};
        assert_eq!(label(&[0, 0, 0, 0]), vec![On, Off, On, Off]);
        let mut t = PhaseTracker::default();
        for _ in 0..4 {
            t.observe(0);
        }
        assert_eq!(t.dropped(), 0, "a flat sequence is not evidence of loss");
    }

    /// The driver's counter is u32 and wraps. A wrap is a step of 1, not a
    /// four-billion-frame gap.
    #[test]
    fn the_sequence_wrapping_is_a_step_of_one() {
        let mut t = PhaseTracker::default();
        t.observe(u32::MAX - 1);
        let a = t.observe(u32::MAX);
        let b = t.observe(0);
        assert_eq!(b, a + 1, "wrap must advance by one");
        assert_eq!(t.dropped(), 0);
    }

    /// A stream that starts partway through the driver's counter still
    /// starts its own phase at 0 — the first frame of an armed window is the
    /// lit one by construction.
    #[test]
    fn the_first_frame_is_always_phase_zero() {
        let mut t = PhaseTracker::default();
        assert_eq!(t.observe(9_001), 0);
        assert_eq!(t.dropped(), 0);
    }
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
