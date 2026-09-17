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
//!
//! # MEASURED: this gate vetoes genuine live faces, and no threshold fixes it
//!
//! Do not reach for `max_radial_deviation` when this gate refuses a real
//! face. It is not the knob, and turning it cannot help.
//!
//! [`monotonic_falloff_score`] has two early returns that yield **exactly**
//! `0.0` — `counts.contains(&0)` and `dynamic_range <= 5% * means[0]`. `0.0`
//! is the strongest spoof assertion the `[0, 1]` score range can carry, so
//! `raw = 1.0 - score` is `1.0`, and `raw <= max_deviation` is false for
//! *every* `max_deviation < 1.0`. On those paths the threshold is never
//! consulted. "I could not compute this statistic" and "this is definitely a
//! spoof" are spelled the same way, and PAD is a veto, so the first refuses.
//!
//! The second early return fires on live faces. Measured on the reference
//! camera (Sonix 3277:0018, 850 nm, subject square to the lens at working
//! distance), 8 consecutive face-carrying strobe pairs, YuNet boxes ~50x48 px:
//! **6 of 8 returned exactly `0.0`**, reproducing the field report of
//! `pad veto: radial (1.000)` on 23 of 24 authentication attempts.
//!
//! The input is *not* degenerate. `D = On - Off` measured `off_mean`
//! 0.01-0.02/255 against `on_mean` 59-94/255, so `D` is essentially the
//! illuminated frame: 100% positive inside the face box, mean 126-185, no
//! clipping on the clean side. `illuminated_first` in the daemon orders the
//! pair by luminance and gets it right every time — worth stating, because
//! the parity tag was measured **inverted** on this device in all 8 pairs
//! (the lit frame is tagged `Off`), exactly the hazard finding F21 names.
//!
//! What actually fails is the physical model. The gate assumes the nose is
//! the nearest point and intensity therefore falls monotonically outward
//! from the box centre. Inside a tight YuNet box a real face does the
//! opposite: the central annulus lands on the nostrils, philtrum and mouth
//! shadow — IR-dark cavities — while the cheeks and forehead at bins 3-5 are
//! the brightest annuli, and the corners are still skin at nearly the same
//! range rather than dark background. Measured annular means (bins 0..7) for
//! one refused pair:
//!
//! ```text
//! [122.71, 122.83, 122.51, 122.30, 126.63, 128.51, 133.29, 131.27]
//! dynamic_range = means[0] - means[7] = -8.56   (NEGATIVE)
//! ```
//!
//! `dynamic_range` was negative in 5 of the 6 refusals (-2.4, -5.8, -8.6,
//! -20.0, -21.8, -22.4). The profile rises outward. A 1/d² falloff across a
//! ~15 cm face at ~50 cm is a few-percent effect to begin with, and the
//! gate's own guard demands more than 5% of the absolute pedestal before it
//! will even form an opinion — while `D` carries that full pedestal precisely
//! because the clean strobe half is black.
//!
//! Fixing this means choosing a different statistic (and a policy for the
//! genuinely undecidable case) and proving the new one separates live from
//! print/replay. That is task 17 and it needs the attack corpus, which does
//! not exist. **Widening `max_deviation` would be worse than useless**: it
//! cannot reach the failing path, and a flat print held close to the lens
//! produces the same low-contrast crop, so any change that lets these
//! through must be justified against real attacks, not against convenience.

use crate::GateResult;

