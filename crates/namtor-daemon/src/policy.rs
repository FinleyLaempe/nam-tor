// SPDX-License-Identifier: GPL-3.0-only
//! Temporal decision policy for multi-pair authentication (spec §6.6 / §3.4).
//!
//! The verify engine (see [`crate::verify`]) historically returned a verdict
//! from a single On/Off frame pair. This module lifts that to a temporal
//! policy: up to `max_pairs` pairs (≤ 5) are evaluated within a `5 s` hard
//! deadline, with early exit on a high-confidence first pair and identity
//! coherence enforced across the sequence.
//!
//! Design rules enforced here, not in the caller:
//!
//! - **PAD is a veto, never a weighted term** (spec §6.4). Any pair whose PAD
//!   vetoed forces the final outcome to [`DecisionOutcome::NoMatch`],
//!   regardless of how many matching pairs preceded it.
//! - **Temporal identity coherence.** Alternating identities
//!   (match, no-match, match, …) never average to [`DecisionOutcome::Match`].
//!   A `Match` requires either a single high-confidence pair (early exit) or a
//!   consistent run of matches with no intervening non-match.
//! - **Hard deadline.** The accumulator is deadline-agnostic, but the policy
//!   carries a `hard_deadline: Duration` (5 s) so the caller can produce
//!   [`DecisionOutcome::Timeout`] when `Instant::now()` exceeds the deadline
//!   before the next pair is ready.
//! - **Privacy-safe trace.** [`DecisionAccumulator::trace`] yields per-pair
//!   scores and PAD-veto flags for the journal ONLY. It never carries
//!   embeddings, frames, pixels, or raw model output.
//!
//! The accumulator is a pure state machine: it never reads the clock, the
//! camera, or the store. The caller feeds it one pair at a time via
//! [`DecisionAccumulator::add_pair`] and acts on the returned outcome.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Hard deadline for the full multi-pair sequence (spec §3.4: 5 s).
pub const HARD_DEADLINE: Duration = Duration::from_secs(5);

/// Maximum pairs the policy will ever escalate to.
pub const MAX_PAIRS: u8 = 5;

/// A calibrated operating point. `Balanced` targets convenience (lower
/// threshold, fewer pairs), `Strict` targets security (higher threshold,
/// more pairs). Both stay within the 5 s / 5 pair ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperatingPoint {
    Balanced,
    Strict,
}

impl OperatingPoint {
    /// Cosine-similarity threshold above which a single pair is trusted to
    /// exit immediately. Strict requires a wider margin.
    pub fn high_confidence_threshold(self) -> f32 {
        match self {
            OperatingPoint::Balanced => 0.62,
            OperatingPoint::Strict => 0.78,
        }
    }

    /// Maximum pairs the policy will collect before forcing a decision.
    /// Balanced stops at 3; Strict escalates to the full 5.
    pub fn max_pairs(self) -> u8 {
        match self {
            OperatingPoint::Balanced => 3,
            OperatingPoint::Strict => MAX_PAIRS,
        }
    }
}

/// The configured policy. Built from an [`OperatingPoint`] with an optional
/// override for tests / calibration runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecisionPolicy {
    /// Score above which the first pair exits immediately (early exit).
    pub high_confidence_threshold: f32,
    /// Maximum number of pairs to evaluate before forcing a decision.
    pub max_pairs: u8,
    /// Hard wall-clock deadline for the whole sequence.
    pub hard_deadline: Duration,
    /// When true, identity must be coherent across all observed pairs.
    pub temporal_coherence: bool,
}

impl DecisionPolicy {
    /// Build a policy from a calibrated operating point with the standard
    /// 5 s deadline and temporal coherence enabled.
    pub fn from_operating_point(point: OperatingPoint) -> Self {
        Self {
            high_confidence_threshold: point.high_confidence_threshold(),
            max_pairs: point.max_pairs(),
            hard_deadline: HARD_DEADLINE,
            temporal_coherence: true,
        }
    }

    /// Default Balanced policy (convenience).
    pub fn balanced() -> Self {
        Self::from_operating_point(OperatingPoint::Balanced)
    }

    /// Default Strict policy (security).
    pub fn strict() -> Self {
        Self::from_operating_point(OperatingPoint::Strict)
    }
}

