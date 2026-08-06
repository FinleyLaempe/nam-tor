// SPDX-License-Identifier: GPL-3.0-only
//! Analytic presentation-attack detection gates.
//!
//! Implements the four physics-first gates from spec §6.4 operating on the
//! strobe differential `D = On − Off`:
//!
//! 1. **Differential energy** — a screen replay yields near-zero `D` because
//!    LCD/OLED panels emit almost nothing at 850 nm and reflect specularly.
//! 2. **Gradient consistency** — a real face exhibits subsurface scattering
//!    producing a smooth spatial gradient field; a flat print does not.
//! 3. **Specular highlight** — skin under a point emitter produces a compact
//!    specular hotspot whose centroid and extent are bounded. Flat media
//!    produce either no highlight or a diffuse one.
//! 4. **Radial falloff** — inverse-square illumination yields 1/d² intensity
//!    falloff from the nearest point (nose) outward. A flat photograph
//!    illuminates uniformly.
//!
//! Each gate returns a [`GateResult`] with pass/fail and a normalised score in
//! `[0, 1]` (0 = spoof, 1 = live). [`PadVerdict`] aggregates the four: **PAD is
//! a veto, not a weighted term** (spec §6.6) — any failing gate refuses.

pub mod energy;
pub mod gradient;
pub mod radial;
pub mod specular;

pub mod calibration;

pub use energy::differential_energy;
pub use gradient::gradient_consistency;
pub use radial::radial_falloff;
pub use specular::specular_highlight;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Error returned when PAD input is malformed.
#[derive(Debug, Error)]
pub enum PadError {
    #[error("frame dimensions mismatch: on={on_w}x{on_h}, off={off_w}x{off_h}")]
    DimensionMismatch {
        on_w: u32,
        on_h: u32,
        off_w: u32,
        off_h: u32,
    },
    #[error("frame is empty")]
    Empty,
}

/// Result of a single analytic gate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GateResult {
    /// Whether this gate passes.
    pub pass: bool,
    /// Normalised score in `[0, 1]` (0 = spoof-like, 1 = live-like).
    pub score: f32,
    /// Gate-specific raw statistic (for logging/audit; never biometric).
    pub raw: f32,
}

impl GateResult {
    pub const fn pass(score: f32, raw: f32) -> Self {
        Self {
            pass: true,
            score,
            raw,
        }
    }
    pub const fn fail(score: f32, raw: f32) -> Self {
        Self {
            pass: false,
            score,
            raw,
        }
    }
}

/// Aggregated verdict across all four gates.
///
/// PAD is a **veto**: any failing gate refuses. Scores are reported for
/// audit/logging but never combined into a weighted term (spec §6.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PadVerdict {
    pub energy: GateResult,
    pub gradient: GateResult,
    pub specular: GateResult,
    pub radial: GateResult,
    /// `true` only if every gate passed.
    pub live: bool,
}

impl PadVerdict {
    pub fn live(&self) -> bool {
        self.live
    }
}

/// Configuration thresholds for the four gates.
///
/// Defaults are derived from spec §6.4 and are defined on **normalised**
/// statistics (spec §6.4 closing remark). Thresholds are not exposed in any
/// GUI (spec hard rule).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PadConfig {
    /// Minimum mean energy of `D` relative to the `On` frame, in `[0, 1]`.
    /// Below this the frame is dominated by noise / a screen replay.
    pub min_energy_ratio: f32,
    /// Minimum gradient-consistency score in `[0, 1]`.
    pub min_gradient_score: f32,
    /// Minimum specular compactness in `[0, 1]` (0 = no hotspot, 1 = tight).
    pub min_specular_compactness: f32,
    /// Maximum radial-falloff deviation from 1/d² model, normalised in `[0, 1]`.
    /// 0 = perfect fit, 1 = no fit. Pass if deviation ≤ threshold.
    pub max_radial_deviation: f32,
}

impl Default for PadConfig {
    fn default() -> Self {
        // Conservative defaults; tuned on R4/R5 cohort data in M9.
        Self {
            min_energy_ratio: 0.04,
            min_gradient_score: 0.45,
            min_specular_compactness: 0.30,
            max_radial_deviation: 0.55,
        }
    }
}

/// Compute the strobe differential `D = On − Off` as a `f32` image.
///
/// Both frames must be single-channel 8-bit (`Grey8`). Returns a row-major
/// `Vec<f32>` of length `w*h` with values in `[-255, 255]`.
pub fn strobe_difference(on: &[u8], off: &[u8], w: u32, h: u32) -> Result<Vec<f32>, PadError> {
    let n = (w as usize) * (h as usize);
    if on.len() != n || off.len() != n {
        return Err(PadError::DimensionMismatch {
            on_w: w,
            on_h: h,
            off_w: w,
            off_h: h,
        });
    }
    if n == 0 {
        return Err(PadError::Empty);
    }
    let mut d = Vec::with_capacity(n);
    for i in 0..n {
        d.push(on[i] as f32 - off[i] as f32);
    }
    Ok(d)
}

