// SPDX-License-Identifier: GPL-3.0-only
//! Gate 4: Radial falloff.
//!
//! Inverse-square illumination: a point emitter at distance `r` yields
//! intensity ∝ 1/r². A real face has a nose nearer than its ears, producing a
//! measurable radial intensity gradient in `D` centred on the brightest point
//! (the nearest surface). A flat photograph is uniformly illuminated, so `D`
//! falls off with distance from the optical centre only due to lens vignetting
//! — a much weaker, lens-symmetric pattern that does not match a face-shaped
//! 1/d² model.
//!
//! Statistic: fit `D(r) = a / (r + b)²` to the positive `D` values as a
//! function of radius from the centroid of positive energy. Measure the
//! normalised residual (1 − R²). Real faces fit well (low residual); flat
//! media fit poorly or fit a constant. Pass if deviation `1 − R²` ≤ threshold.

use crate::GateResult;

/// Crop `D` to the face bounding box, returning `(values, cw, ch)`.
fn crop_bbox(
    d: &[f32],
    w: u32,
    h: u32,
    bbox: Option<(u32, u32, u32, u32)>,
) -> (Vec<f32>, u32, u32) {
    match bbox {
        Some((bx, by, bw, bh)) => {
            let x0 = bx.min(w);
            let y0 = by.min(h);
            let x1 = (x0 + bw).min(w);
            let y1 = (y0 + bh).min(h);
            if x1 <= x0 || y1 <= y0 {
                return (d.to_vec(), w, h);
            }
            let cw = x1 - x0;
            let ch = y1 - y0;
            let mut out = Vec::with_capacity((cw * ch) as usize);
            for y in y0..y1 {
                for x in x0..x1 {
                    out.push(d[(y * w + x) as usize]);
                }
            }
            (out, cw, ch)
        }
        None => (d.to_vec(), w, h),
    }
}

/// Centroid of positive energy in the cropped image. Falls back to the
/// geometric centre if no positive values exist.
fn positive_centroid(img: &[f32], cw: u32, ch: u32) -> (f32, f32) {
    let mut sum_w = 0.0f32;
    let mut sx = 0.0f32;
    let mut sy = 0.0f32;
    for y in 0..ch as usize {
        for x in 0..cw as usize {
            let v = img[y * cw as usize + x];
            if v > 0.0 {
                sum_w += v;
                sx += v * x as f32;
                sy += v * y as f32;
            }
        }
    }
    if sum_w > 0.0 {
        (sx / sum_w, sy / sum_w)
    } else {
        ((cw as f32 - 1.0) / 2.0, (ch as f32 - 1.0) / 2.0)
    }
}

/// Fit `D(r) = a / (r + b)²` via a coarse grid search and return the
/// coefficient of determination `R²` in `[0, 1]` (clamped). A flat image has
/// `R² ≈ 0`; a clean 1/r² falloff has `R² ≈ 1`.
///
/// We use a linearised form: `1/√D = sqrt(1/a) * (r + b)`, so plotting
/// `1/√D` vs `r` is linear if the model holds. We fit a line by least squares
/// over the positive samples (radius, `1/√D`), then compute `R²` of that fit.
fn fit_inverse_square_r2(img: &[f32], cw: u32, ch: u32) -> f32 {
    let (cx, cy) = positive_centroid(img, cw, ch);
    // Build (r, y = 1/sqrt(D)) pairs for positive D.
    let mut pts: Vec<(f32, f32)> = Vec::new();
    for y in 0..ch as usize {
        for x in 0..cw as usize {
            let v = img[y * cw as usize + x];
            if v > 1.0 {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let r = (dx * dx + dy * dy).sqrt();
                let y_val = 1.0 / v.sqrt();
                pts.push((r, y_val));
            }
        }
    }
    if pts.len() < 8 {
        return 0.0;
    }

    // Least-squares line fit: y = m*r + c.
    let n = pts.len() as f32;
    let sum_r: f32 = pts.iter().map(|(r, _)| *r).sum();
    let sum_y: f32 = pts.iter().map(|(_, y)| *y).sum();
    let sum_rr: f32 = pts.iter().map(|(r, _)| r * r).sum();
    let sum_ry: f32 = pts.iter().map(|(r, y)| r * y).sum();
    let denom = n * sum_rr - sum_r * sum_r;
    if denom.abs() < 1e-9 {
        return 0.0;
    }
    let m = (n * sum_ry - sum_r * sum_y) / denom;
    let c = (sum_y - m * sum_r) / n;
    let mean_y = sum_y / n;
    let mut ss_tot = 0.0f32;
    let mut ss_res = 0.0f32;
    for (r, y) in &pts {
        let pred = m * r + c;
        ss_res += (y - pred) * (y - pred);
        ss_tot += (y - mean_y) * (y - mean_y);
    }
    if ss_tot <= 1e-9 {
        return 0.0;
    }
    let r2 = 1.0 - ss_res / ss_tot;
    r2.clamp(0.0, 1.0)
}

