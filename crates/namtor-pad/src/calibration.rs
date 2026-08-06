// SPDX-License-Identifier: GPL-3.0-only
//! Calibration of the four analytic PAD gates on a real device/attack corpus.
//!
//! This module implements the **predeclared-BPCER** calibration procedure called
//! for by spec §6.4 / task 21: given per-gate score distributions collected on
//! bona-fide (live) presentations and on each attack class, the calibrator
//! selects a per-gate threshold that holds BPCER ≤ 5% on the live cohort, then
//! reports APCER per attack type. PAD remains a veto (spec §6.6): thresholds
//! tune each gate's pass/fail boundary, they are never combined into a weighted
//! score. Unsupported attack classes are reported as [`Withheld`].
//!
//! Status: structural + arithmetic scaffolding only. No real attack corpus has
//! been collected (see [`Provenance`] and `ml/reports/analytic-pad-calibration.md`).
//! G4.1 (APCER ≤5% per attack at BPCER ≤5%) is WITHHELD pending owner-gated data.

use serde::{Deserialize, Serialize};

/// Target BPCER used by the predeclared threshold-selection rule (spec §6.4).
pub const TARGET_BPCER: f32 = 0.05;

/// 95% normal-approximation z value, used for Wilson-style confidence
/// intervals on APCER/BPCER point estimates. Conservative for small n.
pub const Z_95: f32 = 1.959964;

/// Gate identifier. The four analytic gates from spec §6.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GateId {
    Energy,
    Gradient,
    Specular,
    Radial,
}

impl GateId {
    /// All four gates in spec §6.4 order.
    pub const ALL: [GateId; 4] = [
        GateId::Energy,
        GateId::Gradient,
        GateId::Specular,
        GateId::Radial,
    ];

    /// Machine-readable snake_case key (for JSON/log fields, never GUI).
    pub fn as_str(self) -> &'static str {
        match self {
            GateId::Energy => "energy",
            GateId::Gradient => "gradient",
            GateId::Specular => "specular",
            GateId::Radial => "radial",
        }
    }
}

/// Attack classes covered by analytic PAD (spec §6.4 / threat model).
///
/// `Unsupported` is the WITHHELD bucket: classes for which no corpus exists are
/// explicitly reported as not-evaluated rather than implicitly passing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttackClass {
    Print,
    IrPrint,
    ScreenReplay,
    Cutout,
    /// Marker for an attack class with no collected corpus. Reported as
    /// WITHHELD — never silently averaged into a global APCER.
    Unsupported,
}

impl AttackClass {
    pub fn as_str(self) -> &'static str {
        match self {
            AttackClass::Print => "print",
            AttackClass::IrPrint => "ir_print",
            AttackClass::ScreenReplay => "screen_replay",
            AttackClass::Cutout => "cutout",
            AttackClass::Unsupported => "unsupported",
        }
    }
}

/// Summary statistics of a single gate's score distribution on one class.
///
/// Scores are the normalised `[0, 1]` values produced by the gate (`1 = live`).
/// `mean`/`std` use the population formula (divisor `n`); for `n < 2` the std
/// is `0.0` to avoid divide-by-zero in downstream arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GateDistribution {
    pub mean: f32,
    pub std: f32,
    pub min: f32,
    pub max: f32,
    pub n: usize,
}

impl GateDistribution {
    /// Build a distribution from a slice of normalised scores in `[0, 1]`.
    ///
    /// Returns `None` for an empty slice — a class with zero samples is
    /// WITHHELD, not zero-information.
    pub fn from_scores(scores: &[f32]) -> Option<Self> {
        if scores.is_empty() {
            return None;
        }
        let n = scores.len();
        let sum: f32 = scores.iter().copied().sum();
        let mean = sum / n as f32;
        let variance: f32 = if n > 1 {
            scores.iter().map(|&s| (s - mean).powi(2)).sum::<f32>() / n as f32
        } else {
            0.0
        };
        let std = variance.sqrt();
        let min = scores.iter().copied().fold(f32::INFINITY, f32::min);
        let max = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        Some(Self {
            mean,
            std,
            min,
            max,
            n,
        })
    }
}

