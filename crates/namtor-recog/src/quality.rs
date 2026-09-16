// SPDX-License-Identifier: GPL-3.0-only
//! Per-frame capture-quality measurement.
//!
//! Every function here is **pure over a greyscale buffer plus a
//! [`Detection`]**: no camera, no filesystem, no model. That is what makes the
//! whole module unit-testable against synthetic images.
//!
//! The numbers produced are *capture quality* signals — how usable a frame is
//! for enrolment or matching. They are explicitly **not** error rates (FAR /
//! FRR) and must never be presented as such.
//!
//! All inputs are treated as hostile: zero-sized or out-of-frame boxes,
//! truncated buffers, coincident landmarks and non-finite coordinates all
//! yield sane clamped values rather than a panic.

use crate::detect::{Detection, Landmarks};

/// [`laplacian_stddev`] value treated as "fully sharp" once reached.
///
/// Matches the threshold the CLI already uses (`>= ~50` is sharp, `< ~5` is
/// junk), so the normalised [`FrameMetrics::blur`] is directly comparable with
/// the legacy `EnrollmentQuality` blur term.
const BLUR_SHARP_AT: f32 = 50.0;

/// Lower edge of the fully-acceptable illumination band. The band is centred
/// on the target mean intensity of 130.
const ILLUM_OK_LOW: f32 = 90.0;
/// Upper edge of the fully-acceptable illumination band.
const ILLUM_OK_HIGH: f32 = 170.0;
/// Mean intensity at which the frame is considered completely underexposed.
const ILLUM_DARK_ZERO: f32 = 20.0;
/// Mean intensity at which the frame is considered completely blown out.
///
/// Deliberately past 255: a uniformly white face box still scores poorly
/// (~0.23) without a merely bright-but-usable frame being condemned. The
/// falloff is asymmetric because an underexposed frame is unrecoverable noise
/// while a bright one still carries gradient information.
const ILLUM_BRIGHT_ZERO: f32 = 280.0;

/// Sample value at or above which a pixel is treated as clipped to white.
///
/// 250 rather than 255 because 8-bit IR sensors clip a few counts below the
/// nominal maximum once gain is applied; a pixel at 250 carries no more
/// gradient than one at 255.
const SATURATED_AT: u8 = 250;

/// Degrees of yaw attributed to a nose offset of one full inter-ocular
/// distance. A quarter-IOD offset therefore reads as ~22.5 deg, which is where
/// embedding quality starts to degrade.
const YAW_DEG_PER_IOD: f32 = 90.0;

/// Raw per-frame measurements extracted from one greyscale frame and the
/// detection found in it.
///
/// Each field is an independent measurement. Nothing here is derived from
/// another field, which is the point: a consumer can weight them, and the
/// weights actually change the outcome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMetrics {
    /// Detector confidence for the face box, `0.0..=1.0`.
    pub detection_confidence: f32,
    /// Sharpness, `0.0..=1.0`, higher = sharper. Normalised
    /// [`laplacian_stddev`] measured **inside the face box only**.
    pub blur: f32,
    /// Illumination adequacy, `0.0..=1.0`. Peaked: a dark frame *and* a blown
    /// out frame both score low, only a well-exposed one scores high.
    pub illumination: f32,
    /// Signed head yaw in degrees. Negative = the nose sits left of the eye
    /// midpoint in image coordinates (subject turned toward the camera's
    /// left); positive = turned the other way. `0.0` when unmeasurable.
    pub yaw_deg: f32,
    /// Signed head roll in degrees, from the inter-ocular vector. Positive =
    /// the subject's left eye sits lower in the image. `0.0` when
    /// unmeasurable.
    pub roll_deg: f32,
    /// Face box area over frame area, `0.0..=1.0`. The distance proxy: small
    /// means "too far away", large means "too close".
    pub face_fraction: f32,
    /// Inter-ocular distance over face box width, `0.0..=1.0`. Degenerate
    /// (near-zero) spread means the landmarks collapsed and the detection is
    /// not trustworthy.
    pub landmark_spread: f32,
    /// Fraction of face-box samples clipped to white (`>= 250`), `0.0..=1.0`.
    ///
    /// Independent of [`Self::illumination`] on purpose (review finding F6):
    /// mean intensity alone cannot tell a bright-but-usable frame from one
    /// whose highlights have been clipped away, because a uniformly white box
    /// still scores `illumination == 0.23` under the asymmetric band. A high
    /// saturated fraction means the identity information in those samples is
    /// gone and no amount of gain will bring it back.
    ///
    /// `0.0` when the pixels are unmeasurable (truncated buffer, box clipped
    /// out of frame) — absence of evidence, not evidence of clipping.
    pub saturated_fraction: f32,
}