impl Default for DecisionPolicy {
    fn default() -> Self {
        Self::balanced()
    }
}

/// The outcome the accumulator returns for a single pair observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    /// High-confidence early exit, or a coherent run of matches at the pair
    /// cap. The caller should stop capturing and return success.
    Match,
    /// PAD vetoed, or a coherent run of non-matches, or the pair cap reached
    /// without ever matching. The caller should stop capturing and return
    /// failure.
    NoMatch,
    /// More pairs are needed to reach a confident decision.
    Continue,
    /// The hard deadline was exceeded before a decision could be reached.
    /// Treated as `NoMatch` by the caller but is distinguishable in the
    /// journal for latency accounting.
    Timeout,
}

/// Per-pair entry in the privacy-safe journal trace. Scores are kept (they
/// are scalar similarity values, not biometric material); embeddings, frames,
/// and pixels are NEVER included.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairTrace {
    pub pair_index: u8,
    pub score: f32,
    pub is_match: bool,
    pub pad_vetoed: bool,
}

/// Journal-only trace of the accumulator's run. Safe to write to disk: it
/// carries no biometric data, only scalar scores and boolean decisions per
/// pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionTrace {
    pub pairs_seen: u8,
    pub outcome: DecisionOutcome,
    pub pairs: Vec<PairTrace>,
}

/// Pure state machine that accumulates per-pair decisions and returns the
/// next [`DecisionOutcome`]. Owns no I/O and reads no clock — the caller is
/// responsible for enforcing `hard_deadline` and calling
/// [`DecisionAccumulator::timeout`] when it fires.
pub struct DecisionAccumulator {
    policy: DecisionPolicy,
    pairs_seen: u8,
    scores: Vec<f32>,
    identities: Vec<bool>,
    pad_vetoed_any: bool,
}

impl DecisionAccumulator {
    pub fn new(policy: DecisionPolicy) -> Self {
        Self {
            policy,
            pairs_seen: 0,
            scores: Vec::with_capacity(MAX_PAIRS as usize),
            identities: Vec::with_capacity(MAX_PAIRS as usize),
            pad_vetoed_any: false,
        }
    }

    /// Number of pairs observed so far.
    pub fn pairs_seen(&self) -> u8 {
        self.pairs_seen
    }

    /// Record one pair's result and get the next decision.
    ///
    /// `score` is the (optionally z-normalised) cosine similarity for this
    /// pair; `is_match` is whether it cleared the per-pair match threshold
    /// (the caller applies that threshold — the accumulator only reasons
    /// about coherence); `pad_vetoed` is true if PAD rejected this pair.
    pub fn add_pair(&mut self, score: f32, is_match: bool, pad_vetoed: bool) -> DecisionOutcome {
        self.pairs_seen += 1;
        self.scores.push(score);
        self.identities.push(is_match);
        if pad_vetoed {
            self.pad_vetoed_any = true;
        }

        // PAD is a veto: it overrides any amount of matching evidence.
        if self.pad_vetoed_any {
            return DecisionOutcome::NoMatch;
        }

        // Early exit: a single high-confidence match wins immediately.
        if self.pairs_seen == 1 && is_match && score >= self.policy.high_confidence_threshold {
            return DecisionOutcome::Match;
        }

        // Temporal coherence: if we have seen any non-match after seeing a
        // match, the identity stream is incoherent and cannot resolve to
        // Match. Continue collecting until the cap, then force NoMatch.
        let coherent_match = self.policy.temporal_coherence && self.all_matches();

        // At the pair cap, force a decision.
        if self.pairs_seen >= self.policy.max_pairs {
            return if coherent_match {
                DecisionOutcome::Match
            } else {
                DecisionOutcome::NoMatch
            };
        }

        // Mid-sequence: continue if we have not yet decided.
        DecisionOutcome::Continue
    }

    /// Force a timeout outcome. The caller invokes this when the hard
    /// deadline fires before `add_pair` returns a terminal outcome.
    pub fn timeout(&self) -> DecisionOutcome {
        if self.pad_vetoed_any {
            // A PAD veto is terminal and overrides the timeout classification
            // — but `add_pair` already returned `NoMatch` in that case, so
            // reaching here means PAD did not fire and we genuinely ran out
            // of time.
            DecisionOutcome::NoMatch
        } else {
            DecisionOutcome::Timeout
        }
    }

