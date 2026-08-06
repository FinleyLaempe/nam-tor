// SPDX-License-Identifier: GPL-3.0-only
//! Gate 4: Radial falloff.
//!
//! A real face has a nose nearer than its ears, producing a measurable radial
//! intensity gradient in `D`. Flat media is illuminated more uniformly.
//!
//! Statistic: average positive differential energy in concentric annuli around
//! its centroid and measure how consistently the profile decreases outward.
//!
//! UNCALIBRATED: the threshold (`max_deviation`) and the annular-bin scoring
//! have NOT been calibrated against real/live/print/replay captures yet.
//! Calibration against the reference hardware + attack corpus is deferred to
//! task 17. Until then the gate runs with a conservative default and its
//! pass/fail boundaries must not be treated as a tuned APCER gate.

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

fn monotonic_falloff_score(img: &[f32], cw: u32, ch: u32) -> f32 {
    const BINS: usize = 8;
    let (cx, cy) = positive_centroid(img, cw, ch);
    let max_radius = ((cw as f32).hypot(ch as f32) / 2.0).max(1.0);
    let mut sums = [0.0f32; BINS];
    let mut counts = [0usize; BINS];
    for y in 0..ch as usize {
        for x in 0..cw as usize {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let radius = (dx * dx + dy * dy).sqrt();
            let bin = ((radius / max_radius) * BINS as f32) as usize;
            let bin = bin.min(BINS - 1);
            sums[bin] += img[y * cw as usize + x].max(0.0);
            counts[bin] += 1;
        }
    }
    if counts.contains(&0) {
        return 0.0;
    }
    let means = std::array::from_fn::<_, BINS, _>(|index| sums[index] / counts[index] as f32);
    let dynamic_range = means[0] - means[BINS - 1];
    if dynamic_range <= means[0].abs() * 0.05 {
        return 0.0;
    }
    let decreasing = means
        .windows(2)
        .filter(|pair| pair[1] <= pair[0] + dynamic_range * 0.10)
        .count();
    decreasing as f32 / (BINS - 1) as f32
}

/// Compute the radial-falloff gate.
///
/// `d` is the strobe differential. `bbox` is the face region in pixels
/// `(x, y, w, h)`; if `None`, the full frame is used. Pass if the
/// deviation from monotonic outward falloff ≤ `max_deviation`.
///
/// UNCALIBRATED — see module docs. Calibration deferred to task 17.
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
    let score = monotonic_falloff_score(&crop, cw, ch);
    let raw = 1.0 - score;
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

    fn make_face_shading(w: u32, h: u32, cx: f32, cy: f32) -> Vec<f32> {
        let mut img = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                let dx = (x as f32 - cx) / (w as f32 * 0.45);
                let dy = (y as f32 - cy) / (h as f32 * 0.55);
                let radius = (dx * dx + dy * dy).sqrt();
                let lambertian = (1.0 - radius * radius).max(0.0).sqrt();
                let facial_structure = 18.0 * (x as f32 * 0.31).sin() * (y as f32 * 0.17).cos();
                img.push((12.0 + 55.0 * lambertian + facial_structure).max(0.0));
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
    fn convex_face_shading_passes_without_exact_inverse_square_shape() {
        let w = 96u32;
        let h = 112u32;
        let d = make_face_shading(w, h, 48.0, 50.0);

        let result = radial_falloff(&d, w, h, None, 0.55);

        assert!(result.pass, "convex face shading should pass: {result:?}");
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
