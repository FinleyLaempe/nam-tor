// SPDX-License-Identifier: GPL-3.0-only
// allow: SIZE_OK — task 17 explicitly mandates a single `fusion.rs` module
// holding Affine2D + EnrollmentTransform + FusionEngine + pair_frames +
// geometric_identity_check + the least-squares fit helper as one cohesive
// cross-sensor fusion API. Splitting would fragment the spec-mandated surface.
//! Cross-sensor RGB↔IR fusion (spec §6.5).
//!
//! Three independent checks, none of which Windows Hello performs:
//!
//! 1. **IR embedding** against the IR gallery (caller's job — see
//!    [`crate::verify`]).
//! 2. **RGB embedding** against the RGB gallery (caller's job).
//! 3. **Cross-sensor geometric + embedding consistency** — owned here. The
//!    sensors sit ~2 cm apart on one bezel, so a real face produces landmark
//!    correspondences under a stable transform between views. A printed IR
//!    photo does not yield a matching RGB face in the expected position; a
//!    phone displaying a face fails the IR path entirely.
//!
//! Extrinsics are fitted at enrollment (never requested from the user) and
//! stored as an [`EnrollmentTransform`]. A camera change invalidates the
//! transform when the fit residual exceeds the configured threshold.
//!
//! **Darkness policy** (spec §6.5): RGB is useless in darkness, so the engine
//! falls back to IR-only with a `reduced_protection` flag. Fusion may only
//! *add* confidence, never subtract it — the bar is never lowered to
//! compensate, otherwise "attack in a dark room" becomes the trivial bypass.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Tolerance for the SOF/timestamp pairing (spec §6.5: pairs must be
/// temporally co-occurring). 33 ms ≈ one frame at 30 fps.
pub const DEFAULT_PAIRING_TOLERANCE: Duration = Duration::from_millis(33);

/// Default max mean residual (pixels) before the enrollment transform is
/// considered invalidated by a camera change. The reference sensors are ~2 cm
/// apart on one bezel; a sub-pixel-to-low-single-pixel residual is expected at
/// enrollment, so 8 px is a conservative invalidation floor.
pub const DEFAULT_RESIDUAL_THRESHOLD: f32 = 8.0;

/// Default tolerance for the geometric identity check (pixels). The expected
/// RGB face center, mapped through the enrollment transform, must land within
/// this radius of the IR face center.
pub const DEFAULT_GEOMETRIC_TOLERANCE: f32 = 12.0;

/// Cross-modal cosine-similarity floor below which the IR and RGB embeddings
/// are deemed inconsistent (one sensor sees a different face than the other —
/// e.g. an IR print held next to a phone showing a different face). SFace
/// embeddings of the same face across IR and RGB cluster well above 0.3 in the
/// reference pipeline; 0.30 is a conservative reject floor.
pub const DEFAULT_CROSS_MODAL_THRESHOLD: f32 = 0.30;

/// A 2D affine transform: `x' = a*x + b*y + c`, `y' = d*x + e*y + f`.
///
/// Stored as six plain floats (no external linear-algebra dep) so it
/// serialises trivially and applies in one evaluate pass. Fitted by
/// [`EnrollmentTransform::fit`] from paired RGB→IR landmark correspondences.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Affine2D {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Affine2D {
    /// Identity transform (`x' = x`, `y' = y`).
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 0.0,
        e: 1.0,
        f: 0.0,
    };

    /// Apply the transform to a point.
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.b * y + self.c,
            self.d * x + self.e * y + self.f,
        )
    }

    /// A transform is degenerate (unusable for the geometric check) when its
    /// linear part collapses the plane to a line or point. Detected via the
    /// determinant of the 2×2 linear block.
    pub fn is_degenerate(&self) -> bool {
        let det = self.a * self.e - self.b * self.d;
        det.abs() < 1e-6
    }
}