impl FrameMetrics {
    /// All-zero metrics: what an unmeasurable frame yields.
    fn zeroed() -> Self {
        Self {
            detection_confidence: 0.0,
            blur: 0.0,
            illumination: 0.0,
            yaw_deg: 0.0,
            roll_deg: 0.0,
            face_fraction: 0.0,
            landmark_spread: 0.0,
            saturated_fraction: 0.0,
        }
    }
}

/// Measure every per-frame quality signal for `det` within `frame`.
///
/// `frame` is a tightly packed 8-bit greyscale buffer of `w * h` samples. A
/// buffer shorter than that, a zero-sized frame, or a face box that clips
/// entirely outside the frame yields zeroed pixel-derived metrics; the purely
/// geometric metrics are still computed where they are well defined.
///
/// Never panics.
pub fn measure(frame: &[u8], w: u32, h: u32, det: &Detection) -> FrameMetrics {
    let mut m = FrameMetrics::zeroed();
    m.detection_confidence = finite(det.box_.score).clamp(0.0, 1.0);
    m.face_fraction = face_fraction(det, w, h);

    let iod = inter_ocular_distance(&det.landmarks);
    m.landmark_spread = landmark_spread(iod, finite(det.box_.w));
    m.yaw_deg = yaw_deg(&det.landmarks, iod);
    m.roll_deg = roll_deg(&det.landmarks, iod);

    if let Some(roi) = clip_box(det, w, h) {
        let pixels = crop(frame, w, h, roi);
        if !pixels.is_empty() {
            m.blur = (laplacian_stddev(&pixels, roi.w, roi.h) / BLUR_SHARP_AT).clamp(0.0, 1.0);
            m.illumination = illumination_score(mean_intensity(&pixels));
            m.saturated_fraction = saturated_fraction(&pixels);
        }
    }
    m
}

/// Face box area over frame area, clamped to `0.0..=1.0`.
fn face_fraction(det: &Detection, w: u32, h: u32) -> f32 {
    let frame_area = w as f32 * h as f32;
    if frame_area <= 0.0 {
        return 0.0;
    }
    let bw = finite(det.box_.w).max(0.0);
    let bh = finite(det.box_.h).max(0.0);
    (bw * bh / frame_area).clamp(0.0, 1.0)
}

/// Inter-ocular distance in pixels; `0.0` for coincident or non-finite eyes.
fn inter_ocular_distance(lm: &Landmarks) -> f32 {
    let (dx, dy) = inter_ocular_vector(lm);
    let d = dx.hypot(dy);
    if d.is_finite() {
        d
    } else {
        0.0
    }
}

/// Vector from the subject's right eye to their left eye, in image
/// coordinates. For an upright frontal face this points along `+x`.
fn inter_ocular_vector(lm: &Landmarks) -> (f32, f32) {
    (
        finite(lm.left_eye.0) - finite(lm.right_eye.0),
        finite(lm.left_eye.1) - finite(lm.right_eye.1),
    )
}

/// Inter-ocular distance relative to the box width.
fn landmark_spread(iod: f32, box_w: f32) -> f32 {
    if box_w <= 0.0 || !box_w.is_finite() || iod <= 0.0 {
        return 0.0;
    }
    (iod / box_w).clamp(0.0, 1.0)
}

/// Signed yaw from the nose's offset along the inter-ocular axis.
///
/// The offset is projected onto the (unit) inter-ocular vector rather than
/// onto the image x-axis, which makes yaw independent of roll. `0.0` when the
/// eyes are coincident, since there is then no axis and no scale.
fn yaw_deg(lm: &Landmarks, iod: f32) -> f32 {
    if iod <= f32::EPSILON {
        return 0.0;
    }
    let (ex, ey) = inter_ocular_vector(lm);
    let mid_x = (finite(lm.left_eye.0) + finite(lm.right_eye.0)) * 0.5;
    let mid_y = (finite(lm.left_eye.1) + finite(lm.right_eye.1)) * 0.5;
    let nx = finite(lm.nose.0) - mid_x;
    let ny = finite(lm.nose.1) - mid_y;
    // Projection of the nose offset onto the eye axis, in units of IOD.
    let ratio = (nx * ex + ny * ey) / (iod * iod);
    (ratio * YAW_DEG_PER_IOD).clamp(-90.0, 90.0)
}

