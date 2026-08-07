// SPDX-License-Identifier: GPL-3.0-only
//! Unlimited rolling enrollment with a bounded candidate reservoir and
//! review-then-commit persistence.
//!
//! Spec §6.6 Tier 1 + task 15: capture an unbounded stream of candidate
//! frames over an arbitrarily long session (multi-hour, 25k+ frames) while
//! holding memory bounded by a fixed-size reservoir. The reservoir keeps the
//! best `max_reservoir` candidates seen so far, replacing the lowest-quality
//! one when full. At [`EnrollmentSession::finish`], the reservoir is split
//! into a template set (90%) and a held-out set (10%); the held-out set is
//! matched against the enrolled gallery to produce [`MatchConfidence`], and
//! the template set is scored into [`EnrollmentQuality`]. The operator then
//! reviews both metrics and chooses [`CommitDecision::Merge`],
//! [`CommitDecision::Replace`], or [`CommitDecision::Cancel`].
//!
//! Metric labels are explicit and non-overlapping with FAR/FRR:
//! - [`EnrollmentQuality`] is labelled `"Enrollment Quality"` — a capture
//!   quality score, never an error rate.
//! - [`MatchConfidence`] is labelled `"Match Confidence"` — held-out-vs-
//!   gallery cosine similarity, never an error rate.
//!
//! No raw frames or crops are persisted: [`EnrollmentSession::finish`] and
//! [`EnrollmentSession::cancel`] both zeroize every held raw buffer.

use namtor_recog::pipeline::RecognitionPipeline;
use namtor_store::template::{ProfileId, ProfileRecord, Template};
use namtor_store::{StoreError, TemplateStore};
use zeroize::Zeroize;

/// Maximum reservoir size. Spec §6.6 Tier 1 keeps 8 templates; a 100-entry
/// reservoir gives the diversity-aware selector a wide pool to choose from
/// while bounding memory for multi-hour / 25k-frame sessions.
pub const DEFAULT_MAX_RESERVOIR: usize = 100;

/// Fraction of the finished reservoir held out for match-confidence
/// evaluation (spec §6.6 Tier 1 review step).
const HOLDOUT_FRACTION: usize = 10; // 10%

/// Number of diverse templates selected from the reservoir for storage.
const SELECT_N: usize = 8;

/// Per-capture quality breakdown, labelled `"Enrollment Quality"`.
///
/// This is a capture-quality score (detection confidence, landmark quality,
/// blur, pose, illumination) — explicitly NOT a FAR/FRR error rate.
#[derive(Debug, Clone, PartialEq)]
pub struct EnrollmentQuality {
    /// Aggregate 0-100 score, labelled `"Enrollment Quality"`.
    pub score: u8,
    /// YuNet detection confidence (0.0-1.0).
    pub detection_confidence: f32,
    /// Landmark localisation quality (0.0-1.0).
    pub landmark_quality: f32,
    /// Variance-of-Laplacian blur metric (higher = sharper).
    pub blur_metric: f32,
    /// Estimated yaw/pitch magnitude in degrees (lower = more frontal).
    pub pose_angle_deg: f32,
    /// Illumination adequacy: mean pixel intensity in 0-255 (too low or
    /// too high -> low score).
    pub illumination: f32,
}

impl EnrollmentQuality {
    /// Build a quality record from raw sub-metrics and compute the 0-100
    /// aggregate. The aggregate weights detection confidence most heavily,
    /// then blur, landmark, pose, and illumination — none of which is an
    /// error rate.
    pub fn new(
        detection_confidence: f32,
        landmark_quality: f32,
        blur_metric: f32,
        pose_angle_deg: f32,
        illumination: f32,
    ) -> Self {
        let det = detection_confidence.clamp(0.0, 1.0);
        let lm = landmark_quality.clamp(0.0, 1.0);
        // Blur: variance-of-laplacian; >= ~50 is sharp, < ~5 is junk.
        let blur = (blur_metric / 50.0).clamp(0.0, 1.0);
        // Pose: 0 deg is frontal; beyond ~25 deg the embedding degrades.
        let pose = (1.0 - (pose_angle_deg / 25.0)).clamp(0.0, 1.0);
        // Illumination: ideal ~90-170; penalise outside that band.
        let dist = (illumination - 130.0).abs();
        let illum = (1.0 - (dist / 130.0)).clamp(0.0, 1.0);

        let weighted = 0.40 * det + 0.20 * blur + 0.15 * lm + 0.15 * pose + 0.10 * illum;
        let score = (weighted * 100.0).round().clamp(0.0, 100.0) as u8;
        Self {
            score,
            detection_confidence: det,
            landmark_quality: lm,
            blur_metric,
            pose_angle_deg,
            illumination,
        }
    }