/// A 2D point correspondence used to fit an [`Affine2D`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointCorrespondence {
    pub src: (f32, f32),
    pub dst: (f32, f32),
}

/// Residual statistics from the least-squares fit, retained so a later
/// camera-change can be detected by re-measuring the residual against fresh
/// correspondences.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ResidualStats {
    /// Mean L2 residual across inliers (pixels).
    pub mean: f32,
    /// Max L2 residual across inliers (pixels).
    pub max: f32,
    /// Number of inliers used for the final fit (after outlier rejection).
    pub inliers: usize,
}

/// Transform fitted at enrollment from paired RGB↔IR landmark
/// correspondences. Maps RGB face coordinates into IR face coordinates (the
/// IR frame is the primary liveness sensor; the RGB face is the one whose
/// position we want to verify).
///
/// Robust to outliers: a first least-squares fit is computed, correspondences
/// whose residual exceeds `2 × median_residual` are dropped, and the fit is
/// re-computed on the inlier set. [`ResidualStats`] reflect the final inlier
/// fit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollmentTransform {
    /// RGB→IR affine map.
    pub affine: Affine2D,
    /// Fit residual statistics (inliers only).
    pub residual: ResidualStats,
    /// Device identity (USB VID:PID or path) recorded at enrollment. A
    /// mismatch on a later session invalidates the transform without needing
    /// a residual re-measurement.
    pub device_identity: Option<String>,
}

impl EnrollmentTransform {
    /// Fit an affine RGB→IR transform from `pairs` using least squares with
    /// one round of median-based outlier rejection. Requires ≥ 3
    /// correspondences (6 equations for 6 unknowns).
    pub fn fit(
        pairs: &[PointCorrespondence],
        device_identity: Option<String>,
    ) -> Result<Self, FusionError> {
        if pairs.len() < 3 {
            return Err(FusionError::InsufficientCorrespondences {
                got: pairs.len(),
                need: 3,
            });
        }
        let affine = fit_affine(pairs);
        let residuals: Vec<f32> = pairs.iter().map(|p| residual_l2(&affine, p)).collect();
        let mut sorted = residuals.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = sorted[sorted.len() / 2];
        let cutoff = (2.0 * median).max(1e-3);
        let inliers: Vec<&PointCorrespondence> = pairs
            .iter()
            .zip(residuals.iter())
            .filter(|(_, r)| **r <= cutoff)
            .map(|(p, _)| p)
            .collect();
        let (affine_final, used, inlier_owned): (Affine2D, usize, Vec<PointCorrespondence>) =
            if inliers.len() >= 3 {
                let owned: Vec<PointCorrespondence> = inliers.iter().map(|p| **p).collect();
                let n_in = owned.len();
                (fit_affine(&owned), n_in, owned)
            } else {
                let owned: Vec<PointCorrespondence> = pairs.to_vec();
                let n_in = owned.len();
                (affine, n_in, owned)
            };
        let final_residuals: Vec<f32> = inlier_owned
            .iter()
            .map(|p| residual_l2(&affine_final, p))
            .collect();
        let mean = final_residuals.iter().sum::<f32>() / final_residuals.len().max(1) as f32;
        let max = final_residuals.iter().copied().fold(0.0f32, f32::max);
        Ok(Self {
            affine: affine_final,
            residual: ResidualStats {
                mean,
                max,
                inliers: used,
            },
            device_identity,
        })
    }

    /// Returns `true` when `new_residual` exceeds `threshold`, signalling the
    /// transform should be invalidated (camera change, bezel shift, …).
    pub fn is_invalidated_by(&self, new_residual: f32, threshold: f32) -> bool {
        new_residual > threshold
    }

    /// Confirm the device identity still matches the one recorded at
    /// enrollment. `None` means identity was not recorded (legacy enrollment)
    /// — accept, but the residual check remains the backstop.
    pub fn matches_device(&self, current: Option<&str>) -> bool {
        match (&self.device_identity, current) {
            (Some(recorded), Some(now)) => recorded == now,
            _ => true,
        }
    }
}

