// SPDX-License-Identifier: GPL-3.0-only
//! Learned differential PAD — the second-stage veto on top of the analytic
//! gates (spec §6.4 + §10.3).
//!
//! The analytic gates in `namtor-pad` run first and are physics-based: they
//! refuse obvious spoofs (screen replay, flat print, cutout) without any
//! trained model. The learned PAD is a small MiniFASNet-class CNN trained on
//! strobe differentials `D = On − Off` that catches subtler attacks the
//! analytic gates cannot express. **It is still a veto, never a weighted
//! term** (spec §6.6): the only outcomes are `Live` (proceed to matching)
//! or `Spoof` (refuse).
//!
//! ## Fail-closed semantics
//!
//! Every non-`Live` outcome refuses authentication:
//!
//! - analytic gate failure → refuse immediately, learned model is not run;
//! - analytic passes but the learned model is missing/unavailable →
//!   [`PadVerdict::Uncertain`] → treated as spoof → refuse;
//! - analytic passes, learned model returns `Spoof` → refuse;
//! - analytic passes, learned model returns `Uncertain` (low confidence)
//!   → refuse.
//!
//! Only `Live` from both stages proceeds. This module never silently widens
//! the trust boundary.
//!
//! ## Decoupling from `namtor-pad`
//!
//! [`AnalyticResult`] is defined locally rather than re-exported from
//! `namtor-pad` so this module compiles / clippy-clean independently of the
//! analytic PAD crate's evolution (e.g. calibration work in task 21). It is
//! a plain structural mirror: callers convert their `namtor_pad::PadVerdict`
//! into [`AnalyticResult`] at the boundary.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Error returned by a learned PAD backend.
#[derive(Debug, Error)]
pub enum LearnedPadError {
    /// Model file not found at the requested path.
    #[error("learned PAD model not found: {0}")]
    NotFound(PathBuf),
    /// I/O error reading the model bytes.
    #[error("i/o error reading learned PAD model: {0}")]
    Io(#[from] std::io::Error),
    /// Backend-specific failure (ONNX parse, shape mismatch, …).
    #[error("learned PAD backend error: {0}")]
    Backend(String),
    /// Input differential had unexpected length / shape.
    #[error("learned PAD input shape mismatch: expected {expected}, got {got}")]
    Shape { expected: usize, got: usize },
}

/// Local mirror of the analytic PAD verdict.
///
/// Only the `live` flag drives the veto decision; the per-gate scores are
/// carried for audit/logging only and are not combined into a weighted term
/// (spec §6.6). Callers build this from `namtor_pad::PadVerdict` at the
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AnalyticResult {
    /// `true` iff every analytic gate passed.
    pub live: bool,
    /// Energy-gate score in `[0,1]` (audit only, not used by the veto).
    pub energy_score: f32,
    /// Gradient-gate score in `[0,1]` (audit only).
    pub gradient_score: f32,
    /// Specular-gate score in `[0,1]` (audit only).
    pub specular_score: f32,
    /// Radial-falloff score in `[0,1]` (audit only).
    pub radial_score: f32,
}

impl AnalyticResult {
    /// Convenience: an analytic verdict with all gates passing.
    pub const fn live() -> Self {
        Self {
            live: true,
            energy_score: 1.0,
            gradient_score: 1.0,
            specular_score: 1.0,
            radial_score: 1.0,
        }
    }

    /// Convenience: an analytic verdict with the energy gate failing.
    pub const fn spoof() -> Self {
        Self {
            live: false,
            energy_score: 0.0,
            gradient_score: 1.0,
            specular_score: 1.0,
            radial_score: 1.0,
        }
    }
}

/// Tri-state verdict of the learned PAD model.
///
/// `Uncertain` is fail-closed: callers MUST treat it identically to `Spoof`.
/// It is reported separately so the audit log can distinguish "model refused"
/// from "model unavailable / low-confidence".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PadVerdict {
    /// Model is confident the differential is a live face.
    Live,
    /// Model is confident this is a presentation attack.
    Spoof,
    /// Model could not decide — missing, errored, or below the confidence
    /// threshold. Fail-closed: refuse.
    Uncertain,
}

impl PadVerdict {
    /// `true` only for [`PadVerdict::Live`]. Every other variant refuses.
    pub fn is_live(self) -> bool {
        matches!(self, PadVerdict::Live)
    }

    /// `true` if this verdict should refuse authentication. `Uncertain`
    /// refuses (fail-closed).
    pub fn refuses(self) -> bool {
        !self.is_live()
    }
}

/// Output of a single learned PAD inference.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PadResult {
    /// Posterior probability that the differential is a live face, in
    /// `[0.0, 1.0]`. `-1.0` if the model could not produce a score
    /// (missing/unavailable).
    pub live_probability: f32,
    /// Discrete verdict derived from `live_probability` and the configured
    /// thresholds.
    pub verdict: PadVerdict,
}

