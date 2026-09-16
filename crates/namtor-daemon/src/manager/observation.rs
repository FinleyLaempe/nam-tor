// SPDX-License-Identifier: GPL-3.0-only
//! Privacy-safe enrollment observation and live-guidance types (plan task 3).
//!
//! [`CaptureObservation`] is the daemon-internal aggregate of per-frame
//! capture suitability signals (detection confidence, landmark stability,
//! blur, pose frontality, illumination, retained embedding spread). It never
//! crosses the D-Bus surface. The wire-safe projection is [`GuidanceProgress`],
//! which carries only bounded percentages, a coarse [`QualityBucket`], and a
//! closed [`Guidance`] enum — no embeddings, thresholds, match scores,
//! templates, pixels, keys, or nonces.
//!
//! Naming rule (enforced by the `wire_projection_contains_no_forbidden_tokens`
//! test): no field or variant here is named `score`. Pose quality is
//! `pose_frontality`, not `pose_score`. The GNOME privacy scan rejects the
//! substring `score` anywhere in the wire surface.
//!
//! Determinism: [`W_DETECTION`], [`W_BLUR`], [`W_POSE`], [`W_ILLUMINATION`]
//! are the fixed weighting constants; [`QUALITY_LOW_MAX`] and
//! [`QUALITY_MEDIUM_MAX`] are the fixed bucket boundaries. Mapping functions
//! are pure and order-deterministic.

use namtor_recog::FrameMetrics;
use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

// ---------------------------------------------------------------------------
// Deterministic weighting / clamping constants
// ---------------------------------------------------------------------------

/// Weight for the detection-suitability term in the quality aggregate.
pub const W_DETECTION: f32 = 0.30;
/// Weight for the blur (sharpness) term in the quality aggregate.
pub const W_BLUR: f32 = 0.25;
/// Weight for the pose-frontality term in the quality aggregate.
pub const W_POSE: f32 = 0.25;
/// Weight for the illumination term in the quality aggregate.
pub const W_ILLUMINATION: f32 = 0.20;
/// Inclusive upper bound (percent) for the `Low` quality bucket.
pub const QUALITY_LOW_MAX: u32 = 33;
/// Inclusive upper bound (percent) for the `Medium` quality bucket.
pub const QUALITY_MEDIUM_MAX: u32 = 66;

// --- metric → observation mapping constants (task 3.5) ---------------------

/// Yaw magnitude, in degrees, at which pose frontality reaches 0.
pub const POSE_YAW_LIMIT_DEG: f32 = 45.0;
/// Roll magnitude, in degrees, at which pose frontality reaches 0. Tighter
/// than yaw: a rolled face still shows both eyes, but the aligner's
/// similarity transform amplifies roll error faster than yaw error.
pub const POSE_ROLL_LIMIT_DEG: f32 = 30.0;
/// Inter-ocular distance over box width that a well-framed frontal face
/// produces. Used to normalise [`FrameMetrics::landmark_spread`] into the
/// 0-100 `landmark_stability` field.
pub const LANDMARK_SPREAD_IDEAL: f32 = 0.45;
/// Below this [`FrameMetrics::landmark_spread`] the landmarks have collapsed
/// and the geometry they imply is meaningless.
///
/// Review finding F8: coincident landmarks make `yaw_deg` and `roll_deg`
/// report `0.0`, which is byte-for-byte what a perfect frontal pose reports.
/// `landmark_spread` is the ONLY signal that tells the two apart, so pose is
/// gated on it before yaw/roll are read at all.
pub const MIN_LANDMARK_SPREAD: f32 = 0.05;
/// Face-box area over frame area (as a percent) below which the subject is
/// too far from the camera.
pub const FACE_EXTENT_MIN: u8 = 4;
/// Face-box area over frame area (as a percent) above which the subject is
/// too close and the box is clipping.
pub const FACE_EXTENT_MAX: u8 = 45;

// ---------------------------------------------------------------------------
// Internal observation type — daemon-internal, never crosses D-Bus
// ---------------------------------------------------------------------------

