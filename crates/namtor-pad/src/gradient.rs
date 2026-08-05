// SPDX-License-Identifier: GPL-3.0-only
//! Gate 2: Gradient consistency.
//!
//! A real face exhibits subsurface scattering that produces a smooth, spatially
//! coherent gradient field in `D`. A printed photograph has no subsurface
//! scattering: its gradient field is either flat (uniform print) or noisy
//! (paper texture), lacking the coherent low-frequency structure of skin.
//!
//! Statistic: ratio of low-frequency gradient energy to total gradient energy,
//! computed via a 3×3 Sobel operator and a coarse (4×4 block) low-pass. Real
//! faces concentrate gradient energy in low frequencies; prints spread it
//! across high frequencies. Pass if the low-frequency ratio ≥ threshold.

use crate::GateResult;

/// Crop `D` to the face bounding box. If no box is given, use the full frame.
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
            let cw = x1 - x0;
            let ch = y1 - y0;
            if cw == 0 || ch == 0 {
                return (d.to_vec(), w, h);
            }
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

/// 3×3 Sobel magnitude at pixel `(x, y)` in a `cw × ch` image.
fn sobel_mag(img: &[f32], cw: usize, ch: usize, x: usize, y: usize) -> f32 {
    let at = |dx: isize, dy: isize| -> f32 {
        let xx = (x as isize + dx).clamp(0, cw as isize - 1) as usize;
        let yy = (y as isize + dy).clamp(0, ch as isize - 1) as usize;
        img[yy * cw + xx]
    };
    let gx = -at(-1, -1) + at(1, -1) - 2.0 * at(-1, 0) + 2.0 * at(1, 0) - at(-1, 1) + at(1, 1);
    let gy = -at(-1, -1) - 2.0 * at(0, -1) - at(1, -1) + at(-1, 1) + 2.0 * at(0, 1) + at(1, 1);
    (gx * gx + gy * gy).sqrt()
}

/// Downsample by 4×4 block averaging the *image*, then compute the gradient
/// energy of the low-pass version and compare to the total gradient energy.
/// A smooth coherent gradient (real face) is preserved by low-pass → high
/// ratio. High-frequency noise (print/texture) is attenuated → low ratio.
/// Returns the ratio in `[0, 1]`.
fn low_frequency_ratio(img: &[f32], cw: u32, ch: u32) -> f32 {
    if cw < 8 || ch < 8 {
        return 0.0;
    }
    let cwu = cw as usize;
    let chu = ch as usize;

    // Total gradient energy of the original image.
    let mut total_energy = 0.0f32;
    for y in 0..chu {
        for x in 0..cwu {
            let m = sobel_mag(img, cwu, chu, x, y);
            total_energy += m * m;
        }
    }
    if total_energy <= 0.0 {
        return 0.0;
    }

    // Low-pass: 4×4 block average of the *image* (not the gradient).
    let bwc = cw / 4;
    let bhc = ch / 4;
    let mut low_img = vec![0.0f32; (bwc * bhc) as usize];
    for by in 0..bhc as usize {
        for bx in 0..bwc as usize {
            let mut sum = 0.0;
            for dy in 0..4 {
                for dx in 0..4 {
                    let x = bx * 4 + dx;
                    let y = by * 4 + dy;
                    sum += img[y * cwu + x];
                }
            }
            low_img[by * bwc as usize + bx] = sum / 16.0;
        }
    }

    // Gradient energy of the low-pass image.
    let mut low_energy = 0.0f32;
    for y in 0..bhc as usize {
        for x in 0..bwc as usize {
            let m = sobel_mag(&low_img, bwc as usize, bhc as usize, x, y);
            low_energy += m * m;
        }
    }

    // The low-pass image has 1/16 the area; scale energy up to compare.
    // Gradient magnitude is preserved under block-averaging for smooth
    // gradients, so scaled low_energy ≈ total_energy for a clean ramp.
    low_energy * 16.0 / total_energy
}

/// Compute the gradient-consistency gate.
///
/// `d` is the strobe differential. `bbox` is the face region in pixels
/// `(x, y, w, h)`; if `None`, the full frame is used. Pass if the
/// low-frequency gradient ratio ≥ `min_score`.
pub fn gradient_consistency(
    d: &[f32],
    w: u32,
    h: u32,
    bbox: Option<(u32, u32, u32, u32)>,
    min_score: f32,
) -> GateResult {
    let (crop, cw, ch) = crop_bbox(d, w, h, bbox);
    if crop.is_empty() || cw < 8 || ch < 8 {
        return GateResult::fail(0.0, 0.0);
    }
    let raw = low_frequency_ratio(&crop, cw, ch);
    // Normalise: a real face typically has ratio >= 0.3, a flat print < 0.1.
    // Score maps [0, 0.5] → [0, 1].
    let score = (raw / 0.5).clamp(0.0, 1.0);
    let pass = raw >= min_score;
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
    fn smooth_gradient_passes() {
        // A smooth ramp has coherent low-frequency gradient.
        let w = 32u32;
        let h = 32u32;
        let d: Vec<f32> = (0..(w * h)).map(|i| (i as f32 / w as f32) * 10.0).collect();
        let r = gradient_consistency(&d, w, h, None, 0.45);
        assert!(r.pass, "smooth ramp should pass: score={}", r.score);
    }

    #[test]
    fn flat_image_fails() {
        let w = 32u32;
        let h = 32u32;
        let d = vec![50.0; (w * h) as usize];
        let r = gradient_consistency(&d, w, h, None, 0.45);
        assert!(!r.pass, "flat image should fail");
    }

    #[test]
    fn noisy_image_fails() {
        // High-frequency noise: no coherent low-frequency gradient.
        let w = 32u32;
        let h = 32u32;
        let d: Vec<f32> = (0..(w * h))
            .map(|i| if i % 2 == 0 { 50.0 } else { -50.0 })
            .collect();
        let r = gradient_consistency(&d, w, h, None, 0.45);
        assert!(!r.pass, "checkerboard noise should fail");
    }

    #[test]
    fn tiny_crop_fails() {
        let d = vec![10.0; 16];
        let r = gradient_consistency(&d, 4, 4, None, 0.45);
        assert!(!r.pass);
    }

    #[test]
    fn bbox_crop_is_used() {
        // Full frame is noise, but the bbox region is smooth.
        let w = 32u32;
        let h = 32u32;
        let mut d = vec![0.0; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                if (8..24).contains(&x) && (8..24).contains(&y) {
                    d[i] = (x as f32 - 8.0) * 2.0; // smooth ramp in bbox
                } else {
                    d[i] = if i.is_multiple_of(2) { 50.0 } else { -50.0 }; // noise outside
                }
            }
        }
        let r = gradient_consistency(&d, w, h, Some((8, 8, 16, 16)), 0.45);
        assert!(r.pass, "smooth bbox should pass: score={}", r.score);
    }

    #[test]
    fn empty_d_fails() {
        let r = gradient_consistency(&[], 0, 0, None, 0.45);
        assert!(!r.pass);
    }
}
