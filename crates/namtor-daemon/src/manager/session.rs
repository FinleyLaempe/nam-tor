// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment capture session: bounded reservoir + diversity selection +
//! zeroize-on-drop (spec §6.6 Tier 1, plan task 11).
//!
//! [`EnrollmentCapture`] is the per-session state held by
//! [`crate::dbus::ManagerService`] while an enrollment is live. It owns a
//! pull-based [`CaptureSource`] and an [`EmbedSource`] that turns a captured
//! frame into a coarse-quality embedding candidate. Candidates accumulate in a
//! fixed-size reservoir; once full, a better candidate evicts the
//! lowest-quality one. At [`EnrollmentCapture::finish`] the reservoir is
//! reduced to a diverse template set via farthest-first traversal (reused from
//! the CLI enrollment session, spec §6.6) and every held raw buffer is
//! zeroized.
//!
//! Privacy: the only values that cross out of this module are coarse —
//! [`ProgressSnapshot`] carries a frame counter, a [`QualityLabel`], and an
//! integer coverage percent. No embedding, score, threshold, or raw pixel
//! buffer is returned or logged.
//
// allow: SIZE_OK — single cohesive enrollment-capture concept (reservoir +
// diversity selection + embed abstraction + zeroize). Splitting would
// fragment one state machine across files; the diversity selector and cosine
// helper are integral to finish(), not reusable elsewhere.

use namtor_capture::frame::Frame;
use namtor_capture::source::{CaptureError, CaptureSource};
use namtor_store::template::Template;
use zeroize::Zeroize;

use crate::dbus::{ApiError, QualityLabel};

/// Default reservoir bound. Bounds memory for multi-frame capture sessions
/// while giving the diversity selector a wide pool.
pub const DEFAULT_MAX_RESERVOIR: usize = 100;

/// Maximum frames [`EnrollmentCapture::pump_until_exhausted`] will pull in a
/// single commit-driven drain. Guards against an unbounded live source.
const PUMP_BUDGET: usize = 512;

/// Number of diverse templates selected from the finished reservoir.
const SELECT_N: usize = 8;

/// Minimum candidates required to produce a template set. Below this,
/// [`EnrollmentCapture::finish`] writes an empty template list rather than
/// a degenerate gallery.
const MIN_CANDIDATES: usize = 3;

/// A single captured candidate held in the reservoir.
struct Candidate {
    embedding: Vec<f32>,
    /// Coarse 0-100 quality score (detection confidence / sharpness / pose).
    quality: u8,
    raw_frame: Vec<u8>,
}

impl Candidate {
    fn score(&self) -> u8 {
        self.quality
    }
}

/// Coarse progress snapshot for the `EnrollmentProgress` signal. Carries no
/// biometric material — only a counter, a quality bucket, and an integer
/// coverage percent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub frames_captured: u32,
    pub quality: QualityLabel,
    pub coverage_percent: u32,
}

/// Lifecycle phase of an [`EnrollmentCapture`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePhase {
    Capturing,
    Finished,
    Cancelled,
}

/// Result of [`EnrollmentCapture::finish`]: the diverse template set ready for
/// the store and a scalar quality for the review label.
#[derive(Debug, Clone)]
pub struct FinishedCapture {
    pub templates: Vec<Template>,
    pub quality_scalar: f32,
}

/// Abstraction over "frame → embedding + coarse quality". Decouples the
/// enrollment session from the recognition pipeline so tests can run without
/// ONNX model weights. Production wraps [`namtor_recog::pipeline::RecognitionPipeline`].
pub trait EmbedSource: Send {
    /// Embed a captured frame. Returns `Ok(None)` when no face was detected
    /// (the frame is skipped); `Ok(Some(embedding, quality))` on success.
    fn embed(&mut self, frame: &Frame) -> Result<Option<(Vec<f32>, u8)>, ApiError>;
}

/// Deterministic mock embedder for tests. Produces a stable pseudo-embedding
/// from the frame bytes (LCG, like `namtor_worker::MockBackend`) and a coarse
/// quality from mean luminance. No model file is read.
pub struct MockEmbedSource {
    dim: usize,
}