/// Score mapping: `raw = 1.0 - score` (deviation from monotonic falloff).
/// Pass if `raw <= max_deviation`, i.e. `score >= 1.0 - max_deviation`.
/// The calibration bridge inverts this as `max_deviation = 1.0 - score_threshold`.
pub const SCORE_INVERSION: f32 = 1.0;

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
        // `raw` is `SCORE_INVERSION - score` on every other path, and the
        // calibration bridge inverts the mapping on that assumption. Reporting
        // `raw = 0.0` beside a failure claimed a *perfect* falloff fit while
        // refusing, so the daemon journalled this veto as
        // `pad veto: radial (0.000)` — indistinguishable in the audit log from
        // a gate that passed outright. Score 0.0 means raw 1.0.
        return GateResult::fail(0.0, SCORE_INVERSION);
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
    fn tiny_crop_reports_raw_consistent_with_its_zero_score() {
        // Regression: this path used to return `fail(0.0, 0.0)`, so the
        // daemon logged the veto as `pad veto: radial (0.000)` — the raw a
        // *perfect* falloff fit produces. `raw = SCORE_INVERSION - score`
        // holds on every other path and the calibration bridge inverts it.
        let d = vec![10.0; 16];
        let r = radial_falloff(&d, 4, 4, None, 0.55);
        assert!(!r.pass);
        assert_eq!(r.score, 0.0);
        assert_eq!(
            r.raw, SCORE_INVERSION,
            "a zero-score failure must report raw = 1.0, not 0.0"
        );
    }

    /// Build a crop whose annular means about its centre follow `profile`
    /// (8 bins, innermost first), using the same radial binning the gate does.
    ///
    /// This is how the measured hardware profiles below are turned into an
    /// image without shipping a face.
    fn from_annular_profile(cw: u32, ch: u32, profile: [f32; 8]) -> Vec<f32> {
        let cx = (cw as f32 - 1.0) / 2.0;
        let cy = (ch as f32 - 1.0) / 2.0;
        let max_radius = ((cw as f32).hypot(ch as f32) / 2.0).max(1.0);
        let mut img = Vec::with_capacity((cw * ch) as usize);
        for y in 0..ch {
            for x in 0..cw {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let radius = (dx * dx + dy * dy).sqrt();
                let bin = (((radius / max_radius) * 8.0) as usize).min(7);
                img.push(profile[bin]);
            }
        }
        img
    }

    /// MEASURED, reference camera, live human face square to the lens at
    /// working distance: YuNet box 55x48 px, `D = On - Off` with `off_mean`
    /// 0.01/255, `D` 100% positive inside the box, mean 126.4.
    ///
    /// The annular profile of a real face in a tight face box *rises*
    /// outward — the central annulus is nostrils and mouth shadow, the
    /// brightest annuli are the cheeks — so `dynamic_range` is negative and
    /// [`monotonic_falloff_score`] takes its `dynamic_range <= 5% * means[0]`
    /// early return and reports exactly 0.0, i.e. `raw` exactly 1.000.
    ///
    /// This is the field report `pad veto: radial (1.000)` reproduced with no
    /// hardware. 6 of 8 consecutive live pairs did this.
    const MEASURED_LIVE_FACE_PROFILE: [f32; 8] = [
        122.71, 122.83, 122.51, 122.30, 126.63, 128.51, 133.29, 131.27,
    ];

    #[test]
    fn measured_live_face_profile_scores_exactly_zero() {
        let (w, h) = (55u32, 48u32);
        let d = from_annular_profile(w, h, MEASURED_LIVE_FACE_PROFILE);

        let r = radial_falloff(&d, w, h, None, 0.55);

        assert!(!r.pass, "measured live face is vetoed: {r:?}");
        assert_eq!(
            r.score, 0.0,
            "not merely a low score — an early return's exact 0.0"
        );
        assert_eq!(r.raw, 1.0, "reproduces the field report's radial (1.000)");
    }

    #[test]
    fn no_threshold_can_admit_the_measured_live_face() {
        // The point of the whole investigation. `max_deviation` is never
        // consulted on the early-return path: `raw` is 1.0, and `raw <= t` is
        // false for every `t < 1.0`. Calibrating `max_radial_deviation`
        // therefore cannot fix this veto, however far it is opened.
        let (w, h) = (55u32, 48u32);
        let d = from_annular_profile(w, h, MEASURED_LIVE_FACE_PROFILE);
        for max_deviation in [0.55f32, 0.75, 0.90, 0.95, 0.99] {
            let r = radial_falloff(&d, w, h, None, max_deviation);
            assert!(
                !r.pass,
                "max_deviation={max_deviation} still vetoes; the threshold is not the knob: {r:?}"
            );
            assert_eq!(r.raw, 1.0);
        }
    }

    #[test]
    fn falloff_shallower_than_five_percent_scores_exactly_zero() {
        // The other half of the same guard: even a *correctly* decreasing
        // profile is refused outright when the centre-to-corner contrast is
        // under 5% of the absolute level. Because the clean strobe half
        // measures ~0.01/255, `D` carries the illuminated frame's full DC, so
        // the 5% is 5% of that pedestal — and 1/d² across a ~15 cm face at
        // ~50 cm is a few-percent effect to begin with.
        let (w, h) = (55u32, 48u32);
        let shallow = [126.0f32, 125.6, 125.2, 124.8, 124.4, 124.0, 123.6, 123.2];
        let d = from_annular_profile(w, h, shallow);

        let r = radial_falloff(&d, w, h, None, 0.55);

        assert!(r.raw > 0.0, "the profile really does decrease outward");
        assert_eq!(r.score, 0.0, "yet it scores the same as a flat print");
        assert!(!r.pass);
    }

    #[test]
    fn same_shape_with_deep_contrast_passes() {
        // Control for the two tests above: the *shape* is not what is refused.
        // Scale the identical monotone profile up to a 40% contrast and it
        // passes, which pins the refusal on the 5% dynamic-range guard rather
        // than on the monotonicity count.
        let (w, h) = (55u32, 48u32);
        let deep = [126.0f32, 118.0, 110.0, 103.0, 96.0, 89.0, 82.0, 76.0];
        let d = from_annular_profile(w, h, deep);

        let r = radial_falloff(&d, w, h, None, 0.55);

        assert!(r.pass, "a deep monotone falloff passes: {r:?}");
        assert!(r.score > 0.9, "score={}", r.score);
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