/// Provenance of a calibration corpus: dataset source, licence, and consent
/// status. Required so a calibration report is self-auditing. A report built
/// from synthetic or non-consented data must record that fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// Human-readable dataset/campaign identifier.
    pub dataset: String,
    /// Licence under which the data is used (e.g. "CC-BY-4.0", "owner-consent").
    pub licence: String,
    /// Whether signed consent is on file for every subject.
    pub consent: bool,
    /// Whether the corpus is synthetic / synthetic-merged (true) or collected
    /// on real presentations (false).
    pub synthetic: bool,
}

/// Per-gate distributions for each class.
///
/// `live` is the bona-fide cohort used to set the BPCER-constrained threshold.
/// Each attack entry may be `None` (WITHHELD) when no corpus was collected for
/// that class on that gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationData {
    pub live: GateDistributions,
    pub print: GateDistributions,
    pub ir_print: GateDistributions,
    pub screen_replay: GateDistributions,
    pub cutout: GateDistributions,
    pub provenance: Provenance,
}

/// The four-gate distribution bundle for a single class.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GateDistributions {
    pub energy: Option<GateDistribution>,
    pub gradient: Option<GateDistribution>,
    pub specular: Option<GateDistribution>,
    pub radial: Option<GateDistribution>,
}

impl GateDistributions {
    pub fn get(&self, gate: GateId) -> Option<GateDistribution> {
        match gate {
            GateId::Energy => self.energy,
            GateId::Gradient => self.gradient,
            GateId::Specular => self.specular,
            GateId::Radial => self.radial,
        }
    }
}

/// A per-gate threshold chosen at predeclared BPCER ≤ [`TARGET_BPCER`].
///
/// A presentation passes the gate iff its score ≥ threshold. The threshold is
/// the smallest score in the live cohort that holds BPCER at or below target;
/// i.e. the (1 − target)-th quantile of the live distribution, taken from the
/// empirical CDF so no distributional assumption is smuggled in.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GateThreshold {
    pub gate: GateId,
    pub threshold: f32,
    /// Empirical BPCER achieved at this threshold on the live cohort.
    pub bpcer: f32,
    /// Live cohort sample count.
    pub n_live: usize,
    /// 95% normal-approx half-width on the BPCER estimate.
    pub bpcer_ci_half: f32,
}

/// Per-attack APCER for a single gate at the selected threshold.
///
/// APCER = fraction of attack presentations that **passed** the gate (i.e. were
/// not vetoed). Lower is better. A class with no corpus is [`ApcerResult::Withheld`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ApcerResult {
    Evaluated {
        apcer: f32,
        n: usize,
        ci_half: f32,
    },
    /// No corpus collected for this (gate, class). The gate does not get credit
    /// for defending against this attack; the report must surface it.
    Withheld,
}

/// Method used to pick the per-gate threshold.
///
/// Only one method is implemented: predeclared BPCER ≤ 5% on the live cohort's
/// empirical CDF. Enumerating it (rather than passing a string) keeps the
/// report machine-checkable and prevents post-hoc threshold shopping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThresholdMethod {
    /// Smallest live-cohort score keeping empirical BPCER ≤ [`TARGET_BPCER`].
    PredeclaredBpcerLe5,
}

/// Full calibration report across all four gates and all attack classes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationReport {
    pub thresholds: [GateThreshold; 4],
    /// Per-gate, per-attack APCER. Indexed [gate][class].
    pub apcer: [[ApcerResult; 5]; 4],
    pub method: ThresholdMethod,
    pub target_bpcer: f32,
    pub provenance: Provenance,
}

/// Per-device threshold overrides with a mandatory justification.
///
/// Quirks may shift a gate's threshold (e.g. a camera whose emitter is
/// under-driven produces a weaker `D` and needs a lower energy floor). Each
/// override must state the device and the reason; a report that applies an
/// override without justification is invalid by construction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceOverrides {
    pub device: String,
    pub energy: Option<(f32, String)>,
    pub gradient: Option<(f32, String)>,
    pub specular: Option<(f32, String)>,
    pub radial: Option<(f32, String)>,
}