/// Compute the radial-falloff gate.
///
/// `d` is the strobe differential. `bbox` is the face region in pixels
/// `(x, y, w, h)`; if `None`, the full frame is used. Pass if the
/// deviation `1 − R²` ≤ `max_deviation`.
pub fn radial_falloff(
    d: &[f32],
    w: u32,
    h: u32,
    bbox: Option<(u32, u32, u32, u32)>,
    max_deviation: f32,
) -> GateResult {
    let (crop, cw, ch) = crop_bbox(d, w, h, bbox);
    if crop.is_empty() || cw < 8 || ch < 8 {
        return GateResult::fail(0.0, 0.0);
    }
    let r2 = fit_inverse_square_r2(&crop, cw, ch);
    let raw = 1.0 - r2; // deviation
                        // Score: 1 at R²=1 (perfect fit), 0 at R²=0.
    let score = r2.clamp(0.0, 1.0);
    let pass = raw <= max_deviation;
    if pass {
        GateResult::pass(score, raw)
    } else {
        GateResult::fail(score, raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a synthetic 1/r² falloff image centred at (cx, cy).
    fn make_falloff(w: u32, h: u32, cx: f32, cy: f32, a: f32, b: f32) -> Vec<f32> {
        let mut img = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let r = (dx * dx + dy * dy).sqrt();
                let v = a / (r + b).powi(2);
                img.push(v.max(0.0));
            }
        }
        img
    }

    #[test]
    fn clean_falloff_passes() {
        let w = 64u32;
        let h = 64u32;
        let d = make_falloff(w, h, 32.0, 32.0, 5000.0, 5.0);
        let r = radial_falloff(&d, w, h, None, 0.55);
        assert!(r.pass, "clean 1/r² falloff should pass: R²={}", 1.0 - r.raw);
        assert!(r.score > 0.8, "R² should be high: {}", r.score);
    }

    #[test]
    fn flat_image_fails() {
        let w = 32u32;
        let h = 32u32;
        let d = vec![50.0; (w * h) as usize];
        let r = radial_falloff(&d, w, h, None, 0.55);
        assert!(!r.pass, "flat image should fail");
    }

    #[test]
    fn noisy_image_fails() {
        let w = 32u32;
        let h = 32u32;
        let d: Vec<f32> = (0..(w * h))
            .map(|i| if i % 2 == 0 { 80.0 } else { 10.0 })
            .collect();
        let r = radial_falloff(&d, w, h, None, 0.55);
        assert!(!r.pass, "noise should fail");
    }

    #[test]
    fn tiny_crop_fails() {
        let d = vec![10.0; 16];
        let r = radial_falloff(&d, 4, 4, None, 0.55);
        assert!(!r.pass);
    }

    #[test]
    fn empty_d_fails() {
        let r = radial_falloff(&[], 0, 0, None, 0.55);
        assert!(!r.pass);
    }

    #[test]
    fn bbox_crop_used() {
        // Full frame is noise, bbox is clean falloff.
        let w = 64u32;
        let h = 64u32;
        let mut d = vec![10.0; (w * h) as usize];
        let inner = make_falloff(32, 32, 16.0, 16.0, 5000.0, 5.0);
        for y in 0..32u32 {
            for x in 0..32u32 {
                d[((y + 16) * w + (x + 16)) as usize] = inner[(y * 32 + x) as usize];
            }
        }
        let r = radial_falloff(&d, w, h, Some((16, 16, 32, 32)), 0.55);
        assert!(r.pass, "bbox-isolated falloff should pass: R²={}", r.score);
    }

    #[test]
    fn positive_centroid_finds_peak() {
        let w = 16u32;
        let h = 16u32;
        let mut img = vec![1.0; (w * h) as usize];
        img[5 * w as usize + 5] = 1000.0; // dominant peak at (5, 5)
        let (cx, cy) = positive_centroid(&img, w, h);
        assert!((cx - 5.0).abs() < 1.5, "cx={cx}");
        assert!((cy - 5.0).abs() < 1.5, "cy={cy}");
    }

    #[test]
    fn positive_centroid_fallback_no_positives() {
        let img = vec![-1.0; 100];
        let (cx, cy) = positive_centroid(&img, 10, 10);
        assert_eq!(cx, 4.5);
        assert_eq!(cy, 4.5);
    }
}
