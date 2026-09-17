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
use namtor_recog::FrameMetrics;
use namtor_store::template::Template;
use zeroize::Zeroize;

use crate::dbus::{ApiError, QualityLabel};
use crate::manager::observation::{self, CaptureObservation, Guidance};

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
    /// The frame's real per-frame measurements, kept so the review screen can
    /// aggregate the same signals the capture screen showed live.
    metrics: FrameMetrics,
    /// Coarse 0-100 capture quality, derived once from [`Self::metrics`] via
    /// [`quality_percent_of`]. Cached because it drives reservoir eviction on
    /// every offer.
    quality: u8,
    raw_frame: Vec<u8>,
}

impl Candidate {
    fn new(embedding: Vec<f32>, metrics: FrameMetrics, raw_frame: Vec<u8>) -> Self {
        Self {
            embedding,
            quality: quality_percent_of(&metrics),
            metrics,
            raw_frame,
        }
    }

    fn score(&self) -> u8 {
        self.quality
    }
}

/// Coarse 0-100 capture quality for one frame's measurements. `diversity` is
/// deliberately 0: [`observation::compute_quality_percent`] does not read
/// `embedding_spread`, and a per-frame quality must not depend on which other
/// frames happen to be retained.
pub(crate) fn quality_percent_of(m: &FrameMetrics) -> u8 {
    observation::compute_quality_percent(&observation::observation_from(m, 0)).min(100) as u8
}

/// Coarse progress snapshot for the `EnrollmentProgress` signal. Carries no
/// biometric material — only a counter, a quality bucket, and an integer
/// coverage percent.
///
/// `Eq` is deliberately absent: [`Self::metrics`] carries `f32` measurements.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgressSnapshot {
    /// Frames pulled from the capture source and structurally validated far
    /// enough to be judged. Monotonic.
    pub frames_captured: u32,
    pub quality: QualityLabel,
    /// Reservoir FILL percent (retained candidates / reservoir capacity). This
    /// is NOT diversity — see [`EnrollmentCapture::diversity_percent`].
    pub coverage_percent: u32,
    /// Frames admitted to the reservoir, counting those that evicted a worse
    /// candidate. Monotonic; NOT the reservoir length, which is capped at
    /// `max_reservoir`.
    pub frames_accepted: u32,
    /// Total declined frames: the sum of the four per-reason counters below.
    /// `frames_captured == frames_accepted + frames_rejected` always holds.
    pub frames_rejected: u32,
    /// Declined because no face was detected in the frame.
    pub rejected_no_face: u32,
    /// Declined because the reservoir was full and the candidate did not beat
    /// the worst retained one.
    pub rejected_below_floor: u32,
    /// Declined because the frame failed structural validation (torn or
    /// malformed buffer).
    pub rejected_torn: u32,
    /// Declined because the embedder itself failed on the frame.
    pub rejected_embed_error: u32,
    /// The measurements of the most recently judged frame, or `None` before
    /// the first frame has been embedded. This is what the live guidance is
    /// derived from — the capture screen advises about the frame in front of
    /// the camera right now, not about the retained average.
    ///
    /// Daemon-internal. It never crosses D-Bus: `GuidanceProgress` is the
    /// only projection that does, and it carries bounded percentages only.
    pub metrics: Option<FrameMetrics>,
}

/// Lifecycle phase of an [`EnrollmentCapture`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePhase {
    Capturing,
    /// Capture paused; the bounded reservoir is retained for review/resume.
    /// Transition here via [`EnrollmentCapture::stop`]; leave via
    /// [`EnrollmentCapture::resume`] or [`EnrollmentCapture::finish`].
    Stopped,
    Finished,
    Cancelled,
}

/// Privacy-safe frozen summary of a stopped enrollment session (plan task 4).
/// Returned by [`EnrollmentCapture::review_snapshot`] when the session is in
/// the [`Stopped`](CapturePhase::Stopped) phase. Carries only bounded
/// percentages, a readiness flag, a frame counter, and a snake_case guidance
/// string — no embeddings, raw frames, thresholds, or match scores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSnapshot {
    /// `true` when the reservoir holds at least [`MIN_CANDIDATES`] candidates.
    pub ready_to_save: bool,
    /// 0-100 aggregate capture-suitability percent (NOT authentication accuracy).
    pub quality_percent: u32,
    /// 0-100 retained-embedding spread percent.
    pub diversity_percent: u32,
    /// Frames admitted to the reservoir over the whole session, counting
    /// those that evicted a worse candidate. Monotonic, and the SAME quantity
    /// the capture screen shows as `frames_accepted` — it is deliberately not
    /// the reservoir length, which is capped at `max_reservoir`.
    pub frames_accepted: u32,
    /// Snake_case guidance enum value (mapped client-side from the
    /// [`Guidance`] enum, see [`guidance_to_snake_case`]).
    pub guidance: String,
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
    /// (a counted rejection, see [`ProgressSnapshot::rejected_no_face`]);
    /// `Ok(Some((embedding, metrics)))` on success.
    ///
    /// The full [`FrameMetrics`] is returned rather than a single quality
    /// byte because the guidance ladder needs the measurements to be
    /// independent: collapsing them into one scalar is what made six of the
    /// nine [`Guidance`] variants unreachable.
    fn embed(&mut self, frame: &Frame) -> Result<Option<(Vec<f32>, FrameMetrics)>, ApiError>;
}