/// Daemon-internal aggregate of per-frame capture suitability signals. All
/// fields are 0-100 (u8 callers may exceed 100; mapping functions clamp). This
/// struct is NEVER serialised over the wire — only its bounded projection
/// ([`GuidanceProgress`]) crosses D-Bus.
///
/// Naming: no field is named `score`. Pose quality is `pose_frontality`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureObservation {
    /// 0-100 face detection confidence + size adequacy.
    pub detection_suitability: u8,
    /// 0-100 landmark alignment stability.
    pub landmark_stability: u8,
    /// 0-100 inverse blur (100 = sharp).
    pub blur_estimate: u8,
    /// 0-100 frontal pose quality (NOT "pose_score").
    pub pose_frontality: u8,
    /// 0-100 lighting adequacy.
    pub illumination: u8,
    /// Percent of the frame area covered by the face box — the distance
    /// proxy. Small means "too far away", large means "too close". This is a
    /// raw extent, not a goodness: both ends are bad, so it is read against
    /// [`FACE_EXTENT_MIN`]/[`FACE_EXTENT_MAX`] rather than summed into the
    /// quality aggregate.
    pub face_extent: u8,
    /// 0-100 retained embedding diversity.
    pub embedding_spread: u8,
}

// ---------------------------------------------------------------------------
// Wire-safe types — what crosses D-Bus
// ---------------------------------------------------------------------------

/// Coarse quality bucket exposed over the wire as a single byte (0=low,
/// 1=medium, 2=high). Distinct from `crate::dbus::QualityLabel` (v1) — this is
/// the v3 manager bucket. zvariant's `Type` derive treats a single-field
/// newtype struct as transparent, so the D-Bus signature is `y` (byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct QualityBucket(pub u8);

impl QualityBucket {
    pub const LOW: Self = Self(0);
    pub const MEDIUM: Self = Self(1);
    pub const HIGH: Self = Self(2);

    /// Map a 0-100 percent to a bucket using the frozen boundaries
    /// ([`QUALITY_LOW_MAX`], [`QUALITY_MEDIUM_MAX`]).
    pub fn from_percent(percent: u32) -> Self {
        if percent <= QUALITY_LOW_MAX {
            Self::LOW
        } else if percent <= QUALITY_MEDIUM_MAX {
            Self::MEDIUM
        } else {
            Self::HIGH
        }
    }
}

/// Closed live-guidance enum, serialised as a snake_case string over the wire.
/// No variant name contains a forbidden token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[zvariant(signature = "s", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Guidance {
    Centering,
    MoveCloser,
    MoveBack,
    BetterLighting,
    HoldStill,
    FaceFront,
    LookingGood,
    Capturing,
    InsufficientPose,
}

/// Wire-safe enrollment progress projection. Carries only bounded
/// percentages, a coarse bucket, and a closed guidance enum. No raw
/// observation field is exposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct GuidanceProgress {
    pub frames_seen: u32,
    pub frames_accepted: u32,
    pub frames_rejected: u32,
    pub quality_percent: u32,
    pub diversity_percent: u32,
    pub quality_bucket: QualityBucket,
    pub guidance: Guidance,
}

// ---------------------------------------------------------------------------
// Mapping functions
// ---------------------------------------------------------------------------

/// Clamp a float percent into a `u32` 0-100. NaN, infinity, and negative
/// values collapse to 0; values above 100 collapse to 100; otherwise round to
/// nearest.
pub fn clamp_percent(p: f32) -> u32 {
    if p.is_nan() || p.is_infinite() || p < 0.0 {
        return 0;
    }
    if p > 100.0 {
        return 100;
    }
    p.round() as u32
}

/// Weighted aggregate of the four quality-bearing observation fields
/// (detection, blur, pose, illumination), each clamped to 100, combined with
/// the fixed weights, then clamped to a 0-100 `u32`. `embedding_spread` and
/// `landmark_stability` are not part of the quality aggregate (spread has its
/// own diversity percent; stability feeds guidance only).
pub fn compute_quality_percent(obs: &CaptureObservation) -> u32 {
    let d = obs.detection_suitability.min(100) as f32;
    let b = obs.blur_estimate.min(100) as f32;
    let p = obs.pose_frontality.min(100) as f32;
    let i = obs.illumination.min(100) as f32;
    let weighted = d * W_DETECTION + b * W_BLUR + p * W_POSE + i * W_ILLUMINATION;
    clamp_percent(weighted)
}

/// Project the retained embedding spread into a 0-100 diversity percent. u8
/// values above 100 clamp to 100.
pub fn compute_diversity_percent(embedding_spread: u8) -> u32 {
    embedding_spread.min(100) as u32
}