/// Signed roll from the inter-ocular vector. `0.0` for coincident eyes.
fn roll_deg(lm: &Landmarks, iod: f32) -> f32 {
    if iod <= f32::EPSILON {
        return 0.0;
    }
    let (dx, dy) = inter_ocular_vector(lm);
    dy.atan2(dx).to_degrees()
}

/// A face box clipped to the frame, in whole pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Roi {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// Clip the detection box to the frame. `None` when the frame is empty or the
/// box has no overlap with it.
fn clip_box(det: &Detection, w: u32, h: u32) -> Option<Roi> {
    if w == 0 || h == 0 {
        return None;
    }
    let bx = finite(det.box_.x);
    let by = finite(det.box_.y);
    let bw = finite(det.box_.w);
    let bh = finite(det.box_.h);
    if bw <= 0.0 || bh <= 0.0 {
        return None;
    }
    let x0 = bx.floor().clamp(0.0, w as f32) as u32;
    let y0 = by.floor().clamp(0.0, h as f32) as u32;
    let x1 = (bx + bw).ceil().clamp(0.0, w as f32) as u32;
    let y1 = (by + bh).ceil().clamp(0.0, h as f32) as u32;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(Roi {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    })
}

/// Copy the ROI out of `frame` as a tightly packed sub-image. Empty when the
/// buffer is too short to contain the whole frame.
fn crop(frame: &[u8], w: u32, h: u32, roi: Roi) -> Vec<u8> {
    let stride = w as usize;
    let needed = stride.saturating_mul(h as usize);
    if needed == 0 || frame.len() < needed {
        return Vec::new();
    }
    let rw = roi.w as usize;
    let mut out = Vec::with_capacity(rw.saturating_mul(roi.h as usize));
    for row in 0..roi.h as usize {
        let start = (roi.y as usize + row) * stride + roi.x as usize;
        out.extend_from_slice(&frame[start..start + rw]);
    }
    out
}

/// Mean sample value of a buffer, `0.0` when empty.
fn mean_intensity(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let sum: u64 = data.iter().map(|&v| v as u64).sum();
    sum as f32 / data.len() as f32
}

/// Fraction of samples clipped to white (`>= SATURATED_AT`), `0.0` when empty.
fn saturated_fraction(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let clipped = data.iter().filter(|&&v| v >= SATURATED_AT).count();
    clipped as f32 / data.len() as f32
}

/// Map a mean intensity through the peaked illumination band.
///
/// Flat `1.0` across the acceptable band, falling off to `0.0` on both sides —
/// faster toward black than toward white. A dark frame and a blown-out frame
/// both score low; only a correctly exposed one scores high.
fn illumination_score(mean: f32) -> f32 {
    if !mean.is_finite() {
        return 0.0;
    }
    if (ILLUM_OK_LOW..=ILLUM_OK_HIGH).contains(&mean) {
        return 1.0;
    }
    if mean < ILLUM_OK_LOW {
        ((mean - ILLUM_DARK_ZERO) / (ILLUM_OK_LOW - ILLUM_DARK_ZERO)).clamp(0.0, 1.0)
    } else {
        ((ILLUM_BRIGHT_ZERO - mean) / (ILLUM_BRIGHT_ZERO - ILLUM_OK_HIGH)).clamp(0.0, 1.0)
    }
}

/// Standard deviation of the 4-neighbour Laplacian response over a greyscale
/// buffer: the sharpness term behind [`FrameMetrics::blur`].
///
/// Named for what it returns. The literature calls the family "variance of
/// Laplacian", but this takes the square root of that variance, so the value
/// is a **standard deviation** and is linear in contrast rather than
/// quadratic. Review finding F7: the old name promised a variance and
/// returned a standard deviation, and `namtor-cli` carried a byte-for-byte
/// duplicate of the body. This is now the only implementation; the CLI
/// delegates here.
///
/// ## Denominator
///
/// Both the mean and the variance divide by the **full** `w * h` buffer
/// length, not by the `(w - 2) * (h - 2)` interior the kernel actually
/// writes. The one-pixel border stays at the `0.0` it was initialised to and
/// is averaged in, which pulls the result down by roughly the border's share
/// of the buffer (~6% on a 64x64 ROI, ~0.6% on a 640x360 frame).
///
/// This is deliberate and is **kept** rather than corrected: every threshold
/// in the tree is calibrated against this arithmetic — [`BLUR_SHARP_AT`]
/// here and the CLI's hard `blur < 3.0` reject in `enroll.rs`. Changing the
/// denominator would silently raise every measured value and shift both
/// gates, on a metric whose absolute scale has no external meaning anyway
/// (only the ordering between frames does). Retune the thresholds against
/// real captures first, in one change, if it is ever worth it.
///
/// Returns `0.0` for buffers too small or too short to filter.
pub fn laplacian_stddev(data: &[u8], width: u32, height: u32) -> f32 {
    let w = width as usize;
    let h = height as usize;
    if w < 3 || h < 3 || data.len() < w * h {
        return 0.0;
    }
    let mut laplacian = vec![0f32; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let idx = y * w + x;
            let center = data[idx] as f32;
            let up = data[idx - w] as f32;
            let down = data[idx + w] as f32;
            let left = data[idx - 1] as f32;
            let right = data[idx + 1] as f32;
            laplacian[idx] = -4.0 * center + up + down + left + right;
        }
    }
    let mean: f32 = laplacian.iter().sum::<f32>() / laplacian.len() as f32;
    let var: f32 =
        laplacian.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / laplacian.len() as f32;
    var.sqrt()
}