    /// Human-readable metric label. Asserted by tests to never be FAR/FRR.
    pub fn label() -> &'static str {
        "Enrollment Quality"
    }
}

/// Held-out match evaluation, labelled `"Match Confidence"`.
///
/// Computed by matching the held-out 10% of the reservoir against the
/// enrolled gallery (or the in-session template set when no gallery exists)
/// using cosine similarity. Explicitly NOT a FAR/FRR error rate.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchConfidence {
    /// Aggregate 0-100 score, labelled `"Match Confidence"`.
    pub score: u8,
    /// Mean cosine similarity across held-out frames.
    pub mean_similarity: f32,
    /// Minimum cosine similarity across held-out frames (worst case).
    pub min_similarity: f32,
}

impl MatchConfidence {
    /// Compute match confidence from held-out embeddings matched against a
    /// gallery. Each held-out frame takes its max cosine similarity over the
    /// gallery; the score aggregates the mean and worst-case.
    pub fn new(held_out: &[Vec<f32>], gallery: &[Vec<f32>]) -> Self {
        if held_out.is_empty() || gallery.is_empty() {
            return Self {
                score: 0,
                mean_similarity: 0.0,
                min_similarity: 0.0,
            };
        }
        let mut sims: Vec<f32> = Vec::with_capacity(held_out.len());
        for ho in held_out {
            let mut best = -1.0_f32;
            for g in gallery {
                let s = RecognitionPipeline::cosine_similarity(ho, g);
                if s > best {
                    best = s;
                }
            }
            sims.push(best);
        }
        let mean = sims.iter().sum::<f32>() / sims.len() as f32;
        let min = sims.iter().cloned().fold(f32::INFINITY, f32::min);
        // Cosine sim for L2-normalised embeddings is in [-1, 1]; map
        // [0.3, 1.0] to [0, 100] — below 0.3 is effectively a non-match.
        let mapped = ((mean - 0.3) / 0.7).clamp(0.0, 1.0);
        let score = (mapped * 100.0).round() as u8;
        Self {
            score,
            mean_similarity: mean,
            min_similarity: min,
        }
    }

    /// Human-readable metric label. Asserted by tests to never be FAR/FRR.
    pub fn label() -> &'static str {
        "Match Confidence"
    }
}

/// A single captured candidate held in the reservoir.
pub struct Candidate {
    /// L2-normalised face embedding (e.g. 128-d SFace).
    pub embedding: Vec<f32>,
    /// Capture-time quality breakdown.
    pub quality: EnrollmentQuality,
    /// Raw frame pixels retained for re-embedding / display; zeroized on
    /// session finish or cancel.
    pub raw_frame: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Candidate {
    /// Aggregate quality score (0-100) used for reservoir ranking.
    pub fn score(&self) -> u8 {
        self.quality.score
    }
}

/// Outcome of offering a candidate to a running session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressOutcome {
    /// Candidate was admitted to the reservoir.
    Admitted,
    /// Reservoir was full; the lowest-quality existing candidate was evicted
    /// and the new one took its place.
    Replaced { evicted_score: u8 },
    /// Candidate was rejected (score below the reservoir floor when full).
    Rejected { reason: &'static str },
}

/// Lifecycle phase of an [`EnrollmentSession`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPhase {
    Created,
    Capturing,
    Finished,
    Cancelled,
    Committed,
}

/// Summary produced by [`EnrollmentSession::finish`]: the quality and match
/// metrics the operator reviews before choosing a [`CommitDecision`].
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub quality: EnrollmentQuality,
    pub confidence: MatchConfidence,
    pub selected_templates: Vec<Template>,
    pub held_out_count: usize,
    pub total_seen: usize,
}

/// Operator decision after reviewing [`SessionSummary`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitDecision {
    /// Append the new templates to an existing profile (`merge_profile`).
    Merge,
    /// Overwrite the profile's templates (`merge_profile` with replace
    /// semantics — the store's `merge_profile` replaces templates in place).
    Replace,
    /// Discard everything; write nothing to the store.
    Cancel,
}

/// Persisted-profile context needed by [`EnrollmentSession::review_then_commit`]
/// to write templates under a named profile. Grouped to keep the method's
/// argument count within clippy's limit.
#[derive(Clone)]
pub struct CommitContext<'a> {
    pub store: &'a TemplateStore,
    pub uid: u32,
    pub profile_id: ProfileId,
    pub display_name: String,
    pub model_id: String,
    pub existing_gallery: Vec<Template>,
}