/// Scale a `0.0..=1.0` ratio into a 0-100 `u8`, clamping hostile inputs.
fn pct(ratio: f32) -> u8 {
    clamp_percent(ratio * 100.0).min(100) as u8
}

/// Build a [`CaptureObservation`] from one frame's real measurements.
///
/// Every observation field is fed by a *different* [`FrameMetrics`] field.
/// The previous implementation copied a single three-valued scalar into four
/// slots, which — because the quality weights sum to 1.0 — made
/// [`compute_quality_percent`] able to return only 20, 50 or 85 and left six
/// of the nine [`Guidance`] variants unreachable.
///
/// Two deliberate mappings:
///
/// * **Pose is gated on geometry (review finding F8).** Coincident landmarks
///   yield `yaw_deg == 0.0` and `roll_deg == 0.0`, which is exactly what a
///   perfect frontal pose yields. [`FrameMetrics::landmark_spread`] is the
///   only signal that separates them, so below [`MIN_LANDMARK_SPREAD`] the
///   pose term is 0 rather than 100 and the frame is reported as
///   [`Guidance::InsufficientPose`]. Without this gate a detection with no
///   usable geometry reads as "LookingGood".
///
/// * **Saturation vetoes illumination (review finding F6).** Mean intensity
///   alone rates a fully blown-out face box 0.23, which is too lenient for
///   enrollment: clipped samples carry no identity information. The
///   illumination term is therefore scaled by `1 - saturated_fraction`, so a
///   2% specular glint is free while a uniformly white box collapses to 0.
///
/// A truncated frame buffer zeroes `blur` and `illumination` while leaving
/// the geometry intact, so `illumination == 0.0` is ambiguous between "pitch
/// black" and "unmeasurable". The ambiguity is collapsed deliberately: both
/// readings mean *do not accept this frame*, and the two are already told
/// apart upstream by distinct counters (`rejected_torn` versus the frame
/// simply scoring badly), so nothing is lost by treating them alike here.
pub fn observation_from(m: &FrameMetrics, diversity: u32) -> CaptureObservation {
    let geometry_usable = m.landmark_spread.is_finite() && m.landmark_spread >= MIN_LANDMARK_SPREAD;
    let pose_frontality = if geometry_usable {
        let yaw_term = 1.0 - (m.yaw_deg.abs() / POSE_YAW_LIMIT_DEG).min(1.0);
        let roll_term = 1.0 - (m.roll_deg.abs() / POSE_ROLL_LIMIT_DEG).min(1.0);
        pct(yaw_term.min(roll_term))
    } else {
        0
    };
    let usable_light = m.illumination * (1.0 - m.saturated_fraction.clamp(0.0, 1.0));
    CaptureObservation {
        detection_suitability: pct(m.detection_confidence),
        landmark_stability: pct(m.landmark_spread / LANDMARK_SPREAD_IDEAL),
        blur_estimate: pct(m.blur),
        pose_frontality,
        illumination: pct(usable_light),
        face_extent: pct(m.face_fraction),
        embedding_spread: diversity.min(100) as u8,
    }
}

/// Derive the live-guidance enum from a capture observation using deterministic
/// precedence (first match wins). The order is fixed:
///
/// 1. `detection_suitability < 20` → [`Guidance::Centering`] — nothing to
///    advise about yet.
/// 2. `landmark_stability` degenerate → [`Guidance::InsufficientPose`] (F8);
///    the geometry is unusable, so no pose/framing advice would be honest.
/// 3. `face_extent < FACE_EXTENT_MIN` → [`Guidance::MoveCloser`]
/// 4. `face_extent > FACE_EXTENT_MAX` → [`Guidance::MoveBack`]
/// 5. `illumination < 30` → [`Guidance::BetterLighting`]
/// 6. `blur_estimate < 30` → [`Guidance::HoldStill`]
/// 7. `pose_frontality < 20` → [`Guidance::InsufficientPose`]
/// 8. `pose_frontality < 50` → [`Guidance::FaceFront`]
/// 9. all key fields (detection, blur, pose, illumination) `>= 70` →
///    [`Guidance::LookingGood`]
/// 10. else → [`Guidance::Capturing`]
///
/// Framing (3, 4) is advised before exposure and sharpness because moving the
/// subject changes both — telling someone to hold still while their face
/// fills 60% of the sensor is advice they cannot act on.
pub fn derive_guidance(obs: &CaptureObservation) -> Guidance {
    let d = obs.detection_suitability;
    let blur = obs.blur_estimate;
    let pose = obs.pose_frontality;
    let illum = obs.illumination;
    let landmark = obs.landmark_stability;
    let extent = obs.face_extent;

    if d < 20 {
        return Guidance::Centering;
    }
    if landmark < degenerate_stability() {
        return Guidance::InsufficientPose;
    }
    if extent < FACE_EXTENT_MIN {
        return Guidance::MoveCloser;
    }
    if extent > FACE_EXTENT_MAX {
        return Guidance::MoveBack;
    }
    if illum < 30 {
        return Guidance::BetterLighting;
    }
    if blur < 30 {
        return Guidance::HoldStill;
    }
    if pose < 20 {
        return Guidance::InsufficientPose;
    }
    if pose < 50 {
        return Guidance::FaceFront;
    }
    if d >= 70 && blur >= 70 && pose >= 70 && illum >= 70 {
        return Guidance::LookingGood;
    }
    Guidance::Capturing
}

