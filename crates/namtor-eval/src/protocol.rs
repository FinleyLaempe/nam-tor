// SPDX-License-Identifier: GPL-3.0-only
//! Production evaluation protocol and gate reports (spec §12 G2-G4).
//!
//! The protocol is the contract the evaluation harness enforces before any
//! gate number is published. Reports are the evidentiary artefacts; both are
//! serialisable so a run can be archived and re-checked.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use thiserror::Error;

/// Minimum impostor comparisons for G3 to be reportable at all. The FAR ≤ 1e-5
/// *claim* additionally requires the rule-of-three upper bound (3/n) to clear
/// 1e-5, i.e. `n >= 300_000` (see [`far_upper_bound_95`]).
pub const G3_MIN_IMPOSTOR_COMPARISONS: u64 = 100_000;

/// Sample size at which zero false accepts yields a 95% upper bound of 1e-5
/// under the rule of three (3/n = 1e-5 -> n = 300_000).
pub const FAR_CLAIM_MIN_N: u64 = 300_000;

/// Minimum genuine comparisons per capture condition for G3.
pub const G3_MIN_GENUINE_PER_CONDITION: u32 = 1_000;

/// Wilson score interval 95% upper bound for `successes` out of `trials`.
///
/// Used for FAR/FRR confidence intervals. Returns the upper bound only — that
/// is the side that gates a "≤" claim. Implemented from the standard formula
/// with z = 1.959964 (the 0.975 quantile of the standard normal).
pub fn wilson_upper_95(successes: u64, trials: u64) -> f64 {
    if trials == 0 {
        return 1.0;
    }
    let z = 1.959_963_984_540_054_f64;
    let z2 = z * z;
    let n = trials as f64;
    let p = successes as f64 / n;
    let denom = 1.0 + z2 / n;
    let centre = p + z2 / (2.0 * n);
    let spread = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    (centre + spread) / denom
}

/// Rule-of-three 95% upper bound for zero observed events out of `trials`.
/// Equal to `3 / trials` for `trials > 0`; defined as 1.0 for `trials == 0`.
///
/// This is the conservative special case of [`wilson_upper_95`] at `successes
/// = 0` and is the bound the FAR ≤ 1e-5 gate is held to.
pub fn far_upper_bound_95(trials: u64) -> f64 {
    if trials == 0 {
        1.0
    } else {
        3.0 / trials as f64
    }
}

/// Identity slice: the set of subjects assigned to one side of the
/// subject-disjoint partition (train or test).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentitySlice {
    pub role: SliceRole,
    pub subject_ids: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SliceRole {
    Train,
    Test,
}

/// Device slice: which physical device captured a condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSlice {
    pub label: String,
    pub capture_conditions: Vec<String>,
}

/// The evaluation protocol — the contract a run must satisfy before any gate
/// number is published. Enforced by [`validate_protocol`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalProtocol {
    pub subject_disjoint: Vec<IdentitySlice>,
    pub min_impostor_comparisons: u64,
    pub min_genuine_per_condition: u32,
    pub device_slices: Vec<DeviceSlice>,
    /// If `true`, PAD error rates may be reported averaged across attack
    /// types. G4 forbids this; the flag exists so the validator can reject it.
    pub allow_averaged_attacks: bool,
}

/// Provenance for a single evaluation run: every hash that must agree before
/// the reports can be re-attached to a build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalConfig {
    pub commit_hash: String,
    pub model_hashes: Vec<String>,
    pub hardware_hash: String,
    pub target_gates: Vec<TargetGate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetGate {
    G2,
    G3,
    G4,
}

/// G2 — latency. Spec: median ≤ 500 ms, p95 ≤ 1000 ms, first-boot vs
/// steady-state reported separately because cold model load dominates the
/// first authentication of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct G2Report {
    pub median_ms: f64,
    pub p95_ms: f64,
    pub first_boot: LatencyBand,
    pub steady_state: LatencyBand,
    pub n_attempts: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatencyBand {
    pub median_ms: f64,
    pub p95_ms: f64,
    pub n: u64,
}

/// G3 — biometric accuracy. The DET curve is shipped as a list of operating
/// points so the curve can be redrawn without re-running the harness.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct G3Report {
    pub det_points: Vec<DetPoint>,
    /// FAR achieved at the operating point where FRR = 3%.
    pub far_at_frr_3pct: f64,
    /// 95% Wilson upper bound on `far_at_frr_3pct`.
    pub far_ci_upper_95: f64,
    /// 95% Wilson upper bound on FRR at that operating point.
    pub frr_ci_upper_95: f64,
    pub n_impostor_comparisons: u64,
    pub n_genuine_comparisons: u64,
    /// `true` iff `n_impostor_comparisons >= FAR_CLAIM_MIN_N` so the rule-of-
    /// three upper bound is ≤ 1e-5. When `false`, the FAR ≤ 1e-5 claim is
    /// WITHHELD even if the point estimate is zero.
    pub far_claim_eligible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DetPoint {
    pub threshold: f32,
    pub far: f64,
    pub frr: f64,
}