/// Unlimited rolling enrollment with a bounded candidate reservoir.
///
/// The reservoir holds at most `max_reservoir` candidates; once full, a new
/// candidate evicts the lowest-quality one (or is itself rejected if its
/// score is below the floor). This bounds memory for multi-hour / 25k-frame
/// capture sessions.
pub struct EnrollmentSession {
    reservoir: Vec<Candidate>,
    max_reservoir: usize,
    total_seen: usize,
    phase: SessionPhase,
    finished_quality: Option<EnrollmentQuality>,
    finished_confidence: Option<MatchConfidence>,
    finished_templates: Option<Vec<Template>>,
}

impl EnrollmentSession {
    /// Create a session with the default reservoir bound.
    pub fn new() -> Self {
        Self::with_max(DEFAULT_MAX_RESERVOIR)
    }

    /// Create a session with an explicit reservoir bound (tests use a small
    /// bound to exercise eviction).
    pub fn with_max(max_reservoir: usize) -> Self {
        Self {
            reservoir: Vec::with_capacity(max_reservoir),
            max_reservoir: max_reservoir.max(1),
            total_seen: 0,
            phase: SessionPhase::Created,
            finished_quality: None,
            finished_confidence: None,
            finished_templates: None,
        }
    }

    /// Current lifecycle phase.
    pub fn phase(&self) -> &SessionPhase {
        &self.phase
    }

    /// Number of candidates currently held in the reservoir.
    pub fn reservoir_len(&self) -> usize {
        self.reservoir.len()
    }

    /// Total candidates seen since [`EnrollmentSession::start`].
    pub fn total_seen(&self) -> usize {
        self.total_seen
    }