impl PadResult {
    /// Sentinel returned when the model is unavailable: probability unknown,
    /// verdict [`PadVerdict::Uncertain`] (fail-closed).
    pub const fn unavailable() -> Self {
        Self {
            live_probability: -1.0,
            verdict: PadVerdict::Uncertain,
        }
    }

    /// Build a `PadResult` from a raw live probability and a threshold pair.
    ///
    /// - `p >= live_min` → [`PadVerdict::Live`]
    /// - `p <= spoof_max` → [`PadVerdict::Spoof`]
    /// - otherwise → [`PadVerdict::Uncertain`] (fail-closed)
    ///
    /// `live_min` MUST be > `spoof_max`; if not, this returns `Uncertain`
    /// rather than panicking (defensive against misconfiguration, but the
    /// [`VetoPolicy`] constructor validates this up front).
    pub fn from_probability(p: f32, live_min: f32, spoof_max: f32) -> Self {
        if !(0.0..=1.0).contains(&p) || live_min <= spoof_max {
            return Self {
                live_probability: p.clamp(-1.0, 1.0),
                verdict: PadVerdict::Uncertain,
            };
        }
        let verdict = if p >= live_min {
            PadVerdict::Live
        } else if p <= spoof_max {
            PadVerdict::Spoof
        } else {
            PadVerdict::Uncertain
        };
        Self {
            live_probability: p,
            verdict,
        }
    }
}

/// Inference backend for the learned differential PAD model.
///
/// Implementations:
/// - [`MockLearnedPadBackend`] — deterministic, test-only.
/// - the `onnx`-feature backend in [`crate::infer_onnx`] (when wired).
///
/// Implementations must be fail-closed: any internal error returns
/// [`PadVerdict::Uncertain`] via [`PadResult::unavailable`], never panics.
pub trait LearnedPadBackend: Send + Sync {
    /// Load the model from `model_path`. Called once at worker start.
    fn load(model_path: &Path) -> Result<Self, LearnedPadError>
    where
        Self: Sized;

    /// Run inference on a flattened strobe differential `D = On − Off`,
    /// row-major, length `H * W`. Returns a [`PadResult`].
    fn infer(&self, differential: &[f32]) -> Result<PadResult, LearnedPadError>;
}

/// Deterministic test-only [`LearnedPadBackend`].
///
/// Maps the mean of the differential to a live probability via a simple
/// affine transform, then thresholds. No model file is read; `load` succeeds
/// only if `model_path` exists (so fail-closed path is testable).
#[derive(Debug, Clone)]
pub struct MockLearnedPadBackend {
    live_min: f32,
    spoof_max: f32,
}

impl MockLearnedPadBackend {
    /// Construct with explicit thresholds.
    pub fn new(live_min: f32, spoof_max: f32) -> Self {
        Self {
            live_min,
            spoof_max,
        }
    }

    /// Default thresholds: `live_min = 0.7`, `spoof_max = 0.3`.
    pub fn default_thresholds() -> Self {
        Self::new(0.7, 0.3)
    }
}

impl LearnedPadBackend for MockLearnedPadBackend {
    fn load(model_path: &Path) -> Result<Self, LearnedPadError> {
        if !model_path.exists() {
            return Err(LearnedPadError::NotFound(model_path.to_path_buf()));
        }
        Ok(Self::default_thresholds())
    }

    fn infer(&self, differential: &[f32]) -> Result<PadResult, LearnedPadError> {
        if differential.is_empty() {
            return Err(LearnedPadError::Shape {
                expected: 1,
                got: 0,
            });
        }
        // Mean of the differential: live faces have strong positive D,
        // spoofs have near-zero / weak D. Map mean ∈ [0, ~80] → p ∈ [0, 1].
        let mean = differential.iter().copied().sum::<f32>() / differential.len() as f32;
        let p = (mean / 80.0).clamp(0.0, 1.0);
        Ok(PadResult::from_probability(
            p,
            self.live_min,
            self.spoof_max,
        ))
    }
}

/// Configuration for the two-stage PAD veto policy.
///
/// Encodes spec §6.4 + §6.6: **analytic gates run first**, the learned model
/// runs **only if the analytic gates pass**, and the combined verdict is
/// fail-closed (`Uncertain` ⇒ refuse).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VetoPolicy {
    /// Minimum learned live probability to accept. `p >= live_min` ⇒ `Live`.
    pub live_min: f32,
    /// Maximum learned live probability still considered a spoof.
    /// `p <= spoof_max` ⇒ `Spoof`. The band `(spoof_max, live_min)` is
    /// `Uncertain` and refuses (fail-closed).
    pub spoof_max: f32,
    /// If `true`, the learned model is bypassed entirely and only the
    /// analytic gates decide. Used during rollout / when the model is
    /// untrusted. The analytic veto alone still applies.
    pub learned_disabled: bool,
}