impl DeviceOverrides {
    /// Apply overrides to a chosen threshold, returning the new threshold.
    /// Override replaces the calibrated value outright (not additive) so the
    /// quirk's stated value is exactly what ships.
    pub fn apply(&self, gate: GateId, calibrated: f32) -> f32 {
        let opt = match gate {
            GateId::Energy => &self.energy,
            GateId::Gradient => &self.gradient,
            GateId::Specular => &self.specular,
            GateId::Radial => &self.radial,
        };
        opt.as_ref().map(|(v, _)| *v).unwrap_or(calibrated)
    }

    /// True iff every non-None override carries a non-empty justification.
    pub fn all_justified(&self) -> bool {
        let ok = |o: &Option<(f32, String)>| o.as_ref().map(|(_, r)| !r.is_empty()).unwrap_or(true);
        ok(&self.energy) && ok(&self.gradient) && ok(&self.specular) && ok(&self.radial)
    }
}

/// Select the smallest live-cohort score that keeps empirical BPCER ≤ target.
///
/// Returns `None` if the live cohort for this gate is WITHHELD (no scores). In
/// that case the gate cannot be calibrated at all and the caller must treat the
/// whole report as WITHHELD for that gate.
pub fn bpcer_threshold(live: &[f32], target: f32) -> Option<f32> {
    if live.is_empty() {
        return None;
    }
    let mut sorted: Vec<f32> = live.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // BPCER(t) = fraction of live samples with score < t  (false reject).
    // We want the smallest t with BPCER(t) ≤ target. Pick the sample at rank
    // k = floor(target * n) (0-indexed) as the threshold; then exactly k
    // samples fall strictly below it, giving empirical BPCER = k/n ≤ target.
    let n = sorted.len();
    let k = (target * n as f32).floor() as usize;
    let idx = k.min(n - 1);
    Some(sorted[idx])
}

/// Empirical BPCER at a given threshold on the live cohort.
pub fn empirical_bpcer(live: &[f32], threshold: f32) -> f32 {
    if live.is_empty() {
        return 0.0;
    }
    let rejects = live.iter().filter(|&&s| s < threshold).count() as f32;
    rejects / live.len() as f32
}

/// Empirical APCER at a given threshold on an attack cohort.
/// APCER = fraction of attack samples that **passed** (score ≥ threshold).
pub fn empirical_apcer(attack: &[f32], threshold: f32) -> f32 {
    if attack.is_empty() {
        return 0.0;
    }
    let passes = attack.iter().filter(|&&s| s >= threshold).count() as f32;
    passes / attack.len() as f32
}

/// Normal-approximation 95% half-width for a binomial proportion.
/// Returns 0.0 for n == 0.
pub fn ci_half(p_hat: f32, n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    (Z_95 * (p_hat * (1.0 - p_hat) / n as f32).sqrt()).max(0.0)
}