    /// Produce a privacy-safe trace for the journal. Contains per-pair scalar
    /// scores, match flags, and PAD-veto flags ONLY — never embeddings,
    /// frames, or pixels.
    pub fn trace(&self, outcome: DecisionOutcome) -> DecisionTrace {
        let pairs = self
            .scores
            .iter()
            .zip(self.identities.iter())
            .enumerate()
            .map(|(i, (score, is_match))| PairTrace {
                pair_index: i as u8,
                score: *score,
                is_match: *is_match,
                pad_vetoed: self.pad_vetoed_any && i + 1 == self.pairs_seen as usize,
            })
            .collect();
        DecisionTrace {
            pairs_seen: self.pairs_seen,
            outcome,
            pairs,
        }
    }

    fn all_matches(&self) -> bool {
        !self.identities.is_empty() && self.identities.iter().all(|m| *m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// High-confidence first pair exits immediately with `Match`.
    #[test]
    fn high_confidence_first_pair_exits_immediately() {
        let mut acc = DecisionAccumulator::new(DecisionPolicy::balanced());
        let out = acc.add_pair(0.70, true, false);
        assert_eq!(out, DecisionOutcome::Match);
        assert_eq!(acc.pairs_seen(), 1);
    }

    /// An ambiguous first pair (below the high-confidence threshold but
    /// matching) does not exit — it asks for more evidence.
    #[test]
    fn ambiguous_first_pair_continues_to_second() {
        let mut acc = DecisionAccumulator::new(DecisionPolicy::balanced());
        // 0.50 is a match but below the 0.62 high-confidence bar.
        let out = acc.add_pair(0.50, true, false);
        assert_eq!(out, DecisionOutcome::Continue);
        assert_eq!(acc.pairs_seen(), 1);
    }

    /// A coherent run of matches across the pair cap resolves to `Match`.
    #[test]
    fn consistent_matches_across_pair_cap_resolve_to_match() {
        let mut acc = DecisionAccumulator::new(DecisionPolicy::balanced());
        // Balanced cap is 3. Feed three low-margin matches.
        assert_eq!(acc.add_pair(0.50, true, false), DecisionOutcome::Continue);
        assert_eq!(acc.add_pair(0.48, true, false), DecisionOutcome::Continue);
        assert_eq!(acc.add_pair(0.52, true, false), DecisionOutcome::Match);
    }

    /// Alternating identities (match, no-match, match) must never resolve to
    /// `Match` — temporal coherence is enforced.
    #[test]
    fn alternating_identities_never_match() {
        let mut acc = DecisionAccumulator::new(DecisionPolicy::balanced());
        assert_eq!(acc.add_pair(0.50, true, false), DecisionOutcome::Continue);
        assert_eq!(acc.add_pair(0.30, false, false), DecisionOutcome::Continue);
        // Third pair is a match, but the sequence is incoherent → NoMatch.
        assert_eq!(acc.add_pair(0.55, true, false), DecisionOutcome::NoMatch);
    }

    /// A PAD veto on any pair forces `NoMatch` regardless of accumulated
    /// matching evidence.
    #[test]
    fn pad_veto_always_produces_no_match() {
        let mut acc = DecisionAccumulator::new(DecisionPolicy::balanced());
        // First pair: high-confidence match.
        assert_eq!(acc.add_pair(0.80, true, false), DecisionOutcome::Match);
        // A second pair with a PAD veto overrides — but in practice the
        // caller would have stopped after the first Match. Test the case
        // where PAD fires on the FIRST pair instead.
        let mut acc2 = DecisionAccumulator::new(DecisionPolicy::balanced());
        assert_eq!(acc2.add_pair(0.80, true, true), DecisionOutcome::NoMatch);
    }

    /// PAD veto mid-sequence also forces `NoMatch`.
    #[test]
    fn pad_veto_mid_sequence_forces_no_match() {
        let mut acc = DecisionAccumulator::new(DecisionPolicy::strict());
        assert_eq!(acc.add_pair(0.50, true, false), DecisionOutcome::Continue);
        assert_eq!(acc.add_pair(0.50, true, true), DecisionOutcome::NoMatch);
    }

    /// The caller can signal the hard deadline; `timeout` returns `Timeout`
    /// when no PAD veto occurred.
    #[test]
    fn hard_deadline_enforced_as_timeout() {
        let acc = DecisionAccumulator::new(DecisionPolicy::balanced());
        // No pairs observed, deadline fires.
        assert_eq!(acc.timeout(), DecisionOutcome::Timeout);
    }

    /// `timeout` after a PAD veto still returns `NoMatch` (the veto is
    /// terminal and overrides the timeout classification).
    #[test]
    fn timeout_after_pad_veto_still_no_match() {
        let mut acc = DecisionAccumulator::new(DecisionPolicy::balanced());
        let _ = acc.add_pair(0.50, true, true);
        assert_eq!(acc.timeout(), DecisionOutcome::NoMatch);
    }

    /// Strict and Balanced operating points produce different thresholds and
    /// pair caps.
    #[test]
    fn strict_and_balanced_thresholds_differ() {
        let balanced = DecisionPolicy::balanced();
        let strict = DecisionPolicy::strict();
        assert!(
            strict.high_confidence_threshold > balanced.high_confidence_threshold,
            "Strict must require a higher confidence for early exit"
        );
        assert!(
            strict.max_pairs > balanced.max_pairs,
            "Strict must escalate to more pairs than Balanced"
        );
        assert_eq!(balanced.max_pairs, 3);
        assert_eq!(strict.max_pairs, MAX_PAIRS);
        assert_eq!(balanced.hard_deadline, HARD_DEADLINE);
        assert_eq!(strict.hard_deadline, HARD_DEADLINE);
    }

    /// A first pair that is a non-match and below threshold continues, and a
    /// full run of non-matches at the cap resolves to `NoMatch`.
    #[test]
    fn consistent_non_matches_resolve_to_no_match() {
        let mut acc = DecisionAccumulator::new(DecisionPolicy::balanced());
        assert_eq!(acc.add_pair(0.20, false, false), DecisionOutcome::Continue);
        assert_eq!(acc.add_pair(0.18, false, false), DecisionOutcome::Continue);
        assert_eq!(acc.add_pair(0.22, false, false), DecisionOutcome::NoMatch);
    }

    /// Strict operating point does NOT early-exit on a score that would have
    /// won under Balanced.
    #[test]
    fn strict_does_not_early_exit_below_its_threshold() {
        let mut acc = DecisionAccumulator::new(DecisionPolicy::strict());
        // 0.70 ≥ Balanced's 0.62 but < Strict's 0.78.
        let out = acc.add_pair(0.70, true, false);
        assert_eq!(out, DecisionOutcome::Continue);
    }

    /// The journal trace contains only scalar scores, match flags, and PAD
    /// flags — never embeddings, frames, or pixels.
    #[test]
    fn trace_is_privacy_safe() {
        let mut acc = DecisionAccumulator::new(DecisionPolicy::balanced());
        let _ = acc.add_pair(0.50, true, false);
        let _ = acc.add_pair(0.30, false, false);
        let trace = acc.trace(DecisionOutcome::NoMatch);

        let serialised = serde_json::to_string(&trace).expect("trace must serialise");
        // No biometric field names may appear in the trace.
        for forbidden in ["embedding", "frame", "pixel", "template", "vector", "0x"] {
            assert!(
                !serialised.contains(forbidden),
                "trace leaks `{forbidden}`: {serialised}"
            );
        }
        assert_eq!(trace.pairs.len(), 2);
        assert_eq!(trace.pairs[0].score, 0.50);
        assert!(trace.pairs[0].is_match);
        assert!(!trace.pairs[0].pad_vetoed);
    }

    /// `OperatingPoint` serialises round-trip (used in daemon config).
    #[test]
    fn operating_point_serialises_roundtrip() {
        for point in [OperatingPoint::Balanced, OperatingPoint::Strict] {
            let s = serde_json::to_string(&point).expect("ser");
            let back: OperatingPoint = serde_json::from_str(&s).expect("de");
            assert_eq!(point, back);
        }
    }
}