impl Default for VetoPolicy {
    fn default() -> Self {
        Self {
            live_min: 0.7,
            spoof_max: 0.3,
            learned_disabled: false,
        }
    }
}

impl VetoPolicy {
    /// Validate the policy. `live_min` must be > `spoof_max`, both in `[0,1]`.
    pub fn validate(&self) -> Result<(), LearnedPadError> {
        if !(0.0..=1.0).contains(&self.live_min) || !(0.0..=1.0).contains(&self.spoof_max) {
            return Err(LearnedPadError::Backend(format!(
                "veto policy thresholds out of [0,1]: live_min={}, spoof_max={}",
                self.live_min, self.spoof_max
            )));
        }
        if self.live_min <= self.spoof_max {
            return Err(LearnedPadError::Backend(format!(
                "veto policy live_min ({}) must exceed spoof_max ({})",
                self.live_min, self.spoof_max
            )));
        }
        Ok(())
    }
}

/// Final combined verdict of the two-stage PAD pipeline.
///
/// Only [`FinalVerdict::Live`] proceeds to face matching. Every other
/// variant refuses authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinalVerdict {
    /// Both stages passed: analytic live AND learned live (or learned
    /// disabled and analytic live).
    Live,
    /// Analytic gates refused.
    AnalyticSpoof,
    /// Analytic passed, learned model refused (Spoof).
    LearnedSpoof,
    /// Analytic passed, learned model unavailable / uncertain → fail-closed.
    FailClosed,
}

impl FinalVerdict {
    /// `true` only for [`FinalVerdict::Live`].
    pub fn is_live(self) -> bool {
        matches!(self, FinalVerdict::Live)
    }

    /// `true` if this verdict should refuse authentication.
    pub fn refuses(self) -> bool {
        !self.is_live()
    }
}

/// Evaluate the two-stage veto.
///
/// Order (spec §6.4 + §6.6):
/// 1. If the analytic verdict has any failing gate (`!analytic.live`),
///    return [`FinalVerdict::AnalyticSpoof`] immediately — the learned
///    model is **not** run.
/// 2. If [`VetoPolicy::learned_disabled`], return [`FinalVerdict::Live`]
///    (analytic alone passed).
/// 3. Otherwise apply the learned `PadResult`:
///    - [`PadVerdict::Live`] → [`FinalVerdict::Live`]
///    - [`PadVerdict::Spoof`] → [`FinalVerdict::LearnedSpoof`]
///    - [`PadVerdict::Uncertain`] → [`FinalVerdict::FailClosed`]
///
/// This function is pure and side-effect free; it is the single decision
/// point that callers must use. `B` is the learned backend; the analytic
/// result already encodes its own gate verdict.
pub fn evaluate<B: LearnedPadBackend>(
    analytic: &AnalyticResult,
    learned: &PadResult,
    policy: &VetoPolicy,
    _backend: &B,
) -> FinalVerdict {
    // Stage 1: analytic veto.
    if !analytic.live {
        return FinalVerdict::AnalyticSpoof;
    }
    // Stage 2: optional learned bypass.
    if policy.learned_disabled {
        return FinalVerdict::Live;
    }
    // Stage 3: learned verdict.
    match learned.verdict {
        PadVerdict::Live => FinalVerdict::Live,
        PadVerdict::Spoof => FinalVerdict::LearnedSpoof,
        PadVerdict::Uncertain => FinalVerdict::FailClosed,
    }
}

#[cfg(test)]
mod tests {
    //! Eight tests covering the veto policy decision matrix and fail-closed
    //! semantics. All tests use [`MockLearnedPadBackend`] — no ONNX Runtime,
    //! no model weights, no biometric data.

    use super::*;

    fn policy() -> VetoPolicy {
        VetoPolicy::default()
    }

    fn backend() -> MockLearnedPadBackend {
        MockLearnedPadBackend::default_thresholds()
    }

    fn learned_live() -> PadResult {
        PadResult {
            live_probability: 0.95,
            verdict: PadVerdict::Live,
        }
    }

    fn learned_spoof() -> PadResult {
        PadResult {
            live_probability: 0.1,
            verdict: PadVerdict::Spoof,
        }
    }

    fn learned_uncertain() -> PadResult {
        PadResult::unavailable()
    }

    // 1. Analytic spoof short-circuits: learned model verdict is ignored.
    #[test]
    fn test_analytic_spoof_vetoes_regardless_of_learned() {
        let p = policy();
        let b = backend();
        let v = evaluate(&AnalyticResult::spoof(), &learned_live(), &p, &b);
        assert_eq!(v, FinalVerdict::AnalyticSpoof);
        assert!(v.refuses());
    }