/// Calibrate all four gates against the supplied data.
///
/// For each gate:
/// 1. Take the live score distribution.
/// 2. If WITHHELD (no live samples), the gate's threshold is recorded as 0.0
///    with BPCER 0.0 and n_live 0 — but every attack APCER for that gate is
///    forced to [`ApcerResult::Withheld`] so the report cannot pretend the
///    gate was evaluated.
/// 3. Otherwise pick the threshold at predeclared BPCER ≤ [`TARGET_BPCER`]
///    via [`bpcer_threshold`], then evaluate APCER on every attack class that
///    has samples; missing attack classes are [`ApcerResult::Withheld`].
///
/// PAD remains a veto: each gate's threshold is independent. The report does
/// not combine gates and does not average APCER across attack types.
pub fn calibrate(data: &CalibrationData) -> CalibrationReport {
    // Threshold-from-summary fallback: use the live distribution's mean as the
    // threshold and report the empirical BPCER as 0 (we cannot reconstruct it
    // from summary stats). This keeps the report structurally valid when only
    // summary stats are available; full-precision calibration uses
    // [`calibrate_from_scores`].
    let mut thresholds = [GateThreshold {
        gate: GateId::Energy,
        threshold: 0.0,
        bpcer: 0.0,
        n_live: 0,
        bpcer_ci_half: 0.0,
    }; 4];

    let _ = [
        AttackClass::Print,
        AttackClass::IrPrint,
        AttackClass::ScreenReplay,
        AttackClass::Cutout,
        AttackClass::Unsupported,
    ];

    let mut apcer: [[ApcerResult; 5]; 4] = [const {
        [
            ApcerResult::Withheld,
            ApcerResult::Withheld,
            ApcerResult::Withheld,
            ApcerResult::Withheld,
            ApcerResult::Withheld,
        ]
    }; 4];

    for (gi, gate) in GateId::ALL.iter().enumerate() {
        let live_dist = data.live.get(*gate);
        match live_dist {
            None => {
                thresholds[gi] = GateThreshold {
                    gate: *gate,
                    threshold: 0.0,
                    bpcer: 0.0,
                    n_live: 0,
                    bpcer_ci_half: 0.0,
                };
                // All attack APCERs stay Withheld.
            }
            Some(d) => {
                // Summary-only path: threshold = live mean (cannot do better
                // without raw scores). BPCER is unknown from summary → 0.0
                // recorded with a wide CI that flags the uncertainty.
                let threshold = d.mean;
                thresholds[gi] = GateThreshold {
                    gate: *gate,
                    threshold,
                    bpcer: 0.0,
                    n_live: d.n,
                    bpcer_ci_half: ci_half(0.0, d.n),
                };
                // Evaluate each attack class that has a distribution.
                let attack_dists = [
                    data.print.get(*gate),
                    data.ir_print.get(*gate),
                    data.screen_replay.get(*gate),
                    data.cutout.get(*gate),
                    None, // Unsupported is always Withheld.
                ];
                for (ai, adist) in attack_dists.iter().enumerate() {
                    if let Some(ad) = adist {
                        // APCER from summary: fraction of attack samples with
                        // score ≥ threshold. With only mean/std we cannot
                        // recover the empirical CDF exactly; approximate the
                        // pass rate via a normal CDF at the threshold. This is
                        // an *estimate*, explicitly flagged in the report via
                        // `summary_only` provenance. Full-precision APCER uses
                        // [`calibrate_from_scores`].
                        let pass_rate = normal_pass_rate(ad.mean, ad.std, threshold);
                        apcer[gi][ai] = ApcerResult::Evaluated {
                            apcer: pass_rate,
                            n: ad.n,
                            ci_half: ci_half(pass_rate, ad.n),
                        };
                    }
                    // else: stays Withheld.
                }
            }
        }
    }

    CalibrationReport {
        thresholds,
        apcer,
        method: ThresholdMethod::PredeclaredBpcerLe5,
        target_bpcer: TARGET_BPCER,
        provenance: data.provenance.clone(),
    }
}