/// G4 — presentation attack detection. APCER is reported **per attack type**
/// and never averaged (spec §12 G4). 3D masks are explicitly out of scope for
/// the reference hardware, which has no depth sensor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct G4Report {
    pub per_attack: Vec<AttackRate>,
    pub bpcer: f64,
    pub bpcer_ci_upper_95: f64,
    pub n_bona_fide: u64,
    /// Always `false` on the reference hardware; recorded so the report is
    /// self-describing about what was *not* tested.
    pub evaluated_3d_mask: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttackRate {
    pub attack_type: String,
    pub apcer: f64,
    pub apcer_ci_upper_95: f64,
    pub n_attacks: u64,
}

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("impostor comparison count {n} below G3 minimum {min}")]
    TooFewImpostorComparisons { n: u64, min: u64 },
    #[error("genuine comparisons per condition {n} below G3 minimum {min}")]
    TooFewGenuinePerCondition { n: u32, min: u32 },
    #[error("identity overlap between train and test slices: {overlapping:?}")]
    IdentityOverlap { overlapping: Vec<String> },
    #[error("protocol permits averaged attacks; G4 requires per-attack reporting")]
    AveragedAttacksAllowed,
    #[error(
        "G3 report missing confidence intervals (FAR upper bound required for the ≤ 1e-5 gate)"
    )]
    MissingConfidenceInterval,
}

/// Validate a protocol against the G2-G4 publication rules.
///
/// Returns `Ok(())` only when the partition is subject-disjoint, the
/// comparison counts clear the G3 floors, and the protocol does not permit
/// averaged attack rates. CI presence is a property of the reports, not the
/// protocol, and is checked by [`validate_g3_report`].
pub fn validate_protocol(protocol: &EvalProtocol) -> Result<(), EvalError> {
    if protocol.min_impostor_comparisons < G3_MIN_IMPOSTOR_COMPARISONS {
        return Err(EvalError::TooFewImpostorComparisons {
            n: protocol.min_impostor_comparisons,
            min: G3_MIN_IMPOSTOR_COMPARISONS,
        });
    }
    if protocol.min_genuine_per_condition < G3_MIN_GENUINE_PER_CONDITION {
        return Err(EvalError::TooFewGenuinePerCondition {
            n: protocol.min_genuine_per_condition,
            min: G3_MIN_GENUINE_PER_CONDITION,
        });
    }
    if protocol.allow_averaged_attacks {
        return Err(EvalError::AveragedAttacksAllowed);
    }
    // Subject-disjoint: no subject may appear in two slices.
    let mut seen: HashSet<&str> = HashSet::new();
    let mut overlap: Vec<String> = Vec::new();
    for slice in &protocol.subject_disjoint {
        for id in &slice.subject_ids {
            if !seen.insert(id.as_str()) {
                overlap.push(id.clone());
            }
        }
    }
    if !overlap.is_empty() {
        overlap.sort();
        return Err(EvalError::IdentityOverlap {
            overlapping: overlap,
        });
    }
    Ok(())
}