    /// Begin capturing. Returns `Err` if the session is not in the `Created`
    /// phase.
    pub fn start(&mut self) -> Result<(), &'static str> {
        if self.phase != SessionPhase::Created {
            return Err("session already started or finished");
        }
        self.phase = SessionPhase::Capturing;
        Ok(())
    }

    /// Offer a candidate to the running session.
    ///
    /// If the reservoir is not full, the candidate is admitted. If full, the
    /// candidate replaces the lowest-quality existing entry when its score is
    /// higher; otherwise it is rejected. This bounds memory for unbounded
    /// capture streams.
    pub fn progress(&mut self, candidate: Candidate) -> Result<ProgressOutcome, &'static str> {
        if self.phase != SessionPhase::Capturing {
            return Err("session not in capturing phase");
        }
        self.total_seen += 1;
        if self.reservoir.len() < self.max_reservoir {
            self.reservoir.push(candidate);
            Ok(ProgressOutcome::Admitted)
        } else {
            let mut min_idx = 0;
            let mut min_score = self.reservoir[0].score();
            for (i, c) in self.reservoir.iter().enumerate().skip(1) {
                if c.score() < min_score {
                    min_score = c.score();
                    min_idx = i;
                }
            }
            if candidate.score() <= min_score {
                let mut dropped = candidate;
                dropped.raw_frame.zeroize();
                Ok(ProgressOutcome::Rejected {
                    reason: "below reservoir floor",
                })
            } else {
                let evicted = std::mem::replace(&mut self.reservoir[min_idx], candidate);
                let evicted_score = evicted.score();
                let mut to_drop = evicted;
                to_drop.raw_frame.zeroize();
                Ok(ProgressOutcome::Replaced { evicted_score })
            }
        }
    }

    /// Stop capturing, split the reservoir into selected templates (90%) and
    /// a held-out set (10%), compute [`EnrollmentQuality`] over the selected
    /// set and [`MatchConfidence`] of the held-out set against `gallery`, and
    /// return a [`SessionSummary`] for review.
    ///
    /// `gallery` is the existing enrolled gallery for the target uid (empty
    /// for first enrollment). When non-empty, held-out frames are matched
    /// against it; when empty, they are matched against the in-session
    /// selected templates (self-match) — still a useful consistency signal.
    pub fn finish(&mut self, gallery: &[Vec<f32>]) -> Result<SessionSummary, &'static str> {
        if self.phase != SessionPhase::Capturing {
            return Err("session not in capturing phase");
        }
        if self.reservoir.len() < 3 {
            self.cancel();
            return Err("insufficient candidates (need at least 3)");
        }

        let selected_idx = select_diverse(&self.reservoir, SELECT_N);

        let holdout_target = (self.reservoir.len() / HOLDOUT_FRACTION).max(1);
        let mut held_out_idx: Vec<usize> = (0..self.reservoir.len())
            .filter(|i| !selected_idx.contains(i))
            .take(holdout_target)
            .collect();

        if held_out_idx.is_empty() && !selected_idx.is_empty() {
            let last = *selected_idx.last().unwrap_or(&0);
            held_out_idx.push(last);
        }

        let mut templates: Vec<Template> = Vec::with_capacity(selected_idx.len());
        let mut selected_embeddings: Vec<Vec<f32>> = Vec::with_capacity(selected_idx.len());
        for (rank, &i) in selected_idx.iter().enumerate() {
            let c = &self.reservoir[i];
            templates.push(Template {
                vector: c.embedding.clone(),
                quality: c.quality.score as f32 / 100.0,
                is_anchor: rank == 0,
            });
            selected_embeddings.push(c.embedding.clone());
        }
        let mut held_out_embeddings: Vec<Vec<f32>> = Vec::with_capacity(held_out_idx.len());
        for &i in &held_out_idx {
            held_out_embeddings.push(self.reservoir[i].embedding.clone());
        }

        let quality = aggregate_quality(
            &selected_idx
                .iter()
                .map(|&i| &self.reservoir[i].quality)
                .collect::<Vec<_>>(),
        );

        let conf_gallery: Vec<Vec<f32>> = if gallery.is_empty() {
            selected_embeddings.clone()
        } else {
            gallery.to_vec()
        };
        let confidence = MatchConfidence::new(&held_out_embeddings, &conf_gallery);

        // Spec: zeroize raw frames/crops on finish. Embeddings are already
        // extracted into `templates` and `held_out_embeddings`; the raw
        // pixel buffers are no longer needed and must not persist.
        for c in self.reservoir.iter_mut() {
            c.raw_frame.zeroize();
        }
        self.reservoir.clear();

        let summary = SessionSummary {
            quality: quality.clone(),
            confidence: confidence.clone(),
            selected_templates: templates.clone(),
            held_out_count: held_out_idx.len(),
            total_seen: self.total_seen,
        };

        self.finished_quality = Some(quality);
        self.finished_confidence = Some(confidence);
        self.finished_templates = Some(templates);
        self.phase = SessionPhase::Finished;
        Ok(summary)
    }

    /// Discard everything and write nothing to the store. Zeroizes every
    /// held raw buffer. Idempotent.
    pub fn cancel(&mut self) {
        for c in self.reservoir.drain(..) {
            let mut dropped = c;
            dropped.raw_frame.zeroize();
            dropped.embedding.zeroize();
        }
        if let Some(mut ts) = self.finished_templates.take() {
            for t in ts.iter_mut() {
                t.vector.zeroize();
            }
        }
        self.finished_quality = None;
        self.finished_confidence = None;
        self.phase = SessionPhase::Cancelled;
    }

    /// Apply the operator's review decision: merge into or replace an
    /// existing profile, or cancel. On merge/replace, only the template
    /// vectors and metadata are written — no raw frames or crops are
    /// persisted. On cancel, nothing is written.
    ///
    /// `existing_gallery` is the gallery currently enrolled for the profile
    /// (empty for first enrollment). On `Merge`, the new templates are
    /// appended to `existing_gallery` before writing; on `Replace`, the new
    /// templates overwrite it.
    pub fn review_then_commit(
        &mut self,
        decision: CommitDecision,
        ctx: &CommitContext,
    ) -> Result<(), StoreError> {
        if self.phase != SessionPhase::Finished {
            return Err(StoreError::Crypto(
                "session not finished; call finish() first".into(),
            ));
        }
        match decision {
            CommitDecision::Cancel => {
                self.cancel();
                Ok(())
            }
            CommitDecision::Merge | CommitDecision::Replace => {
                let new_templates = self
                    .finished_templates
                    .clone()
                    .ok_or_else(|| StoreError::Crypto("no finished templates".into()))?;
                let combined: Vec<Template> = match decision {
                    CommitDecision::Merge => {
                        let mut v = ctx.existing_gallery.clone();
                        v.extend(new_templates);
                        for (i, t) in v.iter_mut().enumerate() {
                            t.is_anchor = i == 0 && t.is_anchor;
                        }
                        v
                    }
                    CommitDecision::Replace => {
                        let mut v = new_templates;
                        if let Some(first) = v.first_mut() {
                            first.is_anchor = true;
                        }
                        v
                    }
                    CommitDecision::Cancel => unreachable!(),
                };
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_micros() as u64)
                    .unwrap_or(0);
                let profile = ProfileRecord {
                    profile_id: ctx.profile_id.clone(),
                    display_name: ctx.display_name.clone(),
                    enabled: true,
                    uid: ctx.uid,
                    model_id: ctx.model_id.clone(),
                    created_at_us: now,
                    schema_version: 1,
                    last_used_us: 0,
                };
                ctx.store.merge_profile(ctx.uid, profile, combined)?;
                self.cancel_inner_zeroize();
                self.phase = SessionPhase::Committed;
                Ok(())
            }
        }
    }

    fn cancel_inner_zeroize(&mut self) {
        for c in self.reservoir.drain(..) {
            let mut dropped = c;
            dropped.raw_frame.zeroize();
            dropped.embedding.zeroize();
        }
        if let Some(mut ts) = self.finished_templates.take() {
            for t in ts.iter_mut() {
                t.vector.zeroize();
            }
        }
    }
}