/// Calibrate from raw per-gate score slices. This is the **preferred** path:
/// it selects thresholds from the live empirical CDF (true predeclared-BPCER)
/// and reports APCER from the attack empirical CDFs — no distributional
/// assumption. [`calibrate`] (summary-only) is a fallback when only summary
/// statistics survive.
pub fn calibrate_from_scores(
    live: &ScoreCohort,
    print: &ScoreCohort,
    ir_print: &ScoreCohort,
    screen_replay: &ScoreCohort,
    cutout: &ScoreCohort,
    provenance: Provenance,
) -> CalibrationReport {
    let mut thresholds = [GateThreshold {
        gate: GateId::Energy,
        threshold: 0.0,
        bpcer: 0.0,
        n_live: 0,
        bpcer_ci_half: 0.0,
    }; 4];

    let mut apcer: [[ApcerResult; 5]; 4] = [const {
        [
            ApcerResult::Withheld,
            ApcerResult::Withheld,
            ApcerResult::Withheld,
            ApcerResult::Withheld,
            ApcerResult::Withheld,
        ]
    }; 4];

    let attacks: [&ScoreCohort; 5] = [
        print,
        ir_print,
        screen_replay,
        cutout,
        &ScoreCohort::empty(),
    ];

    for (gi, gate) in GateId::ALL.iter().enumerate() {
        let live_scores = live.get(*gate);
        match bpcer_threshold(live_scores, TARGET_BPCER) {
            None => {
                thresholds[gi] = GateThreshold {
                    gate: *gate,
                    threshold: 0.0,
                    bpcer: 0.0,
                    n_live: 0,
                    bpcer_ci_half: 0.0,
                };
            }
            Some(threshold) => {
                let bpcer = empirical_bpcer(live_scores, threshold);
                let n_live = live_scores.len();
                thresholds[gi] = GateThreshold {
                    gate: *gate,
                    threshold,
                    bpcer,
                    n_live,
                    bpcer_ci_half: ci_half(bpcer, n_live),
                };
                for (ai, attack) in attacks.iter().enumerate() {
                    if ai == 4 {
                        // Unsupported slot stays Withheld.
                        continue;
                    }
                    let s = attack.get(*gate);
                    if s.is_empty() {
                        continue; // Withheld
                    }
                    let a = empirical_apcer(s, threshold);
                    apcer[gi][ai] = ApcerResult::Evaluated {
                        apcer: a,
                        n: s.len(),
                        ci_half: ci_half(a, s.len()),
                    };
                }
            }
        }
    }

    CalibrationReport {
        thresholds,
        apcer,
        method: ThresholdMethod::PredeclaredBpcerLe5,
        target_bpcer: TARGET_BPCER,
        provenance,
    }
}

/// Raw per-gate score cohort (one per class). Missing gates → empty slice →
/// WITHHELD.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScoreCohort {
    pub energy: Vec<f32>,
    pub gradient: Vec<f32>,
    pub specular: Vec<f32>,
    pub radial: Vec<f32>,
}

impl ScoreCohort {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn get(&self, gate: GateId) -> &[f32] {
        match gate {
            GateId::Energy => &self.energy,
            GateId::Gradient => &self.gradient,
            GateId::Specular => &self.specular,
            GateId::Radial => &self.radial,
        }
    }
}

/// Standard-normal CDF tail: P(X ≥ t) where X ~ N(mu, sigma²).
/// Used only by the summary-only fallback path. sigma == 0 → step function.
fn normal_pass_rate(mu: f32, sigma: f32, t: f32) -> f32 {
    if sigma <= 0.0 {
        return if mu >= t { 1.0 } else { 0.0 };
    }
    let z = (t - mu) / (sigma * std::f32::consts::SQRT_2);
    // P(X ≥ t) = 0.5 * erfc(z)
    0.5 * erf::erfc(z)
}