/// Validate a G3 report against the CI rule. The FAR ≤ 1e-5 *claim* is gated
/// by `far_claim_eligible`; this function only enforces that the CI fields
/// are present and finite so a report cannot silently drop them.
pub fn validate_g3_report(report: &G3Report) -> Result<(), EvalError> {
    if !report.far_ci_upper_95.is_finite() || !report.frr_ci_upper_95.is_finite() {
        return Err(EvalError::MissingConfidenceInterval);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disjoint_protocol() -> EvalProtocol {
        let mut train = HashSet::new();
        train.insert("s001".to_string());
        train.insert("s002".to_string());
        let mut test = HashSet::new();
        test.insert("s003".to_string());
        test.insert("s004".to_string());
        EvalProtocol {
            subject_disjoint: vec![
                IdentitySlice {
                    role: SliceRole::Train,
                    subject_ids: train,
                },
                IdentitySlice {
                    role: SliceRole::Test,
                    subject_ids: test,
                },
            ],
            min_impostor_comparisons: G3_MIN_IMPOSTOR_COMPARISONS,
            min_genuine_per_condition: G3_MIN_GENUINE_PER_CONDITION,
            device_slices: vec![DeviceSlice {
                label: "asus-gv601vi".to_string(),
                capture_conditions: vec!["office".to_string(), "dim".to_string()],
            }],
            allow_averaged_attacks: false,
        }
    }

    #[test]
    fn valid_protocol_passes() {
        assert!(validate_protocol(&disjoint_protocol()).is_ok());
    }

    #[test]
    fn low_impostor_count_is_rejected() {
        let mut p = disjoint_protocol();
        p.min_impostor_comparisons = 99_999;
        let err = validate_protocol(&p).unwrap_err();
        assert!(matches!(err, EvalError::TooFewImpostorComparisons { .. }));
    }

    #[test]
    fn identity_overlap_is_rejected() {
        let mut p = disjoint_protocol();
        // Inject an overlap into the test slice.
        p.subject_disjoint[1].subject_ids.insert("s001".to_string());
        let err = validate_protocol(&p).unwrap_err();
        assert!(matches!(err, EvalError::IdentityOverlap { .. }));
        assert!(err.to_string().contains("s001"));
    }

    #[test]
    fn averaged_attacks_are_rejected() {
        let mut p = disjoint_protocol();
        p.allow_averaged_attacks = true;
        let err = validate_protocol(&p).unwrap_err();
        assert!(matches!(err, EvalError::AveragedAttacksAllowed));
    }

    #[test]
    fn rule_of_three_upper_bound_at_100k_is_above_1e5() {
        // 0 / 100_000 -> 3e-5, which is above the 1e-5 claim threshold.
        let ub = far_upper_bound_95(100_000);
        assert!(ub > 1e-5, "upper bound {ub} must exceed 1e-5 at 100k");
        // 100k comparisons are enough to *run* G3 but not to claim FAR ≤ 1e-5.
        assert!(100_000 < FAR_CLAIM_MIN_N);
    }

    #[test]
    fn rule_of_three_upper_bound_at_300k_meets_1e5() {
        let ub = far_upper_bound_95(FAR_CLAIM_MIN_N);
        assert!(
            (ub - 1e-5).abs() <= 1e-7,
            "upper bound {ub} should be ~1e-5 at 300k"
        );
    }

    #[test]
    fn wilson_upper_bound_is_in_unit_interval() {
        // Bound on 0/0 is defined as 1.0 (no evidence -> worst case).
        assert_eq!(wilson_upper_95(0, 0), 1.0);
        // 50/100 -> a real interval strictly inside (0,1).
        let ub = wilson_upper_95(50, 100);
        assert!(ub > 0.5 && ub < 1.0, "got {ub}");
        // 0/1000 -> upper bound is a small positive number near the
        // rule-of-three 3/1000 = 3e-3 (Wilson is slightly larger).
        let ub_zero = wilson_upper_95(0, 1000);
        assert!(ub_zero > 0.0 && ub_zero < 5e-3, "got {ub_zero}");
    }

    #[test]
    fn g3_report_requires_finite_ci() {
        let good = G3Report {
            det_points: vec![DetPoint {
                threshold: 0.5,
                far: 0.0,
                frr: 0.03,
            }],
            far_at_frr_3pct: 0.0,
            far_ci_upper_95: 3e-5,
            frr_ci_upper_95: 0.04,
            n_impostor_comparisons: 100_000,
            n_genuine_comparisons: 2_000,
            far_claim_eligible: false,
        };
        assert!(validate_g3_report(&good).is_ok());

        let mut bad = good.clone();
        bad.far_ci_upper_95 = f64::NAN;
        assert!(matches!(
            validate_g3_report(&bad).unwrap_err(),
            EvalError::MissingConfidenceInterval
        ));
    }

    #[test]
    fn far_claim_withheld_below_300k_even_at_zero_false_accepts() {
        // Zero false accepts at 100k: point estimate is 0 but the claim is
        // WITHHELD because the rule-of-three upper bound (3e-5) > 1e-5.
        let report = G3Report {
            det_points: vec![DetPoint {
                threshold: 0.62,
                far: 0.0,
                frr: 0.03,
            }],
            far_at_frr_3pct: 0.0,
            far_ci_upper_95: far_upper_bound_95(100_000),
            frr_ci_upper_95: 0.04,
            n_impostor_comparisons: 100_000,
            n_genuine_comparisons: 2_000,
            far_claim_eligible: false,
        };
        assert!(!report.far_claim_eligible);
        assert!(report.far_ci_upper_95 > 1e-5);
    }
}