impl MockEmbedSource {
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(1) }
    }
}

impl EmbedSource for MockEmbedSource {
    fn embed(&mut self, frame: &Frame) -> Result<Option<(Vec<f32>, u8)>, ApiError> {
        if frame.data.is_empty() {
            return Ok(None);
        }
        // LCG seeded by frame bytes — identical frames → identical embeddings.
        let mut state: u32 = 0x42;
        for &b in &frame.data {
            state = state.wrapping_mul(31).wrapping_add(b as u32);
        }
        let mut embedding = Vec::with_capacity(self.dim);
        for _ in 0..self.dim {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            embedding.push((state as f32 / u32::MAX as f32) * 2.0 - 1.0);
        }
        // Quality from mean luminance: ideal ~90-170 of 255.
        let mean: f32 = frame.data.iter().map(|&b| b as f32).sum::<f32>() / frame.data.len() as f32;
        let dist = (mean - 130.0).abs();
        let quality = ((1.0 - (dist / 130.0)).clamp(0.0, 1.0) * 100.0).round() as u8;
        Ok(Some((embedding, quality)))
    }
}

/// Per-uid enrollment capture session. Owns the capture source, the embedder,
/// and the candidate reservoir. Zeroizes every held raw buffer on drop.
pub struct EnrollmentCapture {
    target_uid: u32,
    peer_uid: u32,
    name: String,
    capture: Option<Box<dyn CaptureSource>>,
    embed: Option<Box<dyn EmbedSource>>,
    reservoir: Vec<Candidate>,
    max_reservoir: usize,
    total_seen: u32,
    phase: CapturePhase,
}

impl EnrollmentCapture {
    /// Build a new capturing session. `capture`/`embed` are `None` when the
    /// manager has no capture factory configured (the session can still be
    /// created for authorization/bus tests but produces no templates).
    pub fn new(
        target_uid: u32,
        peer_uid: u32,
        name: String,
        capture: Option<Box<dyn CaptureSource>>,
        embed: Option<Box<dyn EmbedSource>>,
        max_reservoir: usize,
    ) -> Self {
        let max_reservoir = max_reservoir.max(1);
        Self {
            target_uid,
            peer_uid,
            name,
            capture,
            embed,
            reservoir: Vec::with_capacity(max_reservoir),
            max_reservoir,
            total_seen: 0,
            phase: CapturePhase::Capturing,
        }
    }

    pub fn target_uid(&self) -> u32 {
        self.target_uid
    }

    pub fn peer_uid(&self) -> u32 {
        self.peer_uid
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn phase(&self) -> CapturePhase {
        self.phase
    }

    pub fn reservoir_len(&self) -> usize {
        self.reservoir.len()
    }

    pub fn total_seen(&self) -> u32 {
        self.total_seen
    }

    /// Pull one frame from the capture source, embed it, offer it to the
    /// reservoir, and return the coarse progress snapshot. Returns
    /// `Ok(None)` when the capture source is exhausted (`Ok(None)` from
    /// [`CaptureSource::next_frame`]).
    pub fn pump(&mut self) -> Result<Option<ProgressSnapshot>, ApiError> {
        if self.phase != CapturePhase::Capturing {
            return Err(ApiError::State("session not in capturing phase".into()));
        }
        let capture = self
            .capture
            .as_mut()
            .ok_or_else(|| ApiError::State("no capture source for session".into()))?;
        let embed = self
            .embed
            .as_mut()
            .ok_or_else(|| ApiError::State("no embed source for session".into()))?;
        let frame = match capture.next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => return Ok(None),
            Err(CaptureError::NotStarted) => {
                capture
                    .start()
                    .map_err(|e| ApiError::State(format!("capture start: {e}")))?;
                return self.pump();
            }
            Err(e) => return Err(ApiError::State(format!("capture: {e}"))),
        };
        frame
            .validate()
            .map_err(|e| ApiError::State(format!("frame validate: {e}")))?;
        self.total_seen += 1;
        let (embedding, quality) = match embed.embed(&frame)? {
            Some(v) => v,
            None => return Ok(Some(self.snapshot())),
        };
        let candidate = Candidate {
            embedding,
            quality,
            raw_frame: frame.data,
        };
        self.offer(candidate);
        Ok(Some(self.snapshot()))
    }