/// Replace NaN / infinity with `0.0` so downstream arithmetic stays sane.
fn finite(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::FaceBox;

    /// A 1-pixel checkerboard: the sharpest image that fits in 8 bits.
    fn checkerboard(w: u32, h: u32) -> Vec<u8> {
        (0..h)
            .flat_map(|y| (0..w).map(move |x| if (x + y) % 2 == 0 { 0u8 } else { 255u8 }))
            .collect()
    }

    /// Separable-free box blur with the given radius, edges clamped.
    fn box_blur(src: &[u8], w: u32, h: u32, radius: i32) -> Vec<u8> {
        let (wi, hi) = (w as i32, h as i32);
        let mut out = vec![0u8; src.len()];
        for y in 0..hi {
            for x in 0..wi {
                let mut sum = 0u32;
                let mut n = 0u32;
                for dy in -radius..=radius {
                    for dx in -radius..=radius {
                        let sx = (x + dx).clamp(0, wi - 1);
                        let sy = (y + dy).clamp(0, hi - 1);
                        sum += src[(sy * wi + sx) as usize] as u32;
                        n += 1;
                    }
                }
                out[(y * wi + x) as usize] = (sum / n) as u8;
            }
        }
        out
    }

    /// Frontal landmarks placed inside the given box.
    fn frontal_landmarks(x: f32, y: f32, w: f32, h: f32) -> Landmarks {
        let eye_y = y + h * 0.35;
        let right_eye = (x + w * 0.3, eye_y);
        let left_eye = (x + w * 0.7, eye_y);
        Landmarks {
            right_eye,
            left_eye,
            nose: (x + w * 0.5, y + h * 0.55),
            right_mouth: (x + w * 0.35, y + h * 0.75),
            left_mouth: (x + w * 0.65, y + h * 0.75),
        }
    }

    fn detection_with_box(x: f32, y: f32, w: f32, h: f32) -> Detection {
        Detection {
            box_: FaceBox {
                x,
                y,
                w,
                h,
                score: 0.9,
            },
            landmarks: frontal_landmarks(x, y, w, h),
        }
    }

    /// A box covering the whole frame, with frontal landmarks.
    fn centered_detection(w: u32, h: u32) -> Detection {
        detection_with_box(0.0, 0.0, w as f32, h as f32)
    }

    fn symmetric_detection(w: u32, h: u32) -> Detection {
        centered_detection(w, h)
    }

    /// Nose displaced a quarter of an inter-ocular distance toward -x.
    fn turned_left_detection(w: u32, h: u32) -> Detection {
        let mut d = centered_detection(w, h);
        let iod = d.landmarks.left_eye.0 - d.landmarks.right_eye.0;
        d.landmarks.nose.0 -= 0.25 * iod;
        d
    }

    // ---- the five metrics -------------------------------------------------

    #[test]
    fn blur_is_lower_for_a_blurred_image() {
        let sharp = checkerboard(64, 64);
        let blurred = box_blur(&sharp, 64, 64, 3);
        let d = centered_detection(64, 64);
        let sharp_blur = measure(&sharp, 64, 64, &d).blur;
        let blurred_blur = measure(&blurred, 64, 64, &d).blur;
        assert!(
            blurred_blur < sharp_blur,
            "blurred {blurred_blur} !< sharp {sharp_blur}"
        );
        assert!(
            blurred_blur < 1.0,
            "blurred image saturated the blur metric"
        );
    }

    #[test]
    fn blur_is_measured_over_the_face_box_only() {
        // Sharp detail lives only in the left half; the box selects the flat
        // right half, so the frame-wide sharpness must not leak in.
        let mut frame = vec![128u8; 64 * 64];
        for y in 0..64usize {
            for x in 0..32usize {
                frame[y * 64 + x] = if (x + y) % 2 == 0 { 0 } else { 255 };
            }
        }
        let flat_half = detection_with_box(32.0, 0.0, 32.0, 64.0);
        let sharp_half = detection_with_box(0.0, 0.0, 32.0, 64.0);
        assert_eq!(measure(&frame, 64, 64, &flat_half).blur, 0.0);
        assert!(measure(&frame, 64, 64, &sharp_half).blur > 0.9);
    }

    #[test]
    fn illumination_tracks_mean_intensity() {
        let dark = vec![20u8; 64 * 64];
        let bright = vec![200u8; 64 * 64];
        let d = centered_detection(64, 64);
        assert!(measure(&dark, 64, 64, &d).illumination < 0.2);
        assert!(measure(&bright, 64, 64, &d).illumination > 0.7);
    }

    #[test]
    fn illumination_is_peaked_not_monotonic() {
        let d = centered_detection(64, 64);
        let score = |v: u8| measure(&vec![v; 64 * 64], 64, 64, &d).illumination;
        let well_lit = score(130);
        let blown_out = score(255);
        let dark = score(20);
        assert_eq!(well_lit, 1.0);
        // A blown-out frame must score LOW, not high: the band has a peak.
        assert!(blown_out < 0.3, "blown-out scored {blown_out}");
        assert!(dark < 0.3, "dark scored {dark}");
        assert!(score(240) < score(200), "bright side is not decreasing");
        assert!(score(60) < score(100), "dark side is not increasing");
    }

    // ---- saturation (review finding F6) -----------------------------------

    #[test]
    fn saturated_fraction_is_one_for_a_blown_out_face_box() {
        let frame = vec![255u8; 64 * 64];
        let d = centered_detection(64, 64);
        let m = measure(&frame, 64, 64, &d);
        assert_eq!(m.saturated_fraction, 1.0, "metrics {m:?}");
    }

    #[test]
    fn saturated_fraction_is_zero_for_a_well_exposed_face_box() {
        let frame = vec![130u8; 64 * 64];
        let d = centered_detection(64, 64);
        assert_eq!(measure(&frame, 64, 64, &d).saturated_fraction, 0.0);
    }

    #[test]
    fn saturated_fraction_is_measured_over_the_face_box_only() {
        // Left half blown out, right half correctly exposed.
        let mut frame = vec![130u8; 64 * 64];
        for y in 0..64usize {
            for x in 0..32usize {
                frame[y * 64 + x] = 255;
            }
        }
        let blown = detection_with_box(0.0, 0.0, 32.0, 64.0);
        let ok = detection_with_box(32.0, 0.0, 32.0, 64.0);
        assert_eq!(measure(&frame, 64, 64, &blown).saturated_fraction, 1.0);
        assert_eq!(measure(&frame, 64, 64, &ok).saturated_fraction, 0.0);
    }

    #[test]
    fn saturated_fraction_tolerates_a_specular_highlight() {
        // 2% of the box at full white is a glint, not a blown-out frame.
        let mut frame = vec![130u8; 100 * 100];
        for px in frame.iter_mut().take(200) {
            *px = 255;
        }
        let d = centered_detection(100, 100);
        let f = measure(&frame, 100, 100, &d).saturated_fraction;
        assert!((f - 0.02).abs() < 1e-3, "saturated_fraction {f}");
    }

    #[test]
    fn saturated_fraction_is_zero_when_pixels_are_unmeasurable() {
        // Truncated buffer: no pixel-derived metric is measurable, and the
        // saturation term must not accuse an unreadable frame of being blown
        // out (review finding F8 ambiguity).
        let frame = vec![255u8; 10];
        let d = centered_detection(64, 64);
        let m = measure(&frame, 64, 64, &d);
        assert_eq!(m.saturated_fraction, 0.0, "metrics {m:?}");
    }

    #[test]
    fn illumination_uses_the_face_box_not_the_whole_frame() {
        // Black frame with a well-lit face region.
        let mut frame = vec![0u8; 64 * 64];
        for y in 16..48usize {
            for x in 16..48usize {
                frame[y * 64 + x] = 130;
            }
        }
        let face = detection_with_box(16.0, 0.0 + 16.0, 32.0, 32.0);
        assert_eq!(measure(&frame, 64, 64, &face).illumination, 1.0);
        let whole = centered_detection(64, 64);
        assert!(measure(&frame, 64, 64, &whole).illumination < 0.3);
    }

    #[test]
    fn yaw_is_near_zero_for_symmetric_landmarks() {
        let d = symmetric_detection(64, 64);
        assert!(measure(&vec![128u8; 64 * 64], 64, 64, &d).yaw_deg.abs() < 5.0);
    }

    #[test]
    fn yaw_is_signed_for_asymmetric_landmarks() {
        let d = turned_left_detection(64, 64);
        assert!(measure(&vec![128u8; 64 * 64], 64, 64, &d).yaw_deg < -10.0);
    }

    #[test]
    fn yaw_is_opposite_signed_for_the_other_direction() {
        let mut d = centered_detection(64, 64);
        let iod = d.landmarks.left_eye.0 - d.landmarks.right_eye.0;
        d.landmarks.nose.0 += 0.25 * iod;
        assert!(measure(&vec![128u8; 64 * 64], 64, 64, &d).yaw_deg > 10.0);
    }

    #[test]
    fn yaw_is_independent_of_roll() {
        // Rotate the frontal landmarks 30 deg about the eye midpoint: the
        // face is still frontal, only rolled, so yaw must stay ~0.
        let mut d = centered_detection(64, 64);
        let mid = (
            (d.landmarks.left_eye.0 + d.landmarks.right_eye.0) * 0.5,
            (d.landmarks.left_eye.1 + d.landmarks.right_eye.1) * 0.5,
        );
        let (s, c) = (30f32).to_radians().sin_cos();
        let rot = |p: (f32, f32)| {
            let (dx, dy) = (p.0 - mid.0, p.1 - mid.1);
            (mid.0 + dx * c - dy * s, mid.1 + dx * s + dy * c)
        };
        d.landmarks.right_eye = rot(d.landmarks.right_eye);
        d.landmarks.left_eye = rot(d.landmarks.left_eye);
        d.landmarks.nose = rot(d.landmarks.nose);
        let m = measure(&vec![128u8; 64 * 64], 64, 64, &d);
        assert!(m.yaw_deg.abs() < 5.0, "yaw leaked from roll: {}", m.yaw_deg);
        assert!((m.roll_deg - 30.0).abs() < 0.01, "roll {}", m.roll_deg);
    }

    #[test]
    fn roll_is_zero_for_level_eyes_and_signed_otherwise() {
        let f = vec![128u8; 64 * 64];
        let level = centered_detection(64, 64);
        assert!(measure(&f, 64, 64, &level).roll_deg.abs() < 0.01);

        let mut tilted = centered_detection(64, 64);
        // Left eye lower in the image (larger y) => positive roll.
        tilted.landmarks.left_eye.1 += tilted.landmarks.left_eye.0 - tilted.landmarks.right_eye.0;
        assert!((measure(&f, 64, 64, &tilted).roll_deg - 45.0).abs() < 0.01);

        let mut other = centered_detection(64, 64);
        other.landmarks.left_eye.1 -= other.landmarks.left_eye.0 - other.landmarks.right_eye.0;
        assert!((measure(&f, 64, 64, &other).roll_deg + 45.0).abs() < 0.01);
    }

    #[test]
    fn face_fraction_grows_with_box_size() {
        let small = detection_with_box(10.0, 10.0, 8.0, 8.0);
        let large = detection_with_box(10.0, 10.0, 40.0, 40.0);
        let f = vec![128u8; 64 * 64];
        assert!(
            measure(&f, 64, 64, &small).face_fraction < measure(&f, 64, 64, &large).face_fraction
        );
    }

    #[test]
    fn face_fraction_is_area_over_frame_area() {
        let f = vec![128u8; 64 * 64];
        let d = detection_with_box(0.0, 0.0, 32.0, 32.0);
        let got = measure(&f, 64, 64, &d).face_fraction;
        assert!((got - 0.25).abs() < 1e-6, "{got}");
    }

    #[test]
    fn detection_confidence_is_passed_through_clamped() {
        let f = vec![128u8; 64 * 64];
        let mut d = centered_detection(64, 64);
        d.box_.score = 0.42;
        assert!((measure(&f, 64, 64, &d).detection_confidence - 0.42).abs() < 1e-6);
        d.box_.score = 7.5;
        assert_eq!(measure(&f, 64, 64, &d).detection_confidence, 1.0);
        d.box_.score = -3.0;
        assert_eq!(measure(&f, 64, 64, &d).detection_confidence, 0.0);
    }

    #[test]
    fn landmark_spread_is_iod_over_box_width() {
        let f = vec![128u8; 64 * 64];
        let d = centered_detection(64, 64);
        // Eyes at 0.3w and 0.7w => spread 0.4.
        let got = measure(&f, 64, 64, &d).landmark_spread;
        assert!((got - 0.4).abs() < 1e-6, "{got}");
    }

    // ---- degenerate input -------------------------------------------------

    #[test]
    fn zero_sized_box_is_safe() {
        let f = vec![128u8; 64 * 64];
        let d = detection_with_box(10.0, 10.0, 0.0, 0.0);
        let m = measure(&f, 64, 64, &d);
        assert_eq!(m.blur, 0.0);
        assert_eq!(m.illumination, 0.0);
        assert_eq!(m.face_fraction, 0.0);
        assert_eq!(m.landmark_spread, 0.0);
    }

    #[test]
    fn negative_box_dimensions_are_safe() {
        let f = vec![128u8; 64 * 64];
        let mut d = centered_detection(64, 64);
        d.box_.w = -10.0;
        d.box_.h = -10.0;
        let m = measure(&f, 64, 64, &d);
        assert_eq!(m.face_fraction, 0.0);
        assert_eq!(m.blur, 0.0);
        assert_eq!(m.landmark_spread, 0.0);
    }

    #[test]
    fn box_wholly_outside_the_frame_is_safe() {
        let f = vec![128u8; 64 * 64];
        for d in [
            detection_with_box(200.0, 200.0, 16.0, 16.0),
            detection_with_box(-100.0, -100.0, 16.0, 16.0),
        ] {
            let m = measure(&f, 64, 64, &d);
            assert_eq!(m.blur, 0.0);
            assert_eq!(m.illumination, 0.0);
        }
    }

    #[test]
    fn box_partially_outside_the_frame_is_clipped() {
        let f = vec![130u8; 64 * 64];
        // Half of the box hangs off the right/bottom edge.
        let d = detection_with_box(48.0, 48.0, 32.0, 32.0);
        let m = measure(&f, 64, 64, &d);
        assert_eq!(m.illumination, 1.0, "clipped ROI should still be sampled");
        // Negative origin clips too.
        let d = detection_with_box(-16.0, -16.0, 32.0, 32.0);
        assert_eq!(measure(&f, 64, 64, &d).illumination, 1.0);
    }

    #[test]
    fn empty_frame_dimensions_are_safe() {
        let d = detection_with_box(0.0, 0.0, 8.0, 8.0);
        for (w, h) in [(0u32, 0u32), (0, 64), (64, 0)] {
            let m = measure(&[], w, h, &d);
            assert_eq!(m.face_fraction, 0.0);
            assert_eq!(m.blur, 0.0);
            assert_eq!(m.illumination, 0.0);
        }
    }

    #[test]
    fn short_buffer_is_safe() {
        let f = vec![130u8; 10]; // far shorter than 64*64
        let d = centered_detection(64, 64);
        let m = measure(&f, 64, 64, &d);
        assert_eq!(m.blur, 0.0);
        assert_eq!(m.illumination, 0.0);
        // Geometry is still measurable from the detection alone.
        assert!(m.face_fraction > 0.0);
        assert!(m.landmark_spread > 0.0);
    }

    #[test]
    fn coincident_landmarks_do_not_divide_by_zero() {
        let f = vec![128u8; 64 * 64];
        let mut d = centered_detection(64, 64);
        let p = (32.0, 32.0);
        d.landmarks = Landmarks {
            right_eye: p,
            left_eye: p,
            nose: p,
            right_mouth: p,
            left_mouth: p,
        };
        let m = measure(&f, 64, 64, &d);
        assert_eq!(m.yaw_deg, 0.0);
        assert_eq!(m.roll_deg, 0.0);
        assert_eq!(m.landmark_spread, 0.0);
        assert!(m.yaw_deg.is_finite() && m.roll_deg.is_finite());
    }

    #[test]
    fn non_finite_coordinates_are_neutralised() {
        let f = vec![128u8; 64 * 64];
        let mut d = centered_detection(64, 64);
        d.box_.x = f32::NAN;
        d.box_.w = f32::INFINITY;
        d.box_.score = f32::NAN;
        d.landmarks.nose = (f32::NAN, f32::NAN);
        let m = measure(&f, 64, 64, &d);
        assert!(m.blur.is_finite());
        assert!(m.illumination.is_finite());
        assert!(m.yaw_deg.is_finite());
        assert!(m.roll_deg.is_finite());
        assert_eq!(m.detection_confidence, 0.0);
        assert_eq!(m.face_fraction, 0.0);
    }

    #[test]
    fn tiny_box_below_the_laplacian_kernel_is_safe() {
        let f = checkerboard(64, 64);
        for (bw, bh) in [(1.0, 1.0), (2.0, 2.0), (1.0, 20.0)] {
            let d = detection_with_box(10.0, 10.0, bw, bh);
            let m = measure(&f, 64, 64, &d);
            assert_eq!(m.blur, 0.0, "{bw}x{bh}");
            // A mean is still well defined for a sub-kernel ROI.
            assert!(m.illumination.is_finite());
        }
    }

    #[test]
    fn landmark_spread_clamps_when_eyes_exceed_the_box() {
        let f = vec![128u8; 64 * 64];
        let mut d = detection_with_box(20.0, 20.0, 10.0, 10.0);
        d.landmarks.right_eye = (0.0, 25.0);
        d.landmarks.left_eye = (60.0, 25.0);
        assert_eq!(measure(&f, 64, 64, &d).landmark_spread, 1.0);
    }

    /// Review finding F7, pinned executably: the denominator is the **full**
    /// `w * h` buffer, not the `(w - 2) * (h - 2)` interior the kernel writes.
    ///
    /// On an 8x8 one-pixel checkerboard every interior Laplacian response is
    /// exactly +-1020 and every border response is the 0.0 it was initialised
    /// to, in a 18/18 sign balance, so the mean is 0 and the result reduces to
    /// `1020 * sqrt(interior / total)`:
    ///
    /// * full-buffer denominator (what this returns): `1020 * sqrt(36/64)` = **765**
    /// * interior-only denominator (what the name "variance of Laplacian"
    ///   would normally imply): **1020**
    ///
    /// Keeping the border in is a deliberate decision, not an oversight: the
    /// CLI's hard `blur < 3.0` reject and [`BLUR_SHARP_AT`] are both
    /// calibrated against these numbers. If this test ever fails at 765, the
    /// denominator changed and both gates need retuning in the same change.
    #[test]
    fn laplacian_stddev_averages_the_zeroed_border_in() {
        let got = laplacian_stddev(&checkerboard(8, 8), 8, 8);
        assert!((got - 765.0).abs() < 0.01, "expected 765.0, got {got}");
        // Sanity: the interior-only figure this deliberately is NOT.
        assert!((got - 1020.0).abs() > 1.0, "border was excluded: {got}");
    }

    /// The border's share shrinks as the ROI grows, so the same measurement is
    /// not comparable across ROI sizes in absolute terms — only the ordering
    /// between frames of one size is. Documented here so nobody reads
    /// [`FrameMetrics::blur`] as an absolute sharpness in physical units.
    #[test]
    fn laplacian_stddev_border_bias_shrinks_with_size() {
        let small = laplacian_stddev(&checkerboard(8, 8), 8, 8);
        let large = laplacian_stddev(&checkerboard(64, 64), 64, 64);
        assert!(small < large, "small {small} !< large {large}");
        assert!(
            large < 1020.0,
            "large {large} reached the interior-only value"
        );
    }

    #[test]
    fn laplacian_stddev_rejects_undersized_input() {
        assert_eq!(laplacian_stddev(&[1, 2, 3, 4], 2, 2), 0.0);
        assert_eq!(laplacian_stddev(&[], 0, 0), 0.0);
        assert_eq!(laplacian_stddev(&[1, 2, 3], 64, 64), 0.0);
    }

    #[test]
    fn metrics_are_independent_of_one_another() {
        // The whole point of the module: a sharp-but-dark frame and a
        // blurry-but-well-lit frame must not produce the same numbers.
        let d = centered_detection(64, 64);
        let sharp_dark: Vec<u8> = checkerboard(64, 64)
            .iter()
            .map(|&v| (v as u32 * 30 / 255) as u8)
            .collect();
        let flat_lit = vec![130u8; 64 * 64];
        let a = measure(&sharp_dark, 64, 64, &d);
        let b = measure(&flat_lit, 64, 64, &d);
        assert!(a.blur > b.blur);
        assert!(a.illumination < b.illumination);
    }
}