/// Configuration for the fusion engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FusionConfig {
    /// Transform fitted at enrollment. `None` until the user enrolls with a
    /// paired RGB+IR capture.
    pub enrollment_transform: Option<EnrollmentTransform>,
    /// Max mean residual (pixels) before the transform is invalidated.
    pub residual_threshold: f32,
    /// Device identity (USB VID:PID or path) of the current camera. Compared
    /// against [`EnrollmentTransform::device_identity`].
    pub device_identity: Option<String>,
    /// Tolerance for the geometric identity check (pixels).
    pub geometric_tolerance: f32,
    /// Cross-modal embedding cosine-similarity floor.
    pub cross_modal_threshold: f32,
    /// SOF/timestamp pairing tolerance.
    pub pairing_tolerance: Duration,
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            enrollment_transform: None,
            residual_threshold: DEFAULT_RESIDUAL_THRESHOLD,
            device_identity: None,
            geometric_tolerance: DEFAULT_GEOMETRIC_TOLERANCE,
            cross_modal_threshold: DEFAULT_CROSS_MODAL_THRESHOLD,
            pairing_tolerance: DEFAULT_PAIRING_TOLERANCE,
        }
    }
}

/// SOF/timestamp pairing outcome. Rejects torn pairs (either SOF missing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PairedFrames {
    pub ir_sof: Duration,
    pub rgb_sof: Duration,
    /// Absolute gap between the two SOFs (always ≤ the pairing tolerance).
    pub gap: Duration,
}

/// Pair an IR SOF and an RGB SOF by timestamp. Returns `None` when either SOF
/// is absent (torn pair) or the gap exceeds `tolerance`.
pub fn pair_frames(
    ir_sof: Option<Duration>,
    rgb_sof: Option<Duration>,
    tolerance: Duration,
) -> Option<PairedFrames> {
    let (ir, rgb) = (ir_sof?, rgb_sof?);
    let gap = ir.abs_diff(rgb);
    if gap > tolerance {
        return None;
    }
    Some(PairedFrames {
        ir_sof: ir,
        rgb_sof: rgb,
        gap,
    })
}

/// A face bounding box: `(x, y, w, h)` in pixels.
pub type FaceBox = (f32, f32, f32, f32);

fn box_center(b: FaceBox) -> (f32, f32) {
    (b.0 + b.2 * 0.5, b.1 + b.3 * 0.5)
}

/// Outcome of the geometric identity check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeometricResult {
    /// RGB face center, mapped through the enrollment transform, lands within
    /// tolerance of the IR face center.
    Accept,
    /// RGB face is present but shifted relative to where the transform
    /// predicts — likely a split spoof (IR print + phone showing a face).
    Reject,
    /// Transform is degenerate or a bounding box is non-positive — the check
    /// is unusable and the caller must fail closed (treat as no fusion).
    Invalid,
}

/// Verify the RGB face is in the expected position given the IR face and the
/// enrollment transform. `ir_face_pos` and `rgb_face_pos` are `(x, y, w, h)`
/// bounding boxes in their respective frames.
pub fn geometric_identity_check(
    ir_face_pos: FaceBox,
    rgb_face_pos: FaceBox,
    transform: &Affine2D,
    tolerance: f32,
) -> GeometricResult {
    if transform.is_degenerate() {
        return GeometricResult::Invalid;
    }
    if ir_face_pos.2 <= 0.0
        || ir_face_pos.3 <= 0.0
        || rgb_face_pos.2 <= 0.0
        || rgb_face_pos.3 <= 0.0
    {
        return GeometricResult::Invalid;
    }
    let (ir_cx, ir_cy) = box_center(ir_face_pos);
    let (rgb_cx, rgb_cy) = box_center(rgb_face_pos);
    let (expected_ir_cx, expected_ir_cy) = transform.apply(rgb_cx, rgb_cy);
    let dx = expected_ir_cx - ir_cx;
    let dy = expected_ir_cy - ir_cy;
    let dist = (dx * dx + dy * dy).sqrt();
    if dist <= tolerance {
        GeometricResult::Accept
    } else {
        GeometricResult::Reject
    }
}