/// Deterministic mock embedder for tests. Produces a stable pseudo-embedding
/// from the frame bytes (LCG, like `namtor_worker::MockBackend`) and a
/// synthetic [`FrameMetrics`]. No model file is read.
///
/// This is a test double, not a simulator. The pixel-derived terms
/// (`detection_confidence`, `blur`, `illumination`) all track mean luminance
/// because a synthetic uniform fill carries no other signal to derive them
/// from — inventing a fake gradient would be a different fabrication from the
/// one task 3.5 removed, not an improvement. The geometric terms are fixed at
/// the values a well-framed frontal face produces.
///
/// To drive a *specific* guidance branch, use [`Self::with_metrics`], which
/// returns exactly the metrics it was given for every frame. The coverage
/// test `mock_embed_source_can_drive_every_guidance_variant` uses it to walk
/// all nine variants through a real capture session.
pub struct MockEmbedSource {
    dim: usize,
    fixed: Option<FrameMetrics>,
}

impl MockEmbedSource {
    pub fn new(dim: usize) -> Self {
        Self {
            dim: dim.max(1),
            fixed: None,
        }
    }

    /// Return `metrics` verbatim for every embedded frame. The embedding is
    /// still derived from the frame bytes, so diversity behaviour is
    /// unchanged.
    pub fn with_metrics(dim: usize, metrics: FrameMetrics) -> Self {
        Self {
            dim: dim.max(1),
            fixed: Some(metrics),
        }
    }

    /// The synthetic metrics this mock reports for a frame whose mean
    /// luminance maps to `band` (`0.0..=1.0`): a well-framed, frontal,
    /// unclipped face whose pixel-derived terms all sit at `band`. Exposed so
    /// other test doubles can produce metrics consistent with this one.
    pub fn nominal_metrics(band: f32) -> FrameMetrics {
        let band = band.clamp(0.0, 1.0);
        FrameMetrics {
            detection_confidence: band,
            blur: band,
            illumination: band,
            yaw_deg: 0.0,
            roll_deg: 0.0,
            face_fraction: 0.20,
            landmark_spread: 0.40,
            saturated_fraction: 0.0,
        }
    }

    /// Mean-luminance band, peaked at 130 of 255 — the same curve the mock
    /// has always used for its coarse quality.
    fn luminance_band(data: &[u8]) -> f32 {
        if data.is_empty() {
            return 0.0;
        }
        let mean: f32 = data.iter().map(|&b| b as f32).sum::<f32>() / data.len() as f32;
        (1.0 - ((mean - 130.0).abs() / 130.0)).clamp(0.0, 1.0)
    }
}

impl EmbedSource for MockEmbedSource {
    fn embed(&mut self, frame: &Frame) -> Result<Option<(Vec<f32>, FrameMetrics)>, ApiError> {
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
        if let Some(m) = self.fixed {
            return Ok(Some((embedding, m)));
        }
        let metrics = Self::nominal_metrics(Self::luminance_band(&frame.data));
        Ok(Some((embedding, metrics)))
    }
}

/// Per-uid enrollment capture session. Owns the capture source, the embedder,
/// and the candidate reservoir. Zeroizes every held raw buffer on drop.
pub struct EnrollmentCapture {
    target_uid: u32,
    peer_uid: u32,
    peer_bus_name: String,
    name: String,
    capture: Option<Box<dyn CaptureSource>>,
    embed: Option<Box<dyn EmbedSource>>,
    reservoir: Vec<Candidate>,
    max_reservoir: usize,
    total_seen: u32,
    /// Frames admitted to the reservoir, incremented on BOTH the admit and the
    /// evict path in [`Self::offer`]. Monotonic: unlike `reservoir.len()` it
    /// does not freeze when the reservoir reaches `max_reservoir`.
    accepted: u32,
    /// Frames declined because [`EmbedSource::embed`] returned `Ok(None)`,
    /// i.e. no face was detected. A real, user-actionable reason — this used
    /// to be a silent drop.
    rejected_no_face: u32,
    /// Frames declined because the reservoir was full and the candidate did
    /// not beat the worst retained candidate.
    rejected_below_floor: u32,
    /// Frames declined because the frame failed structural validation.
    rejected_torn: u32,
    /// Frames declined because the embedder errored on the frame.
    rejected_embed_error: u32,
    /// Measurements of the most recently judged frame. `None` before the
    /// first frame, and cleared again whenever a frame yields no detection.
    last_metrics: Option<FrameMetrics>,
    phase: CapturePhase,
}

