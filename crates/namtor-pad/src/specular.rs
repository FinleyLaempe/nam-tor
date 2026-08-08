// SPDX-License-Identifier: GPL-3.0-only
//! Gate 3: Specular highlight.
//!
//! Skin under a point IR emitter produces a compact specular hotspot: a small
//! bright region in `D` whose centroid is near the face centre and whose
//! spatial extent is bounded. A flat printed photograph produces either no
//! hotspot (diffuse reflection) or a diffuse, spread-out highlight. A screen
//! replay produces no positive `D` at all (caught by gate 1, but this gate is
//! independent).
//!
//! Statistic: compactness = peak_intensity / (90th-percentile intensity),
//! computed on positive `D` values within the face region. Real skin yields a
//! high ratio (tight peak); flat media yield a low ratio (uniform or absent).
//! Pass if compactness ≥ `min_compactness`.

use crate::GateResult;

/// Score normalisation: `score = ((raw - SCORE_OFFSET) / SCORE_SCALE).clamp(0, 1)`.
/// A compactness of `SCORE_OFFSET` (1.0, uniform) maps to score 0; a
/// compactness of `SCORE_OFFSET + SCORE_SCALE` (6.0, tight peak) maps to 1.0.
/// Exposed so the calibration bridge can invert the mapping.
pub const SCORE_OFFSET: f32 = 1.0;
pub const SCORE_SCALE: f32 = 5.0;

/// Crop `D` to the face bounding box. Returns `(cropped_values, cw, ch)`.
fn crop_bbox(d: &[f32], w: u32, h: u32, bbox: Option<(u32, u32, u32, u32)>) -> Vec<f32> {
    match bbox {
        Some((bx, by, bw, bh)) => {
            let x0 = bx.min(w);
            let y0 = by.min(h);
            let x1 = (x0 + bw).min(w);
            let y1 = (y0 + bh).min(h);
            if x1 <= x0 || y1 <= y0 {
                return d.to_vec();
            }
            let mut out = Vec::with_capacity(((x1 - x0) * (y1 - y0)) as usize);
            for y in y0..y1 {
                for x in x0..x1 {
                    out.push(d[(y * w + x) as usize]);
                }
            }
            out
        }
        None => d.to_vec(),
    }
}

/// 90th percentile of positive values in `vals`. Returns 0.0 if no positive
/// values exist.
fn p90_positive(vals: &[f32]) -> f32 {
    let mut pos: Vec<f32> = vals.iter().copied().filter(|&v| v > 0.0).collect();
    if pos.is_empty() {
        return 0.0;
    }
    pos.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((pos.len() as f32) * 0.9) as usize;
    pos[idx.min(pos.len() - 1)]
}

/// Peak positive value in `vals`, or 0.0 if none.
fn peak_positive(vals: &[f32]) -> f32 {
    vals.iter()
        .copied()
        .filter(|&v| v > 0.0)
        .fold(0.0f32, f32::max)
}

/// Compute the specular-highlight gate.
///
/// `d` is the strobe differential. `bbox` is the face region in pixels
/// `(x, y, w, h)`; if `None`, the full frame is used. Pass if the
/// compactness ratio ≥ `min_compactness`.
pub fn specular_highlight(
    d: &[f32],
    w: u32,
    h: u32,
    bbox: Option<(u32, u32, u32, u32)>,
    min_compactness: f32,
) -> GateResult {
    let crop = crop_bbox(d, w, h, bbox);
    if crop.is_empty() {
        return GateResult::fail(0.0, 0.0);
    }
    let peak = peak_positive(&crop);
    let p90 = p90_positive(&crop);
    if peak <= 0.0 || p90 <= 0.0 {
        return GateResult::fail(0.0, 0.0);
    }
    let raw = peak / p90;
    // Normalise: compactness of SCORE_OFFSET = perfectly uniform (no hotspot),
    // SCORE_OFFSET + SCORE_SCALE+ = tight peak. Score maps [1, 6] → [0, 1].
    let score = ((raw - SCORE_OFFSET) / SCORE_SCALE).clamp(0.0, 1.0);
    let pass = raw >= min_compactness;
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
    fn tight_hotspot_passes() {
        // Mostly dim with a single bright peak.
        let mut d = vec![5.0; 400];
        d[200] = 120.0;
        d[201] = 100.0;
        d[202] = 90.0;
        let r = specular_highlight(&d, 20, 20, None, 2.0);
        assert!(r.pass, "tight hotspot should pass: raw={}", r.raw);
    }

    #[test]
    fn uniform_image_fails() {
        let d = vec![50.0; 400];
        let r = specular_highlight(&d, 20, 20, None, 2.0);
        assert!(!r.pass, "uniform image should fail");
    }

    #[test]
    fn diffuse_image_fails() {
        // Many medium-bright pixels, no dominant peak.
        let d: Vec<f32> = (0..400).map(|i| 40.0 + (i as f32 % 10.0)).collect();
        let r = specular_highlight(&d, 20, 20, None, 2.0);
        assert!(!r.pass, "diffuse image should fail");
    }

    #[test]
    fn no_positive_values_fails() {
        let d = vec![-10.0; 400];
        let r = specular_highlight(&d, 20, 20, None, 2.0);
        assert!(!r.pass);
    }

    #[test]
    fn empty_crop_fails() {
        let r = specular_highlight(&[], 0, 0, None, 2.0);
        assert!(!r.pass);
    }

    #[test]
    fn bbox_isolates_hotspot() {
        let w = 32u32;
        let h = 32u32;
        let mut d = vec![5.0; (w * h) as usize];
        // Hotspot inside bbox at (12, 12)
        d[(12 * w + 12) as usize] = 120.0;
        // Noise outside bbox
        for (idx, v) in d.iter_mut().enumerate() {
            if idx % 3 == 0 {
                *v += 80.0;
            }
        }
        let r = specular_highlight(&d, w, h, Some((8, 8, 16, 16)), 2.0);
        assert!(r.pass, "bbox-isolated hotspot should pass: raw={}", r.raw);
    }

    #[test]
    fn p90_positive_handles_no_positives() {
        assert_eq!(p90_positive(&[-1.0, -2.0, -3.0]), 0.0);
    }

    #[test]
    fn p90_positive_returns_high_value() {
        let vals: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let p = p90_positive(&vals);
        assert!((89.0..=91.0).contains(&p));
    }
}