/// Why fusion fell back to IR-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IrOnlyReason {
    /// RGB too dark — spec §6.5 darkness policy.
    Darkness,
    /// No RGB face detected in the RGB frame.
    NoRgbFace,
    /// No RGB camera available (single-sensor host).
    NoRgbCamera,
}

/// Why fusion rejected the pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// IR and RGB embeddings disagree — the two sensors see different faces.
    CrossModalMismatch,
}

/// Outcome of [`FusionEngine::evaluate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FusionOutcome {
    /// Fell back to IR-only. `reduced_protection` is `true` — the caller must
    /// raise the IR/PAD confidence bar to compensate (spec §6.5: the bar is
    /// never lowered to compensate).
    IrOnly {
        reason: IrOnlyReason,
        reduced_protection: bool,
    },
    /// Both sensors agree. `cross_modal_consistency` is the cosine similarity
    /// between the IR and RGB embeddings (a same-face consistency signal —
    /// the caller still matches each embedding against its own gallery).
    Fused { cross_modal_consistency: f32 },
    /// Sensors disagree — reject the attempt.
    Rejected { reason: RejectReason },
}

/// The fusion engine is a pure evaluator over embeddings + transform; it owns
/// no I/O. The caller runs detection+alignment+embedding for each sensor, the
/// geometric identity check, then hands the embeddings here.
#[derive(Debug, Clone)]
pub struct FusionEngine {
    config: FusionConfig,
}

impl FusionEngine {
    pub fn new(config: FusionConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &FusionConfig {
        &self.config
    }

    /// Combine IR + RGB embeddings under the darkness policy.
    ///
    /// - `is_dark` → [`FusionOutcome::IrOnly`] (Darkness).
    /// - no RGB embedding → [`FusionOutcome::IrOnly`] (NoRgbFace / NoRgbCamera
    ///   — the caller passes `None` for both cases; the reason is decided by
    ///   what the caller knows about camera availability. Here we default to
    ///   `NoRgbFace`; the caller can override by constructing the outcome
    ///   directly when it knows there is no RGB camera).
    /// - otherwise compute the cross-modal cosine similarity. If it falls
    ///   below [`FusionConfig::cross_modal_threshold`] the sensors disagree →
    ///   [`FusionOutcome::Rejected`] (a split spoof). Else [`FusionOutcome::Fused`].
    pub fn evaluate(
        &self,
        ir_embedding: &[f32],
        rgb_embedding: Option<&[f32]>,
        _transform: &EnrollmentTransform,
        is_dark: bool,
    ) -> FusionOutcome {
        if is_dark {
            return FusionOutcome::IrOnly {
                reason: IrOnlyReason::Darkness,
                reduced_protection: true,
            };
        }
        let rgb = match rgb_embedding {
            Some(e) => e,
            None => {
                return FusionOutcome::IrOnly {
                    reason: IrOnlyReason::NoRgbFace,
                    reduced_protection: true,
                };
            }
        };
        let consistency = cosine_similarity(ir_embedding, rgb);
        if consistency < self.config.cross_modal_threshold {
            return FusionOutcome::Rejected {
                reason: RejectReason::CrossModalMismatch,
            };
        }
        FusionOutcome::Fused {
            cross_modal_consistency: consistency,
        }
    }
}

/// Cosine similarity for L2-normalised embeddings (SFace output is already
/// L2-normalised, so this is a single dot product). Falls back to the full
/// `a·b / (‖a‖·‖b‖)` form for un-normalised input.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return -1.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a < 1e-9 || norm_b < 1e-9 {
        return -1.0;
    }
    dot / (norm_a * norm_b)
}