impl EnrollmentCapture {
    /// Build a new capturing session. `capture`/`embed` are `None` when the
    /// manager has no capture factory configured (the session can still be
    /// created for authorization/bus tests but produces no templates).
    /// `peer_bus_name` is the D-Bus unique connection name (empty for the PAM
    /// socket path) used by [`ManagerService::handle_disconnect_by_bus_name`]
    /// to cancel sessions when the caller vanishes from the bus.
    pub fn new(
        target_uid: u32,
        peer_uid: u32,
        peer_bus_name: String,
        name: String,
        capture: Option<Box<dyn CaptureSource>>,
        embed: Option<Box<dyn EmbedSource>>,
        max_reservoir: usize,
    ) -> Self {
        let max_reservoir = max_reservoir.max(1);
        Self {
            target_uid,
            peer_uid,
            peer_bus_name,
            name,
            capture,
            embed,
            reservoir: Vec::with_capacity(max_reservoir),
            max_reservoir,
            total_seen: 0,
            accepted: 0,
            rejected_no_face: 0,
            rejected_below_floor: 0,
            rejected_torn: 0,
            rejected_embed_error: 0,
            last_metrics: None,
            phase: CapturePhase::Capturing,
        }
    }

    pub fn target_uid(&self) -> u32 {
        self.target_uid
    }

    pub fn peer_uid(&self) -> u32 {
        self.peer_uid
    }