    /// Drain the capture source until exhaustion or the pump budget, then
    /// finish. A no-op when no capture source is configured (the session
    /// produces an empty template set on the subsequent [`Self::finish`]).
    pub fn pump_until_exhausted(&mut self) -> Result<(), ApiError> {
        if self.capture.is_none() || self.embed.is_none() {
            return Ok(());
        }
        for _ in 0..PUMP_BUDGET {
            match self.pump()? {
                Some(_) => continue,
                None => break,
            }
        }
        Ok(())
    }

    /// Stop capturing, select a diverse template set, zeroize the reservoir,
    /// and transition to `Finished`.
    pub fn finish(&mut self) -> Result<FinishedCapture, ApiError> {
        if self.phase != CapturePhase::Capturing {
            return Err(ApiError::State("session not in capturing phase".into()));
        }
        if self.reservoir.len() < MIN_CANDIDATES {
            self.zeroize_reservoir();
            self.phase = CapturePhase::Finished;
            return Ok(FinishedCapture {
                templates: Vec::new(),
                quality_scalar: 0.0,
            });
        }
        let selected_idx = select_diverse(&self.reservoir, SELECT_N);
        let mut templates = Vec::with_capacity(selected_idx.len());
        let mut quality_sum: f32 = 0.0;
        for (rank, &i) in selected_idx.iter().enumerate() {
            let c = &self.reservoir[i];
            templates.push(Template {
                vector: c.embedding.clone(),
                quality: c.quality as f32 / 100.0,
                is_anchor: rank == 0,
            });
            quality_sum += c.quality as f32;
        }
        let quality_scalar = quality_sum / selected_idx.len() as f32 / 100.0;
        self.zeroize_reservoir();
        self.phase = CapturePhase::Finished;
        Ok(FinishedCapture {
            templates,
            quality_scalar,
        })
    }

    /// Discard everything and zeroize held buffers. Idempotent.
    pub fn cancel(&mut self) {
        self.zeroize_reservoir();
        if let Some(mut c) = self.capture.take() {
            c.stop();
        }
        self.embed = None;
        self.phase = CapturePhase::Cancelled;
    }

    /// Coarse progress snapshot for the `EnrollmentProgress` signal.
    pub fn snapshot(&self) -> ProgressSnapshot {
        let avg_quality = if self.reservoir.is_empty() {
            0.0
        } else {
            self.reservoir.iter().map(|c| c.score() as f32).sum::<f32>()
                / self.reservoir.len() as f32
                / 100.0
        };
        let coverage = if self.max_reservoir == 0 {
            0
        } else {
            ((self.reservoir.len() as u32 * 100) / self.max_reservoir as u32).min(100)
        };
        ProgressSnapshot {
            frames_captured: self.total_seen,
            quality: QualityLabel::from_scalar(avg_quality),
            coverage_percent: coverage,
        }
    }

    fn offer(&mut self, candidate: Candidate) {
        if self.reservoir.len() < self.max_reservoir {
            self.reservoir.push(candidate);
            return;
        }
        let (min_idx, min_score) = self
            .reservoir
            .iter()
            .enumerate()
            .map(|(i, c)| (i, c.score()))
            .min_by_key(|&(_, s)| s)
            .unwrap_or((0, 0));
        if candidate.score() <= min_score {
            let mut dropped = candidate;
            dropped.raw_frame.zeroize();
        } else {
            let evicted = std::mem::replace(&mut self.reservoir[min_idx], candidate);
            let mut dropped = evicted;
            dropped.raw_frame.zeroize();
        }
    }

    fn zeroize_reservoir(&mut self) {
        for c in self.reservoir.drain(..) {
            let mut dropped = c;
            dropped.raw_frame.zeroize();
            dropped.embedding.zeroize();
        }
    }
}