/// Mean pixel intensity of a single-channel 8-bit frame.
pub fn mean_u8(frame: &[u8]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum: usize = frame.iter().map(|&v| v as usize).sum();
    sum as f32 / frame.len() as f32
}

/// Run all four analytic gates on the strobe differential `D`.
///
/// `on_mean` is the mean of the illuminated (`On`) frame — used to normalise
/// the energy gate. `face_bbox` is the detected face bounding box in pixels
/// `(x, y, w, h)` relative to the full frame; gates operate on the face region
/// of `D` where applicable.
pub fn run_analytic_pad(
    d: &[f32],
    w: u32,
    h: u32,
    on_mean: f32,
    face_bbox: Option<(u32, u32, u32, u32)>,
    cfg: &PadConfig,
) -> Result<PadVerdict, PadError> {
    let n = (w as usize) * (h as usize);
    if d.len() != n {
        return Err(PadError::DimensionMismatch {
            on_w: w,
            on_h: h,
            off_w: w,
            off_h: h,
        });
    }
    if n == 0 {
        return Err(PadError::Empty);
    }

    let energy = differential_energy(d, on_mean, cfg.min_energy_ratio);
    let gradient = gradient_consistency(d, w, h, face_bbox, cfg.min_gradient_score);
    let specular = specular_highlight(d, w, h, face_bbox, cfg.min_specular_compactness);
    let radial = radial_falloff(d, w, h, face_bbox, cfg.max_radial_deviation);

    let live = energy.pass && gradient.pass && specular.pass && radial.pass;
    Ok(PadVerdict {
        energy,
        gradient,
        specular,
        radial,
        live,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant_frame(v: u8, n: usize) -> Vec<u8> {
        vec![v; n]
    }

    #[test]
    fn strobe_difference_of_identical_frames_is_zero() {
        let w = 4u32;
        let h = 4u32;
        let on = constant_frame(100, (w * h) as usize);
        let off = constant_frame(100, (w * h) as usize);
        let d = strobe_difference(&on, &off, w, h).unwrap();
        assert!(d.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn strobe_difference_subtracts_correctly() {
        let w = 2u32;
        let h = 2u32;
        let on = vec![200, 100, 50, 0];
        let off = vec![50, 50, 50, 50];
        let d = strobe_difference(&on, &off, w, h).unwrap();
        assert_eq!(d, vec![150.0, 50.0, 0.0, -50.0]);
    }

    #[test]
    fn dimension_mismatch_is_an_error() {
        let on = vec![0u8; 4];
        let off = vec![0u8; 9];
        let err = strobe_difference(&on, &off, 2, 2).unwrap_err();
        assert!(matches!(err, PadError::DimensionMismatch { .. }));
    }

    #[test]
    fn empty_frames_are_an_error() {
        let err = strobe_difference(&[], &[], 0, 0).unwrap_err();
        assert!(matches!(err, PadError::Empty));
    }

    #[test]
    fn mean_u8_of_constant_frame() {
        assert_eq!(mean_u8(&[100, 100, 100]), 100.0);
        assert_eq!(mean_u8(&[0, 255]), 127.5);
        assert_eq!(mean_u8(&[]), 0.0);
    }

    #[test]
    fn pad_verdict_live_requires_all_gates_pass() {
        let v = PadVerdict {
            energy: GateResult::pass(0.9, 0.1),
            gradient: GateResult::pass(0.8, 0.2),
            specular: GateResult::pass(0.7, 0.3),
            radial: GateResult::pass(0.6, 0.4),
            live: true,
        };
        assert!(v.live());

        let v2 = PadVerdict {
            energy: GateResult::fail(0.01, 0.0),
            gradient: GateResult::pass(0.8, 0.2),
            specular: GateResult::pass(0.7, 0.3),
            radial: GateResult::pass(0.6, 0.4),
            live: false,
        };
        assert!(!v2.live());
    }

    #[test]
    fn default_config_thresholds_are_in_range() {
        let cfg = PadConfig::default();
        assert!(cfg.min_energy_ratio > 0.0 && cfg.min_energy_ratio < 1.0);
        assert!(cfg.min_gradient_score > 0.0 && cfg.min_gradient_score < 1.0);
        assert!(cfg.min_specular_compactness > 0.0 && cfg.min_specular_compactness < 1.0);
        assert!(cfg.max_radial_deviation > 0.0 && cfg.max_radial_deviation < 1.0);
    }
}