    pub fn peer_bus_name(&self) -> &str {
        &self.peer_bus_name
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

    /// Frames admitted to the reservoir over the session, including those that
    /// evicted a worse candidate. Monotonic; see [`Self::reservoir_len`] for
    /// the capped count of candidates currently held.
    pub fn frames_accepted(&self) -> u32 {
        self.accepted
    }

    /// Frames declined, summed across every stated reason. Together with
    /// [`Self::frames_accepted`] this always equals [`Self::total_seen`].
    pub fn frames_rejected(&self) -> u32 {
        self.rejected_no_face
            .saturating_add(self.rejected_below_floor)
            .saturating_add(self.rejected_torn)
            .saturating_add(self.rejected_embed_error)
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
        if let Err(e) = frame.validate() {
            // A torn/malformed frame was still pulled off the device: count it
            // as seen AND rejected so `seen == accepted + rejected` holds.
            self.total_seen += 1;
            self.rejected_torn += 1;
            return Err(ApiError::State(format!("frame validate: {e}")));
        }
        self.total_seen += 1;
        let embedded = match embed.embed(&frame) {
            Ok(v) => v,
            Err(e) => {
                self.rejected_embed_error += 1;
                return Err(e);
            }
        };
        let (embedding, metrics) = match embedded {
            Some(v) => v,
            None => {
                // `Ok(None)` means "no face detected" (see `EmbedSource`), a
                // real user-actionable rejection, not a frame to drop silently.
                // There are no measurements to guide with, so the last ones
                // are cleared rather than left to go stale.
                self.rejected_no_face += 1;
                self.last_metrics = None;
                return Ok(Some(self.snapshot()));
            }
        };
        self.last_metrics = Some(metrics);
        self.offer(Candidate::new(embedding, metrics, frame.data));
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
    /// and transition to `Finished`. Only callable from the
    /// [`Stopped`](CapturePhase::Stopped) phase — a Capturing session must
    /// [`stop`](Self::stop) first. This enforces the Stop→Commit flow so the
    /// caller always has a review checkpoint before persistence.
    pub fn finish(&mut self) -> Result<FinishedCapture, ApiError> {
        if self.phase != CapturePhase::Stopped {
            return Err(ApiError::State("session not in stopped phase".into()));
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
        let frames_rejected = self.frames_rejected();
        debug_assert_eq!(
            self.total_seen,
            self.accepted + frames_rejected,
            "every seen frame must be accepted or rejected for a stated reason"
        );
        ProgressSnapshot {
            frames_captured: self.total_seen,
            quality: QualityLabel::from_scalar(avg_quality),
            coverage_percent: coverage,
            frames_accepted: self.accepted,
            frames_rejected,
            rejected_no_face: self.rejected_no_face,
            rejected_below_floor: self.rejected_below_floor,
            rejected_torn: self.rejected_torn,
            rejected_embed_error: self.rejected_embed_error,
            metrics: self.last_metrics,
        }
    }

    /// Measurements of the most recently judged frame, if any.
    pub fn last_metrics(&self) -> Option<FrameMetrics> {
        self.last_metrics
    }

    /// Transition Capturing→Stopped. Retains the bounded reservoir (does NOT
    /// zeroize). Stops the capture source (emitter off) but keeps the embed
    /// source for a subsequent [`resume`](Self::resume). Idempotent: calling
    /// `stop()` when already [`Stopped`](CapturePhase::Stopped) is a no-op.
    pub fn stop(&mut self) -> Result<(), ApiError> {
        if self.phase == CapturePhase::Stopped {
            return Ok(());
        }
        if self.phase != CapturePhase::Capturing {
            return Err(ApiError::State("stop requires capturing phase".into()));
        }
        if let Some(c) = self.capture.as_mut() {
            // A consumer slower than the sensor is the difference between a
            // session that enrols and one that quietly produces nothing, and
            // it leaves no other trace. Say so once, at stop, rather than
            // letting the operator infer it from an empty template set.
            let dropped = c.dropped_frames();
            if dropped > 0 {
                tracing::warn!(
                    dropped,
                    seen = self.total_seen,
                    accepted = self.accepted,
                    "capture could not keep up: frames were produced and lost"
                );
            }
            c.stop();
        }
        self.phase = CapturePhase::Stopped;
        Ok(())
    }

    /// Transition Stopped→Capturing. Restarts the capture source. The
    /// reservoir is preserved across the Stop→Resume cycle.
    pub fn resume(&mut self) -> Result<(), ApiError> {
        if self.phase != CapturePhase::Stopped {
            return Err(ApiError::State("resume requires stopped phase".into()));
        }
        if let Some(c) = self.capture.as_mut() {
            c.start()
                .map_err(|e| ApiError::State(format!("capture restart: {e}")))?;
        }
        self.phase = CapturePhase::Capturing;
        Ok(())
    }

    /// Return a frozen privacy-safe summary of the stopped session. Only
    /// callable in the [`Stopped`](CapturePhase::Stopped) phase.
    pub fn review_snapshot(&self) -> Result<ReviewSnapshot, ApiError> {
        if self.phase != CapturePhase::Stopped {
            return Err(ApiError::State("review requires stopped phase".into()));
        }
        // Same quantity the capture screen reports under the same label.
        let frames_accepted = self.accepted;
        let ready_to_save = self.reservoir.len() >= MIN_CANDIDATES;
        let quality_percent = self.avg_quality_percent();
        let diversity_percent = self.diversity_percent();
        let guidance = self.review_guidance(diversity_percent);
        Ok(ReviewSnapshot {
            ready_to_save,
            quality_percent,
            diversity_percent,
            frames_accepted,
            guidance,
        })
    }

    fn avg_quality_percent(&self) -> u32 {
        if self.reservoir.is_empty() {
            return 0;
        }
        let avg = self.reservoir.iter().map(|c| c.quality as f32).sum::<f32>()
            / self.reservoir.len() as f32;
        observation::clamp_percent(avg)
    }

    /// Mean of the retained candidates' measurements — the review screen's
    /// input, the aggregate counterpart of [`Self::last_metrics`].
    ///
    /// `yaw_deg` and `roll_deg` are averaged as **magnitudes**. A diverse
    /// reservoir deliberately holds faces turned both ways, so their signed
    /// mean tends to zero and would report a set of profile shots as
    /// perfectly frontal. The review question is "was the pose acceptable",
    /// which is a question about magnitude.
    fn mean_metrics(&self) -> Option<FrameMetrics> {
        let n = self.reservoir.len();
        if n == 0 {
            return None;
        }
        let inv = 1.0 / n as f32;
        let mut acc = FrameMetrics {
            detection_confidence: 0.0,
            blur: 0.0,
            illumination: 0.0,
            yaw_deg: 0.0,
            roll_deg: 0.0,
            face_fraction: 0.0,
            landmark_spread: 0.0,
            saturated_fraction: 0.0,
        };
        for c in &self.reservoir {
            acc.detection_confidence += c.metrics.detection_confidence * inv;
            acc.blur += c.metrics.blur * inv;
            acc.illumination += c.metrics.illumination * inv;
            acc.yaw_deg += c.metrics.yaw_deg.abs() * inv;
            acc.roll_deg += c.metrics.roll_deg.abs() * inv;
            acc.face_fraction += c.metrics.face_fraction * inv;
            acc.landmark_spread += c.metrics.landmark_spread * inv;
            acc.saturated_fraction += c.metrics.saturated_fraction * inv;
        }
        Some(acc)
    }

    /// Mean pairwise `1 - cosine_similarity` across the reservoir, as a 0-100
    /// percentage. This is the true pose-spread metric and MUST be the only
    /// quantity reported as "diversity" on either the capture screen or the
    /// review screen. Reservoir *fill* ([`ProgressSnapshot::coverage_percent`])
    /// is a different quantity: it reaches 100% once the bounded reservoir is
    /// full, regardless of how alike the retained candidates are (USA-0b).
    pub fn diversity_percent(&self) -> u32 {
        if self.reservoir.len() < 2 {
            return 0;
        }
        let mut sum_dist = 0.0_f32;
        let mut count = 0.0_f32;
        for i in 0..self.reservoir.len() {
            for j in (i + 1)..self.reservoir.len() {
                let sim =
                    cosine_similarity(&self.reservoir[i].embedding, &self.reservoir[j].embedding);
                sum_dist += 1.0 - sim;
                count += 1.0;
            }
        }
        if count == 0.0 {
            return 0;
        }
        observation::clamp_percent((sum_dist / count) * 100.0)
    }

    /// Derive the review-screen guidance from the same
    /// [`CaptureObservation`] shape the capture screen uses, built from the
    /// same measurements via [`observation::observation_from`] — the capture
    /// screen from the latest frame, the review screen from the retained
    /// mean. Neither screen fabricates an input any more, so their guidance
    /// can differ only where the underlying frames differ.
    ///
    /// `embedding_spread` carries the measured [`Self::diversity_percent`].
    /// [`observation::derive_guidance`] does not read it today; feeding it
    /// the real value means the ladder can start reading it without silently
    /// diverging the two screens.
    fn review_guidance(&self, diversity_percent: u32) -> String {
        let obs = match self.mean_metrics() {
            Some(m) => observation::observation_from(&m, diversity_percent),
            // Nothing was retained, so there is nothing to have an opinion
            // about: the default observation reports Centering, which is what
            // an empty session should say.
            None => CaptureObservation::default(),
        };
        guidance_to_snake_case(observation::derive_guidance(&obs))
    }

    fn offer(&mut self, candidate: Candidate) {
        if self.reservoir.len() < self.max_reservoir {
            self.accepted += 1;
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
            self.rejected_below_floor += 1;
            let mut dropped = candidate;
            dropped.raw_frame.zeroize();
            dropped.embedding.zeroize();
        } else {
            // Evicting a worse candidate IS an acceptance: the frame made it
            // into the retained set. Counting it as a rejection is what made
            // a 2000-frame session claim 1900 rejected.
            self.accepted += 1;
            let evicted = std::mem::replace(&mut self.reservoir[min_idx], candidate);
            let mut dropped = evicted;
            dropped.raw_frame.zeroize();
            dropped.embedding.zeroize();
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

/// Canonical snake_case wire form of a [`Guidance`] variant. This is the ONLY
/// serialization permitted on the D-Bus surface: it must stay in lockstep with
/// `namtor_client::types::Guidance::from_wire`. Never use `format!("{:?}")` —
/// `Debug` prints `MoveCloser`, which lowercases to the unparseable
/// `movecloser` and leaves the UI displaying a raw token.
pub(crate) fn guidance_to_snake_case(g: Guidance) -> String {
    use Guidance::*;
    match g {
        Centering => "centering",
        MoveCloser => "move_closer",
        MoveBack => "move_back",
        BetterLighting => "better_lighting",
        HoldStill => "hold_still",
        FaceFront => "face_front",
        LookingGood => "looking_good",
        Capturing => "capturing",
        InsufficientPose => "insufficient_pose",
    }
    .into()
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
        let mut s = EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(capture),
            Some(embed),
            100,
        );
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
        let mut s = EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(capture),
            Some(embed),
            100,
        );
        s.pump_until_exhausted().unwrap();
        s.stop().unwrap();
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
        let mut s = EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(capture),
            Some(embed),
            100,
        );
        s.pump().unwrap();
        s.stop().unwrap();
        let finished = s.finish().unwrap();
        assert!(finished.templates.is_empty());
    }

    #[test]
    fn cancel_zeroizes_and_stops_capture() {
        let frames: Vec<Frame> = (0..5).map(|i| grey_frame(100 + i, 8, 8)).collect();
        let capture = Box::new(ReplaySource::new(frames));
        let embed = Box::new(MockEmbedSource::new(8));
        let mut s = EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(capture),
            Some(embed),
            100,
        );
        for _ in 0..3 {
            s.pump().unwrap();
        }
        s.cancel();
        assert_eq!(s.phase(), CapturePhase::Cancelled);
        assert_eq!(s.reservoir_len(), 0);
    }

    #[test]
    fn cancel_is_idempotent() {
        let frames: Vec<Frame> = (0..3).map(|i| grey_frame(100 + i, 8, 8)).collect();
        let capture = Box::new(ReplaySource::new(frames));
        let embed = Box::new(MockEmbedSource::new(8));
        let mut s = EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(capture),
            Some(embed),
            100,
        );
        for _ in 0..2 {
            s.pump().unwrap();
        }
        s.cancel();
        assert_eq!(s.phase(), CapturePhase::Cancelled);
        assert_eq!(s.reservoir_len(), 0);
        s.cancel();
        assert_eq!(s.phase(), CapturePhase::Cancelled);
        assert_eq!(s.reservoir_len(), 0, "second cancel is a no-op");
    }