impl Drop for EnrollmentCapture {
    fn drop(&mut self) {
        self.zeroize_reservoir();
        if let Some(mut c) = self.capture.take() {
            c.stop();
        }
    }
}

/// Diversity-aware selection via farthest-first traversal seeded by the
/// highest-quality candidate. Mirrors the CLI enrollment session's selector
/// (spec §6.6 Tier 1).
fn select_diverse(reservoir: &[Candidate], n: usize) -> Vec<usize> {
    if reservoir.is_empty() || n == 0 {
        return Vec::new();
    }
    let n = n.min(reservoir.len());
    let mut selected: Vec<usize> = Vec::with_capacity(n);
    let mut remaining: Vec<usize> = (0..reservoir.len()).collect();

    let seed = remaining
        .iter()
        .copied()
        .max_by(|&a, &b| reservoir[a].score().cmp(&reservoir[b].score()))
        .unwrap_or(0);
    selected.push(seed);
    remaining.retain(|&i| i != seed);

    while selected.len() < n && !remaining.is_empty() {
        let mut best_idx = 0;
        let mut best_sum_sim = f32::INFINITY;
        for (r_pos, &r_idx) in remaining.iter().enumerate() {
            let mut sum_sim = 0.0_f32;
            for &s_idx in &selected {
                sum_sim +=
                    cosine_similarity(&reservoir[r_idx].embedding, &reservoir[s_idx].embedding);
            }
            if sum_sim < best_sum_sim {
                best_sum_sim = sum_sim;
                best_idx = r_pos;
            }
        }
        let picked = remaining.remove(best_idx);
        selected.push(picked);
    }
    selected
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na > 1e-10 && nb > 1e-10 {
        dot / (na * nb)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_capture::frame::{EmitterState, PixFmt};
    use namtor_capture::source::{CaptureError, CaptureSource, SourceCaps, Spectrum};
    use std::time::Duration;

    /// Replay capture source: serves a pre-loaded vector of frames, then
    /// returns `Ok(None)`.
    struct ReplaySource {
        caps: SourceCaps,
        frames: Vec<Frame>,
        idx: usize,
        started: bool,
    }

    impl ReplaySource {
        fn new(frames: Vec<Frame>) -> Self {
            Self {
                caps: SourceCaps {
                    spectrum: Spectrum::Ir,
                    format: PixFmt::Grey8,
                    width: 8,
                    height: 8,
                    fps: 30,
                    has_metadata: false,
                },
                frames,
                idx: 0,
                started: false,
            }
        }
    }

    impl CaptureSource for ReplaySource {
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
            if self.idx >= self.frames.len() {
                return Ok(None);
            }
            let f = self.frames[self.idx].clone();
            self.idx += 1;
            Ok(Some(f))
        }
        fn stop(&mut self) {
            self.started = false;
        }
    }

    fn grey_frame(byte: u8, w: u32, h: u32) -> Frame {
        Frame {
            data: vec![byte; (w * h) as usize],
            format: PixFmt::Grey8,
            width: w,
            height: h,
            ts_mono: Duration::from_millis(0),
            sof: None,
            emitter: EmitterState::On,
            quality: None,
        }
    }

    #[test]
    fn pump_pulls_frames_and_populates_reservoir() {
        let frames: Vec<Frame> = (0..5).map(|i| grey_frame(100 + i, 8, 8)).collect();
        let capture = Box::new(ReplaySource::new(frames));
        let embed = Box::new(MockEmbedSource::new(8));
        let mut s =
            EnrollmentCapture::new(1000, 1000, "alice".into(), Some(capture), Some(embed), 100);
        for _ in 0..5 {
            assert!(s.pump().unwrap().is_some());
        }
        assert!(s.pump().unwrap().is_none(), "source exhausted");
        assert_eq!(s.total_seen(), 5);
        assert_eq!(s.reservoir_len(), 5);
        assert_eq!(s.phase(), CapturePhase::Capturing);
    }

    #[test]
    fn finish_selects_diverse_templates_and_zeroizes() {
        let frames: Vec<Frame> = (0..10).map(|i| grey_frame(110 + i, 8, 8)).collect();
        let capture = Box::new(ReplaySource::new(frames));
        let embed = Box::new(MockEmbedSource::new(8));
        let mut s =
            EnrollmentCapture::new(1000, 1000, "alice".into(), Some(capture), Some(embed), 100);
        s.pump_until_exhausted().unwrap();
        let finished = s.finish().unwrap();
        assert!(finished.templates.len() <= 8);
        assert!(!finished.templates.is_empty());
        assert!(finished.templates[0].is_anchor);
        assert_eq!(s.phase(), CapturePhase::Finished);
        assert_eq!(s.reservoir_len(), 0, "reservoir zeroized after finish");
    }

    #[test]
    fn finish_with_too_few_candidates_yields_empty_templates() {
        let frames = vec![grey_frame(100, 8, 8)];
        let capture = Box::new(ReplaySource::new(frames));
        let embed = Box::new(MockEmbedSource::new(8));
        let mut s =
            EnrollmentCapture::new(1000, 1000, "alice".into(), Some(capture), Some(embed), 100);
        s.pump().unwrap();
        let finished = s.finish().unwrap();
        assert!(finished.templates.is_empty());
    }

    #[test]
    fn cancel_zeroizes_and_stops_capture() {
        let frames: Vec<Frame> = (0..5).map(|i| grey_frame(100 + i, 8, 8)).collect();
        let capture = Box::new(ReplaySource::new(frames));
        let embed = Box::new(MockEmbedSource::new(8));
        let mut s =
            EnrollmentCapture::new(1000, 1000, "alice".into(), Some(capture), Some(embed), 100);
        for _ in 0..3 {
            s.pump().unwrap();
        }
        s.cancel();
        assert_eq!(s.phase(), CapturePhase::Cancelled);
        assert_eq!(s.reservoir_len(), 0);
    }

    #[test]
    fn pump_after_cancel_errors() {
        let frames = vec![grey_frame(100, 8, 8)];
        let capture = Box::new(ReplaySource::new(frames));
        let embed = Box::new(MockEmbedSource::new(8));
        let mut s =
            EnrollmentCapture::new(1000, 1000, "alice".into(), Some(capture), Some(embed), 100);
        s.cancel();
        assert!(s.pump().is_err());
    }

    #[test]
    fn reservoir_evicts_lowest_quality_when_full() {
        let mut s = EnrollmentCapture::new(1000, 1000, "a".into(), None, None, 3);
        // Manually offer candidates via the internal path through pump would
        // need a capture source; instead exercise offer through a tiny source.
        let frames = vec![
            grey_frame(50, 8, 8),
            grey_frame(60, 8, 8),
            grey_frame(70, 8, 8),
            grey_frame(80, 8, 8),
        ];
        let capture = Box::new(ReplaySource::new(frames));
        let embed = Box::new(MockEmbedSource::new(8));
        s.capture = Some(capture);
        s.embed = Some(embed);
        for _ in 0..4 {
            s.pump().unwrap();
        }
        assert_eq!(s.reservoir_len(), 3, "reservoir stays bounded");
    }

    #[test]
    fn snapshot_reports_coarse_progress_without_biometrics() {
        let frames: Vec<Frame> = (0..10).map(|_| grey_frame(130, 8, 8)).collect();
        let capture = Box::new(ReplaySource::new(frames));
        let embed = Box::new(MockEmbedSource::new(8));
        let mut s =
            EnrollmentCapture::new(1000, 1000, "alice".into(), Some(capture), Some(embed), 10);
        for _ in 0..5 {
            s.pump().unwrap();
        }
        let snap = s.snapshot();
        assert_eq!(snap.frames_captured, 5);
        assert!(snap.coverage_percent <= 100);
        // Quality label is a coarse enum, not a score.
        assert!(matches!(
            snap.quality,
            QualityLabel::Low | QualityLabel::Medium | QualityLabel::High
        ));
    }

    #[test]
    fn pump_without_capture_errors() {
        let mut s = EnrollmentCapture::new(1000, 1000, "a".into(), None, None, 10);
        assert!(matches!(s.pump(), Err(ApiError::State(_))));
    }
}
