// SPDX-License-Identifier: GPL-3.0-only
//! Gate 1: Differential energy.
//!
//! A screen replay (LCD/OLED displaying a face video) emits almost nothing at
//! 850 nm and reflects specularly, so `D = On − Off` collapses to near-zero.
//! A real face lit by the emitter produces a strong positive differential.
//!
//! Statistic: mean of `D` normalised by the mean of the `On` frame.
//! Pass if `|mean(D)| / mean(On) ≥ min_energy_ratio`.

use crate::GateResult;

/// Score normalisation: `score = (|raw| / SCORE_FULL_SCALE).clamp(0, 1)`.
/// A ratio of `SCORE_FULL_SCALE` (0.25) maps to score 1.0. Exposed so the
/// calibration bridge can invert the mapping without a magic number.
pub const SCORE_FULL_SCALE: f32 = 0.25;

/// Compute the differential-energy gate.
///
/// `d` is the strobe differential (`On − Off`) as `f32` values.
/// `on_mean` is the mean of the illuminated 8-bit frame (use [`mean_u8`]).
/// `min_ratio` is the pass threshold from [`crate::PadConfig`].
pub fn differential_energy(d: &[f32], on_mean: f32, min_ratio: f32) -> GateResult {
    if d.is_empty() || on_mean <= 0.0 {
        return GateResult::fail(0.0, 0.0);
    }
    let mean_d: f32 = d.iter().copied().sum::<f32>() / d.len() as f32;
    let raw = mean_d / on_mean;
    // Normalise score into [0, 1]: 0 at ratio 0, 1 at ratio >= SCORE_FULL_SCALE.
    let score = (raw.abs() / SCORE_FULL_SCALE).clamp(0.0, 1.0);
    let pass = raw.abs() >= min_ratio;
    if pass {
        GateResult::pass(score, raw)
    } else {
        GateResult::fail(score, raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_differential_fails() {
        let d = vec![0.0; 100];
        let r = differential_energy(&d, 100.0, 0.04);
        assert!(!r.pass);
        assert!(r.score < 0.01);
    }

    #[test]
    fn strong_differential_passes() {
        let d = vec![40.0; 100];
        let r = differential_energy(&d, 100.0, 0.04);
        assert!(r.pass);
        assert!(r.score > 0.9);
    }

    #[test]
    fn negative_differential_uses_abs() {
        // Off brighter than On (shouldn't happen with real strobe but test symmetry)
        let d = vec![-40.0; 100];
        let r = differential_energy(&d, 100.0, 0.04);
        assert!(r.pass);
    }

    #[test]
    fn empty_d_fails() {
        let r = differential_energy(&[], 100.0, 0.04);
        assert!(!r.pass);
    }

    #[test]
    fn zero_on_mean_fails() {
        let d = vec![40.0; 100];
        let r = differential_energy(&d, 0.0, 0.04);
        assert!(!r.pass);
    }

    #[test]
    fn borderline_threshold() {
        // ratio = 0.04 exactly → pass (≥ threshold)
        let d = vec![4.0; 100];
        let r = differential_energy(&d, 100.0, 0.04);
        assert!(r.pass);
    }

    #[test]
    fn just_below_threshold_fails() {
        let d = vec![3.0; 100];
        let r = differential_energy(&d, 100.0, 0.04);
        assert!(!r.pass);
    }

    #[test]
    fn mean_u8_helper() {
        assert_eq!(crate::mean_u8(&[100, 110, 90]), 100.0);
    }
}