/// `landmark_stability` below which the geometry is degenerate — the
/// [`MIN_LANDMARK_SPREAD`] gate expressed in the observation's own 0-100
/// units, so the two can never drift apart.
fn degenerate_stability() -> u8 {
    pct(MIN_LANDMARK_SPREAD / LANDMARK_SPREAD_IDEAL)
}

/// Project a capture observation plus frame counters into the wire-safe
/// [`GuidanceProgress`] struct. This is the only function that produces a
/// value intended to cross D-Bus.
pub fn project(
    obs: &CaptureObservation,
    frames_seen: u32,
    frames_accepted: u32,
    frames_rejected: u32,
) -> GuidanceProgress {
    let quality_percent = compute_quality_percent(obs);
    let diversity_percent = compute_diversity_percent(obs.embedding_spread);
    let quality_bucket = QualityBucket::from_percent(quality_percent);
    let guidance = derive_guidance(obs);
    GuidanceProgress {
        frames_seen,
        frames_accepted,
        frames_rejected,
        quality_percent,
        diversity_percent,
        quality_bucket,
        guidance,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_recog::FrameMetrics;

    /// Subset of [`crate::manager::contract::FORBIDDEN_TOKENS`] that is NOT a
    /// substring of any approved `GuidanceProgress` field name. `frame` and
    /// `vec` are excluded because `frames_seen`/`frames_accepted`/`frames_rejected`
    /// legitimately contain `frame` as a substring (approved Manager3 fields).
    const FORBIDDEN_WIRE_TOKENS: &[&str] = &[
        "embedding",
        "threshold",
        "score",
        "template",
        "pixel",
        "key",
        "nonce",
        "biometric",
    ];

    #[test]
    fn quality_percent_clamps_to_0_100() {
        let zero = CaptureObservation::default();
        assert_eq!(compute_quality_percent(&zero), 0);

        let perfect = CaptureObservation {
            detection_suitability: 100,
            landmark_stability: 100,
            blur_estimate: 100,
            pose_frontality: 100,
            illumination: 100,
            face_extent: 20,
            embedding_spread: 100,
        };
        assert_eq!(compute_quality_percent(&perfect), 100);

        // u8 fields can exceed 100 (up to 255); they must clamp to 100.
        let overflow = CaptureObservation {
            detection_suitability: 255,
            landmark_stability: 255,
            blur_estimate: 255,
            pose_frontality: 255,
            illumination: 255,
            face_extent: 255,
            embedding_spread: 255,
        };
        assert_eq!(compute_quality_percent(&overflow), 100);
    }

    #[test]
    fn quality_percent_weighting_is_deterministic() {
        let obs = CaptureObservation {
            detection_suitability: 80,
            landmark_stability: 90,
            blur_estimate: 70,
            pose_frontality: 60,
            illumination: 50,
            face_extent: 20,
            embedding_spread: 40,
        };
        let a = compute_quality_percent(&obs);
        let b = compute_quality_percent(&obs);
        assert_eq!(a, b);
        // Spot-check the weighted math: 80*0.30 + 70*0.25 + 60*0.25 + 50*0.20
        // = 24.0 + 17.5 + 15.0 + 10.0 = 66.5 → round to 67 (banker's rounding
        // may give 66; assert it lands in [66,67]).
        assert!((66..=67).contains(&a), "weighted quality was {a}");
    }

    #[test]
    fn quality_bucket_boundaries() {
        assert_eq!(QualityBucket::from_percent(0), QualityBucket::LOW);
        assert_eq!(QualityBucket::from_percent(33), QualityBucket::LOW);
        assert_eq!(QualityBucket::from_percent(34), QualityBucket::MEDIUM);
        assert_eq!(QualityBucket::from_percent(66), QualityBucket::MEDIUM);
        assert_eq!(QualityBucket::from_percent(67), QualityBucket::HIGH);
        assert_eq!(QualityBucket::from_percent(100), QualityBucket::HIGH);
    }

    #[test]
    fn guidance_precedence_centering_beats_lighting() {
        // detection=10 triggers Centering even though illumination=10 would
        // otherwise trigger BetterLighting. Centering is higher precedence.
        let obs = CaptureObservation {
            detection_suitability: 10,
            illumination: 10,
            ..Default::default()
        };
        assert_eq!(derive_guidance(&obs), Guidance::Centering);
    }

    #[test]
    fn guidance_precedence_order() {
        // Each row constructs an observation that triggers exactly one variant.
        // "All else high" means the other fields are set to values that do not
        // trigger an earlier-precedence branch.
        let high_rest = CaptureObservation {
            detection_suitability: 80,
            landmark_stability: 80,
            blur_estimate: 80,
            pose_frontality: 80,
            illumination: 80,
            face_extent: 20,
            embedding_spread: 80,
        };

        let cases: [(CaptureObservation, Guidance); 10] = [
            // 1. Centering: detection < 20.
            (
                CaptureObservation {
                    detection_suitability: 10,
                    ..high_rest
                },
                Guidance::Centering,
            ),
            // 2. InsufficientPose (F8): the landmarks collapsed, so no pose
            //    or framing advice would be honest.
            (
                CaptureObservation {
                    landmark_stability: 2,
                    ..high_rest
                },
                Guidance::InsufficientPose,
            ),
            // 3. MoveCloser: the face covers too little of the frame.
            (
                CaptureObservation {
                    face_extent: 2,
                    ..high_rest
                },
                Guidance::MoveCloser,
            ),
            // 4. MoveBack: the face covers too much of the frame.
            (
                CaptureObservation {
                    face_extent: 60,
                    ..high_rest
                },
                Guidance::MoveBack,
            ),
            // 5. BetterLighting: framing is fine, exposure is not.
            (
                CaptureObservation {
                    illumination: 10,
                    ..high_rest
                },
                Guidance::BetterLighting,
            ),
            // 6. HoldStill: exposure fine, frame smeared.
            (
                CaptureObservation {
                    blur_estimate: 10,
                    ..high_rest
                },
                Guidance::HoldStill,
            ),
            // 7. InsufficientPose (measured): the head is turned too far to
            //    be recoverable by asking for a small correction.
            (
                CaptureObservation {
                    pose_frontality: 10,
                    ..high_rest
                },
                Guidance::InsufficientPose,
            ),
            // 8. FaceFront: a recoverable turn.
            (
                CaptureObservation {
                    pose_frontality: 30,
                    ..high_rest
                },
                Guidance::FaceFront,
            ),
            // 9. LookingGood: all key fields >= 70.
            (high_rest, Guidance::LookingGood),
            // 10. Capturing: falls through every branch; key fields not all
            //     >= 70 and pose >= 50.
            (
                CaptureObservation {
                    detection_suitability: 60,
                    landmark_stability: 80,
                    blur_estimate: 60,
                    pose_frontality: 60,
                    illumination: 60,
                    face_extent: 20,
                    embedding_spread: 60,
                },
                Guidance::Capturing,
            ),
        ];

        for (obs, expected) in cases {
            assert_eq!(
                derive_guidance(&obs),
                expected,
                "guidance mismatch for observation {obs:?}"
            );
        }
    }

    /// The F8 gate is expressed in the observation's own units and must track
    /// [`MIN_LANDMARK_SPREAD`] exactly — a drift between the two would let a
    /// degenerate detection through the ladder.
    #[test]
    fn degenerate_stability_tracks_the_spread_gate() {
        let at_gate = observation_from(
            &FrameMetrics {
                landmark_spread: MIN_LANDMARK_SPREAD,
                ..good_metrics()
            },
            0,
        );
        assert!(at_gate.landmark_stability >= degenerate_stability());
        assert_ne!(derive_guidance(&at_gate), Guidance::InsufficientPose);
    }

    #[test]
    fn empty_observations_yield_default() {
        let obs = CaptureObservation::default();
        let prog = project(&obs, 0, 0, 0);
        assert_eq!(prog.quality_percent, 0);
        assert_eq!(prog.quality_bucket, QualityBucket::LOW);
        assert_eq!(prog.guidance, Guidance::Centering);
        assert_eq!(prog.diversity_percent, 0);
    }

    #[test]
    fn invalid_observations_clamp_safely() {
        assert_eq!(clamp_percent(f32::NAN), 0);
        assert_eq!(clamp_percent(f32::INFINITY), 0);
        assert_eq!(clamp_percent(f32::NEG_INFINITY), 0);
        assert_eq!(clamp_percent(-5.0), 0);
        assert_eq!(clamp_percent(150.0), 100);
        assert_eq!(clamp_percent(42.4), 42);
        assert_eq!(clamp_percent(42.6), 43);

        let overflow = CaptureObservation {
            detection_suitability: 255,
            landmark_stability: 255,
            blur_estimate: 255,
            pose_frontality: 255,
            illumination: 255,
            face_extent: 255,
            embedding_spread: 255,
        };
        assert_eq!(compute_quality_percent(&overflow), 100);
    }

    #[test]
    fn diversity_percent_clamps_to_0_100() {
        assert_eq!(compute_diversity_percent(0), 0);
        assert_eq!(compute_diversity_percent(50), 50);
        assert_eq!(compute_diversity_percent(100), 100);
        assert_eq!(compute_diversity_percent(255), 100);
    }

    #[test]
    fn wire_projection_contains_no_forbidden_tokens() {
        let obs = CaptureObservation {
            detection_suitability: 75,
            landmark_stability: 80,
            blur_estimate: 70,
            pose_frontality: 72,
            illumination: 68,
            face_extent: 22,
            embedding_spread: 55,
        };
        let prog = project(&obs, 120, 90, 30);
        let json = serde_json::to_string(&prog).unwrap();
        let lower = json.to_lowercase();
        for token in FORBIDDEN_WIRE_TOKENS {
            assert!(
                !lower.contains(token),
                "wire projection JSON `{json}` contains forbidden token `{token}`"
            );
        }
    }

    #[test]
    fn guidance_enum_serializes_snake_case() {
        let cases = [
            (Guidance::Centering, "centering"),
            (Guidance::MoveCloser, "move_closer"),
            (Guidance::MoveBack, "move_back"),
            (Guidance::BetterLighting, "better_lighting"),
            (Guidance::HoldStill, "hold_still"),
            (Guidance::FaceFront, "face_front"),
            (Guidance::LookingGood, "looking_good"),
            (Guidance::Capturing, "capturing"),
            (Guidance::InsufficientPose, "insufficient_pose"),
        ];
        for (val, expected) in cases {
            let json = serde_json::to_string(&val).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
        }
    }

    #[test]
    fn guidance_enum_round_trips() {
        let all = [
            Guidance::Centering,
            Guidance::MoveCloser,
            Guidance::MoveBack,
            Guidance::BetterLighting,
            Guidance::HoldStill,
            Guidance::FaceFront,
            Guidance::LookingGood,
            Guidance::Capturing,
            Guidance::InsufficientPose,
        ];
        for val in all {
            let json = serde_json::to_string(&val).unwrap();
            let back: Guidance = serde_json::from_str(&json).unwrap();
            assert_eq!(back, val);
        }
    }

    #[test]
    fn project_combines_all_fields() {
        let obs = CaptureObservation {
            detection_suitability: 90,
            landmark_stability: 90,
            blur_estimate: 90,
            pose_frontality: 90,
            illumination: 90,
            face_extent: 20,
            embedding_spread: 70,
        };
        let prog = project(&obs, 100, 80, 20);
        assert_eq!(prog.frames_seen, 100);
        assert_eq!(prog.frames_accepted, 80);
        assert_eq!(prog.frames_rejected, 20);
        assert_eq!(prog.quality_percent, 90);
        assert_eq!(prog.diversity_percent, 70);
        assert_eq!(prog.quality_bucket, QualityBucket::HIGH);
        assert_eq!(prog.guidance, Guidance::LookingGood);
    }

    // -----------------------------------------------------------------
    // Task 3.5: the observation is built from real, independent metrics
    // -----------------------------------------------------------------

    /// A frame that is good on every axis. Individual tests spoil one field.
    fn good_metrics() -> FrameMetrics {
        FrameMetrics {
            detection_confidence: 0.95,
            blur: 0.9,
            illumination: 1.0,
            yaw_deg: 2.0,
            roll_deg: 2.0,
            face_fraction: 0.20,
            landmark_spread: 0.40,
            saturated_fraction: 0.0,
        }
    }

    /// One representative frame per guidance branch. The arbiter for
    /// [`all_nine_guidance_variants_are_reachable`].
    fn representative_metric_sweep() -> Vec<FrameMetrics> {
        vec![
            // Centering: the detector is barely holding on to anything.
            FrameMetrics {
                detection_confidence: 0.10,
                ..good_metrics()
            },
            // InsufficientPose (degenerate geometry): landmarks collapsed.
            FrameMetrics {
                landmark_spread: 0.0,
                ..good_metrics()
            },
            // MoveCloser: the face covers 2% of the frame.
            FrameMetrics {
                face_fraction: 0.02,
                ..good_metrics()
            },
            // MoveBack: the face covers 60% of the frame.
            FrameMetrics {
                face_fraction: 0.60,
                ..good_metrics()
            },
            // BetterLighting.
            FrameMetrics {
                illumination: 0.05,
                ..good_metrics()
            },
            // HoldStill.
            FrameMetrics {
                blur: 0.10,
                ..good_metrics()
            },
            // InsufficientPose (measured): yaw at the limit.
            FrameMetrics {
                yaw_deg: 44.0,
                ..good_metrics()
            },
            // FaceFront: a recoverable turn.
            FrameMetrics {
                yaw_deg: 25.0,
                ..good_metrics()
            },
            // LookingGood.
            good_metrics(),
            // Capturing: usable everywhere, excellent nowhere.
            FrameMetrics {
                detection_confidence: 0.60,
                blur: 0.50,
                illumination: 0.60,
                yaw_deg: 10.0,
                roll_deg: 5.0,
                face_fraction: 0.20,
                landmark_spread: 0.40,
                saturated_fraction: 0.0,
            },
        ]
    }

    #[test]
    fn observation_fields_are_independent_not_one_scalar_copied() {
        // A sharp but badly-lit frame must score high on blur and low on
        // illumination — the fabricated version made them identical.
        let m = FrameMetrics {
            blur: 0.9,
            illumination: 0.1,
            ..good_metrics()
        };
        let obs = observation_from(&m, 50);
        assert!(obs.blur_estimate > 80, "blur: {}", obs.blur_estimate);
        assert!(obs.illumination < 20, "illumination: {}", obs.illumination);
        assert_ne!(obs.blur_estimate, obs.illumination);
    }

    #[test]
    fn observation_carries_the_measured_diversity() {
        let obs = observation_from(&good_metrics(), 37);
        assert_eq!(obs.embedding_spread, 37);
        assert_eq!(
            observation_from(&good_metrics(), 4000).embedding_spread,
            100
        );
    }

    #[test]
    fn dark_frame_yields_better_lighting_guidance() {
        let m = FrameMetrics {
            illumination: 0.05,
            ..good_metrics()
        };
        assert_eq!(
            derive_guidance(&observation_from(&m, 50)),
            Guidance::BetterLighting
        );
    }

    #[test]
    fn small_face_yields_move_closer_guidance() {
        let m = FrameMetrics {
            face_fraction: 0.02,
            ..good_metrics()
        };
        assert_eq!(
            derive_guidance(&observation_from(&m, 50)),
            Guidance::MoveCloser
        );
    }

    #[test]
    fn large_face_yields_move_back_guidance() {
        let m = FrameMetrics {
            face_fraction: 0.60,
            ..good_metrics()
        };
        assert_eq!(
            derive_guidance(&observation_from(&m, 50)),
            Guidance::MoveBack
        );
    }

    #[test]
    fn turned_head_yields_face_front_guidance() {
        let m = FrameMetrics {
            yaw_deg: 35.0,
            ..good_metrics()
        };
        assert_eq!(
            derive_guidance(&observation_from(&m, 50)),
            Guidance::FaceFront
        );
    }

    /// Review finding F8. Coincident landmarks make `yaw_deg`/`roll_deg`
    /// read `0.0`, which is indistinguishable from a perfect frontal pose.
    /// `landmark_spread == 0.0` is the ONLY signal that they are meaningless,
    /// so the mapping must not trust them.
    #[test]
    fn degenerate_landmarks_are_never_scored_as_a_frontal_pose() {
        let m = FrameMetrics {
            landmark_spread: 0.0,
            yaw_deg: 0.0,
            roll_deg: 0.0,
            ..good_metrics()
        };
        let obs = observation_from(&m, 50);
        assert_eq!(
            obs.pose_frontality, 0,
            "yaw/roll trusted without geometry: {obs:?}"
        );
        assert_ne!(derive_guidance(&obs), Guidance::LookingGood);
        assert_eq!(derive_guidance(&obs), Guidance::InsufficientPose);
    }

    /// The same gate, one notch above zero: a spread far below what a real
    /// face produces is still degenerate.
    #[test]
    fn near_coincident_landmarks_are_also_gated() {
        let m = FrameMetrics {
            landmark_spread: 0.01,
            ..good_metrics()
        };
        let obs = observation_from(&m, 50);
        assert_eq!(obs.pose_frontality, 0);
        assert_eq!(derive_guidance(&obs), Guidance::InsufficientPose);
    }

    /// F8, second half: a truncated buffer zeroes the pixel-derived metrics
    /// while the geometry survives. Whether that means "pitch black" or
    /// "unmeasurable" the frame is unusable, so it must never read as good.
    #[test]
    fn unmeasurable_pixels_never_read_as_looking_good() {
        let m = FrameMetrics {
            blur: 0.0,
            illumination: 0.0,
            ..good_metrics()
        };
        let obs = observation_from(&m, 50);
        let g = derive_guidance(&obs);
        assert_ne!(g, Guidance::LookingGood);
        assert_eq!(g, Guidance::BetterLighting, "obs {obs:?}");
    }

    /// Review finding F6. A blown-out face box scores illumination 0.23 on
    /// mean intensity alone; the saturated-sample fraction must veto it.
    #[test]
    fn blown_out_frame_is_penalised_by_the_saturation_term() {
        let m = FrameMetrics {
            illumination: 0.23,
            saturated_fraction: 1.0,
            ..good_metrics()
        };
        let obs = observation_from(&m, 50);
        assert_eq!(obs.illumination, 0, "obs {obs:?}");
        assert_eq!(derive_guidance(&obs), Guidance::BetterLighting);
    }

    /// A specular glint on an otherwise well-exposed frame must not be
    /// treated as a blown-out frame.
    #[test]
    fn a_specular_highlight_does_not_veto_a_good_frame() {
        let m = FrameMetrics {
            saturated_fraction: 0.02,
            ..good_metrics()
        };
        let obs = observation_from(&m, 50);
        assert!(obs.illumination >= 95, "obs {obs:?}");
        assert_eq!(derive_guidance(&obs), Guidance::LookingGood);
    }

    #[test]
    fn all_nine_guidance_variants_are_reachable() {
        let mut seen = std::collections::HashSet::new();
        for m in representative_metric_sweep() {
            seen.insert(derive_guidance(&observation_from(&m, 50)));
        }
        assert_eq!(seen.len(), 9, "unreachable variants; saw {seen:?}");
    }

    #[test]
    fn quality_percent_is_not_limited_to_three_values() {
        let mut values = std::collections::HashSet::new();
        for m in representative_metric_sweep() {
            values.insert(compute_quality_percent(&observation_from(&m, 50)));
        }
        assert!(
            values.len() >= 6,
            "quality collapsed to {} distinct values: {values:?}",
            values.len()
        );
    }

    #[test]
    fn quality_bucket_dbus_signature_is_byte() {
        // QualityBucket is a transparent newtype over u8 → D-Bus signature "y".
        assert_eq!(QualityBucket::SIGNATURE, "y");
    }

    #[test]
    fn guidance_dbus_signature_is_string() {
        assert_eq!(Guidance::SIGNATURE, "s");
    }
}