    // 2. Analytic live + learned live ⇒ FinalVerdict::Live.
    #[test]
    fn test_both_live_proceeds() {
        let p = policy();
        let b = backend();
        let v = evaluate(&AnalyticResult::live(), &learned_live(), &p, &b);
        assert_eq!(v, FinalVerdict::Live);
        assert!(v.is_live());
    }

    // 3. Analytic live + learned spoof ⇒ LearnedSpoof (refuse).
    #[test]
    fn test_learned_spoof_after_analytic_live_refuses() {
        let p = policy();
        let b = backend();
        let v = evaluate(&AnalyticResult::live(), &learned_spoof(), &p, &b);
        assert_eq!(v, FinalVerdict::LearnedSpoof);
        assert!(v.refuses());
    }

    // 4. Analytic live + learned uncertain ⇒ FailClosed (fail-closed refuse).
    #[test]
    fn test_learned_uncertain_is_fail_closed() {
        let p = policy();
        let b = backend();
        let v = evaluate(&AnalyticResult::live(), &learned_uncertain(), &p, &b);
        assert_eq!(v, FinalVerdict::FailClosed);
        assert!(v.refuses());
    }

    // 5. learned_disabled bypasses the learned model: analytic live ⇒ Live.
    #[test]
    fn test_learned_disabled_uses_analytic_only() {
        let p = VetoPolicy {
            learned_disabled: true,
            ..policy()
        };
        let b = backend();
        let v = evaluate(&AnalyticResult::live(), &learned_spoof(), &p, &b);
        assert_eq!(v, FinalVerdict::Live);
    }

    // 6. learned_disabled does NOT bypass the analytic veto.
    #[test]
    fn test_learned_disabled_still_respects_analytic_veto() {
        let p = VetoPolicy {
            learned_disabled: true,
            ..policy()
        };
        let b = backend();
        let v = evaluate(&AnalyticResult::spoof(), &learned_live(), &p, &b);
        assert_eq!(v, FinalVerdict::AnalyticSpoof);
    }

    // 7. Mock backend maps a strong-positive differential to Live.
    #[test]
    fn test_mock_backend_live_differential() {
        let b = backend();
        let diff = vec![80.0_f32; 64 * 64];
        let r = b.infer(&diff).expect("infer");
        assert!(
            r.verdict.is_live(),
            "verdict={:?} p={}",
            r.verdict,
            r.live_probability
        );
    }

    // 8. Mock backend maps a near-zero differential to Spoof.
    #[test]
    fn test_mock_backend_spoof_differential() {
        let b = backend();
        let diff = vec![0.0_f32; 64 * 64];
        let r = b.infer(&diff).expect("infer");
        assert!(r.verdict.refuses(), "verdict={:?}", r.verdict);
    }

    // 9. VetoPolicy::validate rejects live_min <= spoof_max.
    #[test]
    fn test_policy_validate_rejects_inverted_thresholds() {
        let bad = VetoPolicy {
            live_min: 0.3,
            spoof_max: 0.7,
            learned_disabled: false,
        };
        assert!(bad.validate().is_err());
    }

    // 10. PadResult::from_probability places the band as Uncertain.
    #[test]
    fn test_pad_result_band_is_uncertain() {
        let r = PadResult::from_probability(0.5, 0.7, 0.3);
        assert_eq!(r.verdict, PadVerdict::Uncertain);
        let r_live = PadResult::from_probability(0.9, 0.7, 0.3);
        assert_eq!(r_live.verdict, PadVerdict::Live);
        let r_spoof = PadResult::from_probability(0.1, 0.7, 0.3);
        assert_eq!(r_spoof.verdict, PadVerdict::Spoof);
    }

    // 11. Missing model file → NotFound, never panic.
    #[test]
    fn test_load_missing_model_is_not_found() {
        let p = Path::new("/nonexistent/namtor/does/not/exist.onnx");
        let r = MockLearnedPadBackend::load(p);
        assert!(matches!(r, Err(LearnedPadError::NotFound(_))));
    }

    // 12. Empty differential input ⇒ Shape error (not panic, not Uncertain).
    #[test]
    fn test_empty_differential_is_shape_error() {
        let b = backend();
        let r = b.infer(&[]);
        assert!(matches!(r, Err(LearnedPadError::Shape { .. })));
    }

    // 13. PadResult::unavailable is Uncertain and refuses.
    #[test]
    fn test_unavailable_is_uncertain_and_refuses() {
        let u = PadResult::unavailable();
        assert_eq!(u.verdict, PadVerdict::Uncertain);
        assert!(u.verdict.refuses());
        assert!(!u.verdict.is_live());
        assert_eq!(u.live_probability, -1.0);
    }
}