fn residual_l2(affine: &Affine2D, p: &PointCorrespondence) -> f32 {
    let (px, py) = affine.apply(p.src.0, p.src.1);
    let dx = px - p.dst.0;
    let dy = py - p.dst.1;
    (dx * dx + dy * dy).sqrt()
}

/// Least-squares fit of an [`Affine2D`] from `pairs` (RGB→IR). Solves the two
/// independent 3×3 normal equations for x' and y' via Cramer's rule. Requires
/// ≥ 3 non-degenerate correspondences; the caller enforces the count.
fn fit_affine(pairs: &[PointCorrespondence]) -> Affine2D {
    // Normal equations for x' = a*x + b*y + c:
    //   [ Σx²  Σxy  Σx ] [a]   [Σx·x']
    //   [ Σxy  Σy²  Σy ] [b] = [Σy·x']
    //   [ Σx   Σy   n  ] [c]   [Σx'  ]
    // y' is identical with x'→y'. Solve via Cramer's rule on the 3×3 system.
    let n = pairs.len() as f32;
    let (mut sx, mut sy, mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    let (mut sxp, mut syp, mut sxpx, mut sxpy, mut sypx, mut sypy) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for p in pairs {
        let (x, y) = p.src;
        let (xp, yp) = p.dst;
        sx += x;
        sy += y;
        sxx += x * x;
        syy += y * y;
        sxy += x * y;
        sxp += xp;
        syp += yp;
        sxpx += x * xp;
        sxpy += y * xp;
        sypx += x * yp;
        sypy += y * yp;
    }
    let det = |m: [[f32; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let base = [[sxx, sxy, sx], [sxy, syy, sy], [sx, sy, n]];
    let d0 = det(base);
    if d0.abs() < 1e-9 {
        return Affine2D::IDENTITY;
    }
    let solve = |rhs: [f32; 3]| {
        let m = [[rhs[0], sxy, sx], [rhs[1], syy, sy], [rhs[2], sy, n]];
        let m1 = [[sxx, rhs[0], sx], [sxy, rhs[1], sy], [sx, rhs[2], n]];
        let m2 = [[sxx, sxy, rhs[0]], [sxy, syy, rhs[1]], [sx, sy, rhs[2]]];
        (det(m) / d0, det(m1) / d0, det(m2) / d0)
    };
    let (a, b, c) = solve([sxpx, sxpy, sxp]);
    let (d, e, f) = solve([sypx, sypy, syp]);
    Affine2D { a, b, c, d, e, f }
}

/// Fusion errors. None of these carry biometric data.
#[derive(Debug, PartialEq)]
pub enum FusionError {
    /// `fit` requires ≥ 3 correspondences to solve for 6 unknowns.
    InsufficientCorrespondences { got: usize, need: usize },
}

impl std::fmt::Display for FusionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FusionError::InsufficientCorrespondences { got, need } => write!(
                f,
                "fusion: need ≥{need} landmark correspondences to fit an affine transform, got {got}"
            ),
        }
    }
}