    #[test]
    fn peer_bus_name_stored_and_accessible() {
        let s = EnrollmentCapture::new(1000, 1000, ":1.99".into(), "alice".into(), None, None, 10);
        assert_eq!(s.peer_bus_name(), ":1.99");
    }

    #[test]
    fn pump_after_cancel_errors() {
        let frames = vec![grey_frame(100, 8, 8)];
        let capture = Box::new(ReplaySource::new(frames));
        let embed = Box::new(MockEmbedSource::new(8));
        let mut s = EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(capture),
            Some(embed),
            100,
        );
        s.cancel();
        assert!(s.pump().is_err());
    }

    #[test]
    fn reservoir_evicts_lowest_quality_when_full() {
        let mut s = EnrollmentCapture::new(1000, 1000, ":1.42".into(), "a".into(), None, None, 3);
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
        let mut s = EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(capture),
            Some(embed),
            10,
        );
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

    /// Embedder that reports "no face detected" (`Ok(None)`) for every second
    /// frame. Exercises the branch that used to drop such frames silently.
    #[derive(Default)]
    struct AlternatingNoFace {
        n: usize,
    }
    impl EmbedSource for AlternatingNoFace {
        fn embed(&mut self, _frame: &Frame) -> Result<Option<(Vec<f32>, FrameMetrics)>, ApiError> {
            self.n += 1;
            if self.n.is_multiple_of(2) {
                Ok(None)
            } else {
                Ok(Some((vec![1.0; 8], MockEmbedSource::nominal_metrics(0.8))))
            }
        }
    }

    fn session_with(
        frames: Vec<Frame>,
        embed: Box<dyn EmbedSource>,
        max_reservoir: usize,
    ) -> EnrollmentCapture {
        EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(Box::new(ReplaySource::new(frames))),
            Some(embed),
            max_reservoir,
        )
    }

    #[test]
    fn accepted_count_keeps_rising_after_the_reservoir_fills() {
        // Ascending luminance => strictly non-decreasing quality, so every
        // frame past the 4th evicts a worse candidate and is genuinely
        // accepted. Before the fix `frames_accepted` was `reservoir_len()`,
        // which freezes at the cap and reports every later frame as rejected.
        let mut s = session_with_frames(12, 4);
        for _ in 0..12 {
            s.pump().unwrap();
        }
        let snap = s.snapshot();
        assert_eq!(snap.frames_captured, 12);
        assert!(
            snap.frames_accepted > 4,
            "accepted froze at the reservoir cap: {}",
            snap.frames_accepted
        );
        assert_eq!(
            snap.frames_captured,
            snap.frames_accepted + snap.frames_rejected,
            "every seen frame is either accepted or rejected for a stated reason"
        );
    }

    #[test]
    fn no_face_frames_are_counted_not_silently_dropped() {
        let frames: Vec<Frame> = (0..10).map(|_| grey_frame(130, 8, 8)).collect();
        let mut s = session_with(frames, Box::new(AlternatingNoFace::default()), 100);
        for _ in 0..10 {
            s.pump().unwrap();
        }
        let snap = s.snapshot();
        assert_eq!(snap.frames_captured, 10);
        assert_eq!(snap.rejected_no_face, 5, "no-face frames must be counted");
        assert_eq!(snap.frames_accepted, 5);
        assert_eq!(snap.frames_rejected, 5);
        assert_eq!(
            snap.frames_captured,
            snap.frames_accepted + snap.frames_rejected
        );
    }

    #[test]
    fn frames_below_the_retained_floor_are_counted_as_such() {
        // Descending luminance away from the ideal => descending quality, so
        // once the reservoir is full nothing beats the worst retained
        // candidate and every later frame is genuinely rejected.
        let frames: Vec<Frame> = (0..10)
            .map(|i| grey_frame(130 - (i as u8 * 10), 8, 8))
            .collect();
        let mut s = session_with(frames, Box::new(MockEmbedSource::new(8)), 3);
        for _ in 0..10 {
            s.pump().unwrap();
        }
        let snap = s.snapshot();
        assert_eq!(snap.frames_captured, 10);
        assert_eq!(
            snap.frames_accepted, 3,
            "only the first three were admitted"
        );
        assert_eq!(snap.rejected_below_floor, 7);
        assert_eq!(snap.rejected_no_face, 0);
        assert_eq!(
            snap.frames_captured,
            snap.frames_accepted + snap.frames_rejected
        );
    }

    #[test]
    fn counters_survive_a_stop_resume_cycle() {
        let mut s = session_with_frames(10, 100);
        for _ in 0..4 {
            s.pump().unwrap();
        }
        s.stop().unwrap();
        let mid = s.snapshot();
        s.resume().unwrap();
        for _ in 0..4 {
            s.pump().unwrap();
        }
        let end = s.snapshot();
        assert_eq!(mid.frames_accepted, 4);
        assert_eq!(
            end.frames_accepted, 8,
            "counters are monotonic across resume"
        );
        assert_eq!(
            end.frames_captured,
            end.frames_accepted + end.frames_rejected
        );
    }

    #[test]
    fn review_reports_the_same_accepted_count_as_progress() {
        let mut s = session_with_frames(12, 4);
        for _ in 0..12 {
            s.pump().unwrap();
        }
        let snap = s.snapshot();
        s.stop().unwrap();
        let review = s.review_snapshot().unwrap();
        assert_eq!(
            review.frames_accepted, snap.frames_accepted,
            "capture screen and review screen disagree under the same label"
        );
    }

    #[test]
    fn pump_without_capture_errors() {
        let mut s = EnrollmentCapture::new(1000, 1000, ":1.42".into(), "a".into(), None, None, 10);
        assert!(matches!(s.pump(), Err(ApiError::State(_))));
    }

    fn session_with_frames(n: u32, max_reservoir: usize) -> EnrollmentCapture {
        let frames: Vec<Frame> = (0..n).map(|i| grey_frame(110 + (i as u8), 8, 8)).collect();
        let capture = Box::new(ReplaySource::new(frames));
        let embed = Box::new(MockEmbedSource::new(8));
        EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(capture),
            Some(embed),
            max_reservoir,
        )
    }

    #[test]
    fn stop_transitions_to_stopped_and_retains_reservoir() {
        let mut s = session_with_frames(5, 100);
        for _ in 0..5 {
            s.pump().unwrap();
        }
        s.stop().unwrap();
        assert_eq!(s.phase(), CapturePhase::Stopped);
        assert_eq!(s.reservoir_len(), 5, "reservoir retained after stop");
    }

    #[test]
    fn stop_is_idempotent() {
        let mut s = session_with_frames(5, 100);
        for _ in 0..3 {
            s.pump().unwrap();
        }
        s.stop().unwrap();
        assert_eq!(s.phase(), CapturePhase::Stopped);
        s.stop().unwrap();
        assert_eq!(s.phase(), CapturePhase::Stopped);
    }

    #[test]
    fn stop_from_non_capturing_fails() {
        let mut s = session_with_frames(5, 100);
        for _ in 0..3 {
            s.pump().unwrap();
        }
        s.stop().unwrap();
        s.finish().unwrap();
        let err = s.stop().unwrap_err();
        assert!(matches!(err, ApiError::State(_)));
        assert_eq!(s.phase(), CapturePhase::Finished, "state unchanged");
    }

    #[test]
    fn resume_transitions_to_capturing() {
        let mut s = session_with_frames(10, 100);
        for _ in 0..3 {
            s.pump().unwrap();
        }
        s.stop().unwrap();
        s.resume().unwrap();
        assert_eq!(s.phase(), CapturePhase::Capturing);
        assert_eq!(s.reservoir_len(), 3, "reservoir preserved across resume");
    }

    #[test]
    fn resume_from_non_stopped_fails() {
        let mut s = session_with_frames(5, 100);
        s.pump().unwrap();
        let err = s.resume().unwrap_err();
        assert!(matches!(err, ApiError::State(_)));
        assert_eq!(s.phase(), CapturePhase::Capturing, "state unchanged");

        s.stop().unwrap();
        s.finish().unwrap();
        let err = s.resume().unwrap_err();
        assert!(matches!(err, ApiError::State(_)));
    }

    #[test]
    fn review_snapshot_in_stopped_returns_summary() {
        let mut s = session_with_frames(5, 100);
        for _ in 0..5 {
            s.pump().unwrap();
        }
        s.stop().unwrap();
        let review = s.review_snapshot().unwrap();
        assert_eq!(review.frames_accepted, 5);
        assert!(review.ready_to_save, "5 >= MIN_CANDIDATES(3)");
        assert!(review.quality_percent <= 100);
        assert!(review.diversity_percent <= 100);
        assert!(!review.guidance.is_empty());
    }

    #[test]
    fn review_snapshot_in_non_stopped_fails() {
        let mut s = session_with_frames(5, 100);
        s.pump().unwrap();
        let err = s.review_snapshot().unwrap_err();
        assert!(matches!(err, ApiError::State(_)));
    }

    #[test]
    fn finish_from_stopped_commits_and_zeroizes() {
        let mut s = session_with_frames(10, 100);
        s.pump_until_exhausted().unwrap();
        s.stop().unwrap();
        let finished = s.finish().unwrap();
        assert!(!finished.templates.is_empty());
        assert_eq!(s.phase(), CapturePhase::Finished);
        assert_eq!(s.reservoir_len(), 0, "reservoir zeroized after finish");
    }

    #[test]
    fn finish_from_capturing_fails() {
        let mut s = session_with_frames(10, 100);
        s.pump_until_exhausted().unwrap();
        let err = s.finish().unwrap_err();
        assert!(matches!(err, ApiError::State(_)));
        assert_eq!(s.phase(), CapturePhase::Capturing, "state unchanged");
        assert!(
            s.reservoir_len() > 0,
            "reservoir intact after failed finish"
        );
    }

    #[test]
    fn cancel_from_stopped_zeroizes() {
        let mut s = session_with_frames(5, 100);
        for _ in 0..5 {
            s.pump().unwrap();
        }
        s.stop().unwrap();
        assert_eq!(s.reservoir_len(), 5);
        s.cancel();
        assert_eq!(s.phase(), CapturePhase::Cancelled);
        assert_eq!(
            s.reservoir_len(),
            0,
            "retained reservoir zeroized on cancel"
        );
    }

    #[test]
    fn cancel_from_finished_transitions_to_cancelled() {
        let mut s = session_with_frames(10, 100);
        s.pump_until_exhausted().unwrap();
        s.stop().unwrap();
        s.finish().unwrap();
        assert_eq!(s.phase(), CapturePhase::Finished);
        assert_eq!(s.reservoir_len(), 0);
        s.cancel();
        assert_eq!(s.phase(), CapturePhase::Cancelled);
        assert_eq!(s.reservoir_len(), 0, "still zeroized");
    }

    #[test]
    fn resume_preserves_reservoir_across_stop_resume_cycle() {
        let mut s = session_with_frames(10, 100);
        for _ in 0..5 {
            s.pump().unwrap();
        }
        s.stop().unwrap();
        s.resume().unwrap();
        for _ in 0..3 {
            s.pump().unwrap();
        }
        s.stop().unwrap();
        assert_eq!(s.reservoir_len(), 8, "reservoir accumulated across cycle");
        let finished = s.finish().unwrap();
        assert!(
            !finished.templates.is_empty(),
            "templates reflect 8 candidates"
        );
    }

    #[test]
    fn full_lifecycle_capture_stop_review_resume_stop_commit() {
        let mut s = session_with_frames(15, 100);
        for _ in 0..10 {
            s.pump().unwrap();
        }
        s.stop().unwrap();
        let review = s.review_snapshot().unwrap();
        assert!(review.ready_to_save, "ready after 10 frames");
        s.resume().unwrap();
        for _ in 0..5 {
            s.pump().unwrap();
        }
        s.stop().unwrap();
        let finished = s.finish().unwrap();
        assert!(
            !finished.templates.is_empty(),
            "one durable profile with templates"
        );
        assert_eq!(s.phase(), CapturePhase::Finished);
        assert_eq!(s.reservoir_len(), 0);
    }

    #[test]
    fn commit_while_capturing_fails_without_persistence() {
        let mut s = session_with_frames(10, 100);
        s.pump_until_exhausted().unwrap();
        let err = s.finish().unwrap_err();
        assert!(matches!(err, ApiError::State(_)));
        assert_eq!(s.phase(), CapturePhase::Capturing, "no phase change");
        assert!(s.reservoir_len() > 0, "reservoir intact, no persistence");
    }

    #[test]
    fn resume_after_cancel_fails() {
        let mut s = session_with_frames(5, 100);
        for _ in 0..5 {
            s.pump().unwrap();
        }
        s.cancel();
        assert_eq!(s.phase(), CapturePhase::Cancelled);
        let err = s.resume().unwrap_err();
        assert!(matches!(err, ApiError::State(_)));
        assert_eq!(s.phase(), CapturePhase::Cancelled, "still cancelled");
        assert_eq!(s.reservoir_len(), 0, "no retained buffer");
    }
}