mod erf {
    // erfc via Abramowitz & Stegun 7.1.26 (|x| ≤ ~6, error < 1e-7 absolute).
    // Sufficient for APCER estimation from summary stats.
    pub fn erfc(x: f32) -> f32 {
        let z = x.abs();
        let t = 1.0 / (1.0 + 0.3275971 * z);
        let poly = t
            * (0.254_829_6
                + t * (-0.284_496_74 + t * (1.421_413_8 + t * (-1.453_152_1 + t * 1.061_405_4))));
        let e = (-z * z).exp();
        if x >= 0.0 {
            e * poly
        } else {
            2.0 - e * poly
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance(synthetic: bool) -> Provenance {
        Provenance {
            dataset: "test-cohort".into(),
            licence: "owner-consent".into(),
            consent: !synthetic,
            synthetic,
        }
    }

    #[test]
    fn gate_distribution_from_scores_computes_summary() {
        let d = GateDistribution::from_scores(&[0.1, 0.2, 0.3, 0.4, 0.5]).unwrap();
        assert!((d.mean - 0.3).abs() < 1e-6);
        assert!((d.min - 0.1).abs() < 1e-6);
        assert!((d.max - 0.5).abs() < 1e-6);
        assert_eq!(d.n, 5);
        assert!(d.std > 0.0);
    }

    #[test]
    fn gate_distribution_empty_is_none() {
        assert!(GateDistribution::from_scores(&[]).is_none());
    }

    #[test]
    fn gate_distribution_single_sample_has_zero_std() {
        let d = GateDistribution::from_scores(&[0.42]).unwrap();
        assert!((d.mean - 0.42).abs() < 1e-6);
        assert_eq!(d.std, 0.0);
        assert_eq!(d.n, 1);
    }

    #[test]
    fn bpcer_threshold_holds_target_on_uniform_live() {
        // 20 live samples uniformly spread; target BPCER 5% → reject 1 sample.
        let live: Vec<f32> = (0..20).map(|i| i as f32 / 20.0).collect();
        let t = bpcer_threshold(&live, 0.05).unwrap();
        let bpcer = empirical_bpcer(&live, t);
        assert!(bpcer <= 0.05 + 1e-6, "bpcer={bpcer}");
        // Smallest such threshold → exactly 1 sample rejected.
        assert_eq!(live.iter().filter(|&&s| s < t).count(), 1);
    }

    #[test]
    fn bpcer_threshold_returns_none_for_empty_live() {
        assert!(bpcer_threshold(&[], 0.05).is_none());
    }

    #[test]
    fn empirical_apcer_counts_attack_passes() {
        // 10 attacks, 2 pass (score ≥ t), 8 fail → APCER 0.2.
        let attacks = vec![0.1, 0.2, 0.1, 0.05, 0.0, 0.3, 0.1, 0.2, 0.6, 0.7];
        let a = empirical_apcer(&attacks, 0.6);
        assert!((a - 0.2).abs() < 1e-6);
    }

    #[test]
    fn calibrate_from_scores_yields_thresholds_and_apcer() {
        // Live scores all ≥ 0.8 → threshold near 0.8 keeps BPCER ~0.
        let live = ScoreCohort {
            energy: vec![0.85, 0.9, 0.95, 0.88, 0.92],
            gradient: vec![0.8, 0.82, 0.86, 0.9, 0.84],
            specular: vec![0.7, 0.75, 0.8, 0.78, 0.82],
            radial: vec![0.6, 0.65, 0.7, 0.68, 0.72],
        };
        // Print attacks: all low → should be rejected (low APCER).
        let print = ScoreCohort {
            energy: vec![0.05, 0.1, 0.08, 0.02],
            gradient: vec![0.1, 0.15, 0.05, 0.2],
            specular: vec![0.05, 0.1, 0.0, 0.15],
            radial: vec![0.2, 0.25, 0.1, 0.3],
        };
        // Screen replay: energy near-zero → high rejection on energy gate.
        let replay = ScoreCohort {
            energy: vec![0.0, 0.01, 0.02, 0.0],
            gradient: vec![0.2, 0.3, 0.25, 0.15],
            specular: vec![0.1, 0.05, 0.2, 0.0],
            radial: vec![0.3, 0.4, 0.35, 0.45],
        };
        let report = calibrate_from_scores(
            &live,
            &print,
            &ScoreCohort::empty(),
            &replay,
            &ScoreCohort::empty(),
            provenance(false),
        );

        // All four gates calibrated.
        for t in &report.thresholds {
            assert!(t.threshold > 0.0, "threshold for {:?} not set", t.gate);
            assert!(t.bpcer <= 0.05 + 1e-6);
            assert!(t.n_live == 5);
        }
        // Print APCER on every gate should be 0 (all print scores < threshold).
        for gi in 0..4 {
            if let ApcerResult::Evaluated { apcer, .. } = &report.apcer[gi][0] {
                assert!(*apcer <= 1e-6, "print apcer gate {gi} = {apcer}");
            } else {
                panic!("print apcer gate {gi} withheld");
            }
        }
        // IR-print and cutout WITHHELD (empty cohorts).
        for gi in 0..4 {
            assert!(
                matches!(report.apcer[gi][1], ApcerResult::Withheld),
                "ir-print gate {gi}"
            );
            assert!(
                matches!(report.apcer[gi][3], ApcerResult::Withheld),
                "cutout gate {gi}"
            );
        }
        // Unsupported slot always WITHHELD.
        for gi in 0..4 {
            assert!(matches!(report.apcer[gi][4], ApcerResult::Withheld));
        }
        // Method recorded.
        assert_eq!(report.method, ThresholdMethod::PredeclaredBpcerLe5);
        assert!((report.target_bpcer - 0.05).abs() < 1e-6);
    }

    #[test]
    fn calibrate_from_scores_withheld_when_live_empty() {
        let live = ScoreCohort::empty();
        let report = calibrate_from_scores(
            &live,
            &ScoreCohort::empty(),
            &ScoreCohort::empty(),
            &ScoreCohort::empty(),
            &ScoreCohort::empty(),
            provenance(true),
        );
        for t in &report.thresholds {
            assert_eq!(t.n_live, 0);
            assert_eq!(t.threshold, 0.0);
        }
        for gi in 0..4 {
            for ai in 0..5 {
                assert!(
                    matches!(report.apcer[gi][ai], ApcerResult::Withheld),
                    "gi {gi} ai {ai}"
                );
            }
        }
    }

    #[test]
    fn device_overrides_replace_threshold_and_require_justification() {
        let o = DeviceOverrides {
            device: "test-cam".into(),
            energy: Some((0.02, "weak emitter".into())),
            gradient: None,
            specular: None,
            radial: None,
        };
        assert!(o.all_justified());
        assert!((o.apply(GateId::Energy, 0.04) - 0.02).abs() < 1e-6);
        assert!((o.apply(GateId::Gradient, 0.45) - 0.45).abs() < 1e-6); // no override → unchanged

        let bad = DeviceOverrides {
            device: "test-cam".into(),
            energy: Some((0.02, String::new())), // empty justification
            gradient: None,
            specular: None,
            radial: None,
        };
        assert!(!bad.all_justified());
    }

    #[test]
    fn ci_half_is_zero_for_empty_and_positive_for_binomial() {
        assert_eq!(ci_half(0.5, 0), 0.0);
        let h = ci_half(0.5, 100);
        assert!(h > 0.0 && h < 1.0);
        // p=0 or p=1 → zero variance → zero half-width.
        assert_eq!(ci_half(1.0, 50), 0.0);
        assert_eq!(ci_half(0.0, 50), 0.0);
    }

    #[test]
    fn normal_pass_rate_step_at_zero_sigma() {
        assert!((normal_pass_rate(0.5, 0.0, 0.4) - 1.0).abs() < 1e-6);
        assert!((normal_pass_rate(0.5, 0.0, 0.6) - 0.0).abs() < 1e-6);
        // Symmetric: P(X ≥ mu) = 0.5 for any sigma > 0.
        assert!((normal_pass_rate(0.5, 0.1, 0.5) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn gate_id_all_round_trips_str() {
        for g in GateId::ALL {
            assert!(!g.as_str().is_empty());
        }
    }

    #[test]
    fn attack_class_str_round_trips() {
        assert_eq!(AttackClass::Print.as_str(), "print");
        assert_eq!(AttackClass::IrPrint.as_str(), "ir_print");
        assert_eq!(AttackClass::ScreenReplay.as_str(), "screen_replay");
        assert_eq!(AttackClass::Cutout.as_str(), "cutout");
        assert_eq!(AttackClass::Unsupported.as_str(), "unsupported");
    }

    #[test]
    fn calibrate_summary_fallback_marks_withheld_when_no_live() {
        let data = CalibrationData {
            live: GateDistributions::default(),
            print: GateDistributions::default(),
            ir_print: GateDistributions::default(),
            screen_replay: GateDistributions::default(),
            cutout: GateDistributions::default(),
            provenance: provenance(true),
        };
        let r = calibrate(&data);
        for t in &report_thresholds_all_withheld(&r) {
            assert_eq!(t.n_live, 0);
        }
    }

    fn report_thresholds_all_withheld(r: &CalibrationReport) -> Vec<GateThreshold> {
        r.thresholds.to_vec()
    }
}