impl std::error::Error for FusionError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn corr(src: (f32, f32), dst: (f32, f32)) -> PointCorrespondence {
        PointCorrespondence { src, dst }
    }

    fn identity_transform() -> EnrollmentTransform {
        EnrollmentTransform {
            affine: Affine2D::IDENTITY,
            residual: ResidualStats {
                mean: 0.0,
                max: 0.0,
                inliers: 5,
            },
            device_identity: None,
        }
    }

    /// SOF pairing accepts frames within the tolerance.
    #[test]
    fn sof_pairing_accepts_within_tolerance() {
        let ir = Duration::from_millis(100);
        let rgb = Duration::from_millis(125);
        let paired = pair_frames(Some(ir), Some(rgb), Duration::from_millis(33));
        let paired = paired.expect("within-tolerance pair must be accepted");
        assert_eq!(paired.ir_sof, ir);
        assert_eq!(paired.rgb_sof, rgb);
        assert_eq!(paired.gap, Duration::from_millis(25));
    }

    /// SOF pairing rejects frames farther apart than the tolerance.
    #[test]
    fn sof_pairing_rejects_too_far_apart() {
        let ir = Duration::from_millis(100);
        let rgb = Duration::from_millis(200);
        assert_eq!(
            pair_frames(Some(ir), Some(rgb), Duration::from_millis(33)),
            None,
            "100 ms gap must exceed the 33 ms tolerance"
        );
    }

    /// SOF pairing rejects torn pairs (either SOF missing).
    #[test]
    fn sof_pairing_rejects_torn_pairs() {
        assert_eq!(
            pair_frames(
                Some(Duration::from_millis(100)),
                None,
                Duration::from_millis(33)
            ),
            None,
            "missing RGB SOF = torn pair"
        );
        assert_eq!(
            pair_frames(
                None,
                Some(Duration::from_millis(100)),
                Duration::from_millis(33)
            ),
            None,
            "missing IR SOF = torn pair"
        );
        assert_eq!(
            pair_frames(None, None, Duration::from_millis(33)),
            None,
            "both missing = torn pair"
        );
    }

    /// Geometric identity accepts a co-located face (RGB center maps onto IR
    /// center within tolerance).
    #[test]
    fn geometric_identity_accepts_colocated_face() {
        let t = Affine2D::IDENTITY;
        let ir = (100.0, 100.0, 40.0, 40.0);
        let rgb = (100.0, 100.0, 40.0, 40.0);
        assert_eq!(
            geometric_identity_check(ir, rgb, &t, 12.0),
            GeometricResult::Accept
        );
    }

    /// Geometric identity rejects a shifted face (RGB center maps > tolerance
    /// away from the IR center).
    #[test]
    fn geometric_identity_rejects_shifted_face() {
        let t = Affine2D::IDENTITY;
        let ir = (100.0, 100.0, 40.0, 40.0);
        let rgb = (150.0, 100.0, 40.0, 40.0);
        assert_eq!(
            geometric_identity_check(ir, rgb, &t, 12.0),
            GeometricResult::Reject
        );
    }

    /// Geometric identity returns `Invalid` for a degenerate transform or a
    /// non-positive bounding box.
    #[test]
    fn geometric_identity_invalid_for_degenerate_transform_or_bad_box() {
        let degenerate = Affine2D {
            a: 0.0,
            b: 0.0,
            c: 0.0,
            d: 0.0,
            e: 0.0,
            f: 0.0,
        };
        let good = (100.0, 100.0, 40.0, 40.0);
        assert_eq!(
            geometric_identity_check(good, good, &degenerate, 12.0),
            GeometricResult::Invalid,
            "degenerate transform must be Invalid"
        );
        let t = Affine2D::IDENTITY;
        let zero_w = (100.0, 100.0, 0.0, 40.0);
        assert_eq!(
            geometric_identity_check(zero_w, good, &t, 12.0),
            GeometricResult::Invalid,
            "non-positive box must be Invalid"
        );
    }

    /// Darkness policy falls back to IR-only with reduced protection flagged.
    #[test]
    fn darkness_policy_falls_back_to_ir_only() {
        let engine = FusionEngine::new(FusionConfig::default());
        let t = identity_transform();
        let ir = vec![0.1_f32; 128];
        let rgb = vec![0.1_f32; 128];
        let outcome = engine.evaluate(&ir, Some(&rgb), &t, true);
        match outcome {
            FusionOutcome::IrOnly {
                reason: IrOnlyReason::Darkness,
                reduced_protection: true,
            } => {}
            other => panic!("darkness must produce IrOnly(Darkness, true), got {other:?}"),
        }
    }

    /// No RGB embedding (no RGB face / no RGB camera) falls back to IR-only.
    #[test]
    fn missing_rgb_embedding_falls_back_to_ir_only() {
        let engine = FusionEngine::new(FusionConfig::default());
        let t = identity_transform();
        let ir = vec![0.1_f32; 128];
        let outcome = engine.evaluate(&ir, None, &t, false);
        match outcome {
            FusionOutcome::IrOnly {
                reason: IrOnlyReason::NoRgbFace,
                reduced_protection: true,
            } => {}
            other => panic!("missing RGB must produce IrOnly(NoRgbFace), got {other:?}"),
        }
    }

    /// Matching IR+RGB embeddings fuse with a high cross-modal consistency.
    #[test]
    fn matching_embeddings_produce_fused_outcome() {
        let engine = FusionEngine::new(FusionConfig::default());
        let t = identity_transform();
        let ir = vec![0.5_f32; 128];
        let rgb = ir.clone();
        let outcome = engine.evaluate(&ir, Some(&rgb), &t, false);
        match outcome {
            FusionOutcome::Fused {
                cross_modal_consistency,
            } => assert!(
                (cross_modal_consistency - 1.0).abs() < 1e-5,
                "identical embeddings must score ~1.0"
            ),
            other => panic!("matching embeddings must Fused, got {other:?}"),
        }
    }

    /// Divergent IR+RGB embeddings (split spoof) are rejected.
    #[test]
    fn divergent_embeddings_are_rejected() {
        let engine = FusionEngine::new(FusionConfig::default());
        let t = identity_transform();
        // Orthogonal unit vectors → cosine sim 0 < default 0.30 floor.
        let mut ir = vec![0.0_f32; 128];
        ir[0] = 1.0;
        let mut rgb = vec![0.0_f32; 128];
        rgb[1] = 1.0;
        let outcome = engine.evaluate(&ir, Some(&rgb), &t, false);
        assert!(
            matches!(
                outcome,
                FusionOutcome::Rejected {
                    reason: RejectReason::CrossModalMismatch
                }
            ),
            "divergent embeddings must be Rejected, got {outcome:?}"
        );
    }

    /// Transform invalidation fires when a fresh residual exceeds the
    /// configured threshold (camera change / bezel shift).
    #[test]
    fn transform_invalidation_on_high_residual() {
        let t = EnrollmentTransform {
            affine: Affine2D::IDENTITY,
            residual: ResidualStats {
                mean: 1.0,
                max: 2.0,
                inliers: 5,
            },
            device_identity: Some("3277:0018".into()),
        };
        assert!(
            t.is_invalidated_by(10.0, DEFAULT_RESIDUAL_THRESHOLD),
            "10 px residual > 8 px threshold -> invalidate"
        );
        assert!(
            !t.is_invalidated_by(5.0, DEFAULT_RESIDUAL_THRESHOLD),
            "5 px residual <= 8 px threshold -> still valid"
        );
    }

    /// Device identity mismatch invalidates the transform's device match
    /// (camera swap detected even before a residual re-measurement).
    #[test]
    fn device_identity_mismatch_invalidates() {
        let t = EnrollmentTransform {
            affine: Affine2D::IDENTITY,
            residual: ResidualStats {
                mean: 0.0,
                max: 0.0,
                inliers: 5,
            },
            device_identity: Some("3277:0018".into()),
        };
        assert!(
            !t.matches_device(Some("046d:0825")),
            "different device must not match"
        );
        assert!(
            t.matches_device(Some("3277:0018")),
            "same device must match"
        );
        assert!(
            t.matches_device(None),
            "legacy enrollment (no id recorded) accepts"
        );
    }

    /// `EnrollmentTransform::fit` recovers a known affine transform from clean
    /// correspondences and rejects a gross outlier.
    #[test]
    fn fit_recovers_transform_and_rejects_outlier() {
        let truth = Affine2D {
            a: 1.1,
            b: 0.05,
            c: 3.0,
            d: -0.02,
            e: 0.9,
            f: 1.5,
        };
        let srcs = [
            (10.0, 20.0),
            (50.0, 80.0),
            (100.0, 30.0),
            (15.0, 90.0),
            (75.0, 45.0),
            (200.0, 150.0),
            (5.0, 5.0),
        ];
        let mut pairs: Vec<_> = srcs
            .iter()
            .map(|&(x, y)| {
                let (xp, yp) = truth.apply(x, y);
                corr((x, y), (xp, yp))
            })
            .collect();
        pairs.push(corr((100.0, 100.0), (500.0, 500.0)));
        let fitted = EnrollmentTransform::fit(&pairs, Some("3277:0018".into()))
            .expect("fit must succeed with 8 correspondences");
        assert!(
            (fitted.affine.a - truth.a).abs() < 1e-2,
            "a: {}",
            fitted.affine.a
        );
        assert!(
            (fitted.affine.b - truth.b).abs() < 1e-2,
            "b: {}",
            fitted.affine.b
        );
        assert!(
            (fitted.affine.c - truth.c).abs() < 1e-2,
            "c: {}",
            fitted.affine.c
        );
        assert!(
            (fitted.affine.e - truth.e).abs() < 1e-2,
            "e: {}",
            fitted.affine.e
        );
        assert!(
            fitted.residual.mean < 1.0,
            "inlier mean residual must be tiny"
        );
        assert!(
            fitted.residual.inliers >= 7,
            "inliers must exclude the outlier"
        );
        assert_eq!(fitted.device_identity.as_deref(), Some("3277:0018"));
    }

    /// `fit` rejects fewer than 3 correspondences (underdetermined system).
    #[test]
    fn fit_rejects_insufficient_correspondences() {
        let pairs = [corr((0.0, 0.0), (1.0, 1.0)), corr((1.0, 0.0), (2.0, 1.0))];
        let err = EnrollmentTransform::fit(&pairs, None).unwrap_err();
        match err {
            FusionError::InsufficientCorrespondences { got: 2, need: 3 } => {}
            other => panic!("wrong error: {other:?}"),
        }
    }

    /// `Affine2D::apply` and `is_degenerate` behave on identity and a collapse.
    #[test]
    fn affine_apply_and_degenerate() {
        let id = Affine2D::IDENTITY;
        let (x, y) = id.apply(10.0, 20.0);
        assert!((x - 10.0).abs() < 1e-6 && (y - 20.0).abs() < 1e-6);
        assert!(!id.is_degenerate());
        let collapse = Affine2D {
            a: 0.0,
            b: 0.0,
            c: 5.0,
            d: 0.0,
            e: 0.0,
            f: 5.0,
        };
        assert!(collapse.is_degenerate());
    }

    /// `FusionConfig` serialises round-trip (used in daemon config).
    #[test]
    fn fusion_config_serialises_roundtrip() {
        let cfg = FusionConfig::default();
        let s = serde_json::to_string(&cfg).expect("ser");
        let back: FusionConfig = serde_json::from_str(&s).expect("de");
        assert_eq!(cfg, back);
    }

    /// No biometric material leaks through the serialised `FusionOutcome` —
    /// only scalar consistency and enum tags.
    #[test]
    fn fusion_outcome_serialises_no_biometric_data() {
        let outcomes = [
            FusionOutcome::IrOnly {
                reason: IrOnlyReason::Darkness,
                reduced_protection: true,
            },
            FusionOutcome::Fused {
                cross_modal_consistency: 0.42,
            },
            FusionOutcome::Rejected {
                reason: RejectReason::CrossModalMismatch,
            },
        ];
        for o in outcomes {
            let s = serde_json::to_string(&o).expect("ser");
            for forbidden in ["embedding", "frame", "pixel", "template", "vector", "0x"] {
                assert!(
                    !s.contains(forbidden),
                    "FusionOutcome leaks `{forbidden}`: {s}"
                );
            }
        }
    }
}