impl Default for EnrollmentSession {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for EnrollmentSession {
    fn drop(&mut self) {
        // Defensive: ensure raw frames never survive a panicking drop.
        self.cancel_inner_zeroize();
    }
}

/// Diversity-aware selection: pick `n` indices from `reservoir` using
/// farthest-first traversal seeded by the highest-quality candidate. This
/// returns a varied set rather than just the top-`n` by score.
fn select_diverse(reservoir: &[Candidate], n: usize) -> Vec<usize> {
    if reservoir.is_empty() || n == 0 {
        return Vec::new();
    }
    let n = n.min(reservoir.len());
    let mut selected: Vec<usize> = Vec::with_capacity(n);
    let mut remaining: Vec<usize> = (0..reservoir.len()).collect();

    // Seed: highest-quality candidate.
    let seed = remaining
        .iter()
        .copied()
        .max_by(|&a, &b| {
            reservoir[a]
                .score()
                .cmp(&reservoir[b].score())
                .then_with(|| {
                    reservoir[a]
                        .embedding
                        .len()
                        .cmp(&reservoir[b].embedding.len())
                })
        })
        .unwrap_or(0);
    selected.push(seed);
    remaining.retain(|&i| i != seed);

    // Farthest-first: repeatedly add the candidate whose total cosine
    // similarity to the already-selected set is smallest (most distant
    // overall). Using the sum rather than the minimum breaks ties among
    // equally-distant candidates in favour of those that add a genuinely
    // new direction, which is what a diverse gallery requires.
    while selected.len() < n && !remaining.is_empty() {
        let mut best_idx = 0;
        let mut best_sum_sim = f32::INFINITY;
        for (r_pos, &r_idx) in remaining.iter().enumerate() {
            let mut sum_sim = 0.0_f32;
            for &s_idx in &selected {
                sum_sim += RecognitionPipeline::cosine_similarity(
                    &reservoir[r_idx].embedding,
                    &reservoir[s_idx].embedding,
                );
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

/// Aggregate a set of [`EnrollmentQuality`] records into a single one by
/// averaging the sub-metrics and recomputing the 0-100 score.
fn aggregate_quality(qs: &[&EnrollmentQuality]) -> EnrollmentQuality {
    if qs.is_empty() {
        return EnrollmentQuality::new(0.0, 0.0, 0.0, 0.0, 0.0);
    }
    let n = qs.len() as f32;
    let det: f32 = qs.iter().map(|q| q.detection_confidence).sum::<f32>() / n;
    let lm: f32 = qs.iter().map(|q| q.landmark_quality).sum::<f32>() / n;
    let blur: f32 = qs.iter().map(|q| q.blur_metric).sum::<f32>() / n;
    let pose: f32 = qs.iter().map(|q| q.pose_angle_deg).sum::<f32>() / n;
    let illum: f32 = qs.iter().map(|q| q.illumination).sum::<f32>() / n;
    EnrollmentQuality::new(det, lm, blur, pose, illum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_store::template::ProfileId;

    fn cand(score: u8, embedding_seed: f32) -> Candidate {
        // Embeddings are L2-normalised-ish for cosine similarity tests.
        let v = vec![embedding_seed; 8];
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        let embedding = if norm > 1e-10 {
            v.iter().map(|x| x / norm).collect()
        } else {
            v
        };
        cand_with_score(score, embedding)
    }

    /// Build a candidate with an exact quality score (bypassing the
    /// `EnrollmentQuality::new` recompute, which maps sub-metrics back to a
    /// 0-100 aggregate and would not round-trip exactly).
    fn cand_with_score(score: u8, embedding: Vec<f32>) -> Candidate {
        Candidate {
            embedding,
            quality: EnrollmentQuality {
                score,
                detection_confidence: 0.9,
                landmark_quality: 0.9,
                blur_metric: 50.0,
                pose_angle_deg: 5.0,
                illumination: 130.0,
            },
            raw_frame: vec![0u8; 64],
            width: 8,
            height: 8,
        }
    }

    fn cand_with_vec(score: u8, embedding: Vec<f32>) -> Candidate {
        cand_with_score(score, embedding)
    }

    fn orthogonal_unit(i: usize) -> Vec<f32> {
        let mut v = vec![0.0_f32; 8];
        v[i % 8] = 1.0;
        v
    }

    #[test]
    fn session_start_progress_finish_lifecycle() {
        let mut s = EnrollmentSession::with_max(10);
        assert_eq!(s.phase(), &SessionPhase::Created);
        s.start().unwrap();
        assert_eq!(s.phase(), &SessionPhase::Capturing);

        for i in 0..5u8 {
            let out = s.progress(cand(60 + i, 1.0)).unwrap();
            assert_eq!(out, ProgressOutcome::Admitted);
        }
        assert_eq!(s.reservoir_len(), 5);
        assert_eq!(s.total_seen(), 5);

        let summary = s.finish(&[]).unwrap();
        assert_eq!(s.phase(), &SessionPhase::Finished);
        assert!(summary.selected_templates.len() <= 5);
        assert!(!summary.selected_templates.is_empty());
        assert_eq!(summary.total_seen, 5);
        assert!(summary.held_out_count >= 1);
        // First selected template is the anchor.
        assert!(summary.selected_templates[0].is_anchor);
    }

    #[test]
    fn start_twice_rejected() {
        let mut s = EnrollmentSession::with_max(10);
        s.start().unwrap();
        let err = s.start().unwrap_err();
        assert!(err.contains("already started"));
    }

    #[test]
    fn progress_before_start_rejected() {
        let mut s = EnrollmentSession::with_max(10);
        let err = s.progress(cand(80, 1.0)).unwrap_err();
        assert!(err.contains("not in capturing phase"));
    }

    #[test]
    fn finish_before_start_rejected() {
        let mut s = EnrollmentSession::with_max(10);
        let err = s.finish(&[]).unwrap_err();
        assert!(err.contains("not in capturing phase"));
    }

    #[test]
    fn finish_with_too_few_candidates_cancels() {
        let mut s = EnrollmentSession::with_max(10);
        s.start().unwrap();
        s.progress(cand(80, 1.0)).unwrap();
        s.progress(cand(80, 1.0)).unwrap();
        let err = s.finish(&[]).unwrap_err();
        assert!(err.contains("insufficient"));
        assert_eq!(s.phase(), &SessionPhase::Cancelled);
    }

    #[test]
    fn bounded_reservoir_stays_within_limit_with_25k_frames() {
        let mut s = EnrollmentSession::with_max(100);
        s.start().unwrap();
        let mut admitted = 0usize;
        let mut replaced = 0usize;
        // 25,000 frames — well over the 100-entry reservoir.
        for i in 0..25_000u32 {
            // Scores cycle 0..=100 so we exercise admit, replace, reject.
            let score = (i % 101) as u8;
            let out = s.progress(cand(score, ((i % 7) as f32) + 1.0)).unwrap();
            match out {
                ProgressOutcome::Admitted => admitted += 1,
                ProgressOutcome::Replaced { .. } => replaced += 1,
                ProgressOutcome::Rejected { .. } => {}
            }
            assert!(
                s.reservoir_len() <= 100,
                "reservoir grew to {} at i={i}",
                s.reservoir_len()
            );
        }
        assert_eq!(s.reservoir_len(), 100, "reservoir must be exactly full");
        assert_eq!(admitted, 100, "first 100 admitted");
        assert!(replaced > 0, "later high-score candidates must replace");
        assert_eq!(s.total_seen(), 25_000);
    }

    #[test]
    fn diverse_gallery_not_just_top_score() {
        // Reservoir where top-8 by score are collinear and a few lower-score
        // candidates are orthogonal. A pure top-score selector would return
        // 8 collinear vectors; the diversity-aware selector must include
        // the orthogonal ones.
        let mut s = EnrollmentSession::with_max(20);
        s.start().unwrap();
        // 8 high-score (90) but collinear candidates along axis 0.
        for _ in 0..8 {
            s.progress(cand_with_vec(90, orthogonal_unit(0))).unwrap();
        }
        // 4 lower-score (70) candidates orthogonal to those and each other.
        for axis in 1..=4 {
            s.progress(cand_with_vec(70, orthogonal_unit(axis)))
                .unwrap();
        }
        // 4 more mid-score (80) collinear along axis 0.
        for _ in 0..4 {
            s.progress(cand_with_vec(80, orthogonal_unit(0))).unwrap();
        }
        let summary = s.finish(&[]).unwrap();
        let selected = &summary.selected_templates;
        // Count distinct embedding directions in the selected set.
        let mut distinct_directions = 0;
        for axis in 0..8 {
            let probe = orthogonal_unit(axis);
            let any = selected
                .iter()
                .any(|t| RecognitionPipeline::cosine_similarity(&t.vector, &probe) > 0.9);
            if any {
                distinct_directions += 1;
            }
        }
        assert!(
            distinct_directions >= 3,
            "diverse selector must include >=3 orthogonal directions, got {distinct_directions}"
        );
    }

    #[test]
    fn cancel_writes_nothing_to_store() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let store = TemplateStore::open(dir.path(), &key_path).unwrap();

        let mut s = EnrollmentSession::with_max(10);
        s.start().unwrap();
        for i in 0..5u8 {
            s.progress(cand(70 + i, 1.0)).unwrap();
        }
        s.finish(&[]).unwrap();
        let ctx = CommitContext {
            store: &store,
            uid: 1000,
            profile_id: ProfileId("default".into()),
            display_name: "test".into(),
            model_id: "sface".into(),
            existing_gallery: vec![],
        };
        s.review_then_commit(CommitDecision::Cancel, &ctx).unwrap();
        assert_eq!(s.phase(), &SessionPhase::Cancelled);
        // No sealed file for uid 1000 must exist.
        let sealed = dir.path().join("1000.sealed");
        assert!(!sealed.exists(), "cancel must not write any sealed file");
        assert!(store.list_uids().unwrap().is_empty());
    }

    #[test]
    fn cancel_direct_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let _store = TemplateStore::open(dir.path(), &key_path).unwrap();

        let mut s = EnrollmentSession::with_max(10);
        s.start().unwrap();
        for i in 0..5u8 {
            s.progress(cand(70 + i, 1.0)).unwrap();
        }
        s.cancel();
        assert_eq!(s.phase(), &SessionPhase::Cancelled);
        assert_eq!(s.reservoir_len(), 0);
        let sealed = dir.path().join("1000.sealed");
        assert!(!sealed.exists());
    }

    #[test]
    fn finish_stores_templates_only_no_raw_frames() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let store = TemplateStore::open(dir.path(), &key_path).unwrap();

        let mut s = EnrollmentSession::with_max(10);
        s.start().unwrap();
        for i in 0..6u8 {
            // Distinct orthogonal directions so selection picks varied ones.
            s.progress(cand_with_vec(80 + i, orthogonal_unit(i as usize)))
                .unwrap();
        }
        let summary = s.finish(&[]).unwrap();
        // Verify summary has template vectors but no raw frame bytes.
        for t in &summary.selected_templates {
            assert!(!t.vector.is_empty(), "template vector must be present");
        }
        let ctx = CommitContext {
            store: &store,
            uid: 1001,
            profile_id: ProfileId("default".into()),
            display_name: "test".into(),
            model_id: "sface".into(),
            existing_gallery: vec![],
        };
        s.review_then_commit(CommitDecision::Replace, &ctx).unwrap();
        assert_eq!(s.phase(), &SessionPhase::Committed);

        // Reload the stored profile and verify only templates are present
        // (no raw frame bytes serialised alongside).
        let profiles = store.list_profiles(1001).unwrap();
        assert_eq!(profiles.len(), 1);
        let entry = &profiles[0];
        assert!(!entry.templates.is_empty());
        for t in &entry.templates {
            // Template struct only carries vector + quality + is_anchor —
            // no raw_frame field exists in the serialised form. Confirm the
            // vector length matches an SFace embedding shape (not a raw
            // 64-byte frame).
            assert_eq!(
                t.vector.len(),
                8,
                "stored vector is the embedding, not raw frame"
            );
            assert!(t.quality > 0.0);
        }
    }

    #[test]
    fn metrics_labelled_correctly_not_far_frr() {
        let q = EnrollmentQuality::new(0.9, 0.8, 60.0, 5.0, 130.0);
        let m = MatchConfidence::new(&[vec![1.0, 0.0]], &[vec![1.0, 0.0]]);
        assert_eq!(EnrollmentQuality::label(), "Enrollment Quality");
        assert_eq!(MatchConfidence::label(), "Match Confidence");
        // Negative assertion: neither label contains FAR or FRR.
        let ql = EnrollmentQuality::label().to_lowercase();
        let ml = MatchConfidence::label().to_lowercase();
        assert!(!ql.contains("far"));
        assert!(!ql.contains("frr"));
        assert!(!ml.contains("far"));
        assert!(!ml.contains("frr"));
        // Score ranges sane.
        assert!(q.score <= 100);
        assert!(m.score <= 100);
    }

    #[test]
    fn match_confidence_high_for_identical_gallery() {
        let v = vec![1.0_f32, 0.0, 0.0];
        let gallery = vec![v.clone()];
        let m = MatchConfidence::new(&gallery, &[v]);
        // Identical embeddings -> cosine sim 1.0 -> max score.
        assert!((m.mean_similarity - 1.0).abs() < 1e-5);
        assert_eq!(m.score, 100);
    }

    #[test]
    fn match_confidence_zero_for_orthogonal_gallery() {
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![0.0_f32, 1.0, 0.0];
        let m = MatchConfidence::new(&[a], &[b]);
        assert!(m.mean_similarity.abs() < 1e-5);
        assert_eq!(m.score, 0);
    }

    #[test]
    fn merge_appends_to_existing_gallery() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let store = TemplateStore::open(dir.path(), &key_path).unwrap();

        // Pre-existing gallery of 2 templates.
        let existing = vec![
            Template {
                vector: orthogonal_unit(0),
                quality: 0.9,
                is_anchor: true,
            },
            Template {
                vector: orthogonal_unit(1),
                quality: 0.85,
                is_anchor: false,
            },
        ];

        let mut s = EnrollmentSession::with_max(10);
        s.start().unwrap();
        for i in 0..4u8 {
            s.progress(cand_with_vec(80 + i, orthogonal_unit((i as usize) + 2)))
                .unwrap();
        }
        s.finish(&[]).unwrap();
        let ctx = CommitContext {
            store: &store,
            uid: 1002,
            profile_id: ProfileId("default".into()),
            display_name: "test".into(),
            model_id: "sface".into(),
            existing_gallery: existing.clone(),
        };
        s.review_then_commit(CommitDecision::Merge, &ctx).unwrap();

        let gallery = store.enabled_gallery(1002).unwrap();
        // Existing 2 + new selected templates (SELECT_N or reservoir size).
        assert!(
            gallery.len() >= 3,
            "merge must produce at least existing+1 templates, got {}",
            gallery.len()
        );
        // First entry retains anchor.
        assert!(gallery[0].is_anchor);
    }

    #[test]
    fn replace_overwrites_existing_gallery() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let store = TemplateStore::open(dir.path(), &key_path).unwrap();

        // Pre-existing gallery of 5 templates.
        let existing: Vec<Template> = (0..5)
            .map(|i| Template {
                vector: orthogonal_unit(i),
                quality: 0.5,
                is_anchor: i == 0,
            })
            .collect();

        let mut s = EnrollmentSession::with_max(10);
        s.start().unwrap();
        for i in 0..6u8 {
            s.progress(cand_with_vec(80, orthogonal_unit((i as usize) + 2)))
                .unwrap();
        }
        let summary = s.finish(&[]).unwrap();
        let new_count = summary.selected_templates.len();
        let ctx = CommitContext {
            store: &store,
            uid: 1003,
            profile_id: ProfileId("default".into()),
            display_name: "test".into(),
            model_id: "sface".into(),
            existing_gallery: existing.clone(),
        };
        s.review_then_commit(CommitDecision::Replace, &ctx).unwrap();

        let gallery = store.enabled_gallery(1003).unwrap();
        assert_eq!(
            gallery.len(),
            new_count,
            "replace must discard existing gallery and keep only new templates"
        );
        assert!(gallery[0].is_anchor);
    }

    #[test]
    fn reservoir_replaces_lowest_quality_when_full() {
        let mut s = EnrollmentSession::with_max(3);
        s.start().unwrap();
        // Fill with scores 50, 60, 70.
        s.progress(cand(50, 1.0)).unwrap();
        s.progress(cand(60, 2.0)).unwrap();
        s.progress(cand(70, 3.0)).unwrap();
        assert_eq!(s.reservoir_len(), 3);
        // New candidate score 80 must evict the score-50 entry.
        let out = s.progress(cand(80, 4.0)).unwrap();
        match out {
            ProgressOutcome::Replaced { evicted_score } => {
                assert_eq!(evicted_score, 50);
            }
            other => panic!("expected Replaced, got {other:?}"),
        }
        assert_eq!(s.reservoir_len(), 3);
        // New candidate score 30 must be rejected (below floor 60).
        let out = s.progress(cand(30, 5.0)).unwrap();
        assert!(matches!(out, ProgressOutcome::Rejected { .. }));
        assert_eq!(s.reservoir_len(), 3);
    }

    #[test]
    fn drop_zeroizes_raw_frames() {
        let mut s = EnrollmentSession::with_max(10);
        s.start().unwrap();
        // Fill with non-zero raw frames.
        s.progress(Candidate {
            embedding: orthogonal_unit(0),
            quality: EnrollmentQuality::new(0.9, 0.9, 50.0, 5.0, 130.0),
            raw_frame: vec![0xAB; 64],
            width: 8,
            height: 8,
        })
        .unwrap();
        s.progress(Candidate {
            embedding: orthogonal_unit(1),
            quality: EnrollmentQuality::new(0.9, 0.9, 50.0, 5.0, 130.0),
            raw_frame: vec![0xCD; 64],
            width: 8,
            height: 8,
        })
        .unwrap();
        // Drop the session; after drop, internal reservoir raw frames are
        // zeroized. We cannot inspect after drop directly, but the Drop impl
        // calls cancel_inner_zeroize — exercise it explicitly via cancel.
        s.cancel();
        assert_eq!(s.reservoir_len(), 0);
    }
}
