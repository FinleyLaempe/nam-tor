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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
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

/// Derive the live-guidance enum from a capture observation using deterministic
/// precedence (first match wins). The order is fixed:
///
/// 1. `detection_suitability < 20` → [`Guidance::Centering`]
/// 2. `illumination < 30` → [`Guidance::BetterLighting`]
/// 3. `detection_suitability < 50` → [`Guidance::MoveCloser`]
/// 4. `landmark_stability < 25` → [`Guidance::MoveBack`]
/// 5. `blur_estimate < 30` → [`Guidance::HoldStill`]
/// 6. `20 <= pose_frontality < 50` → [`Guidance::FaceFront`]
/// 7. `pose_frontality < 20` → [`Guidance::InsufficientPose`]
/// 8. all key fields (detection, blur, pose, illumination) `>= 70` →
///    [`Guidance::LookingGood`]
/// 9. else → [`Guidance::Capturing`]
pub fn derive_guidance(obs: &CaptureObservation) -> Guidance {
    let d = obs.detection_suitability;
    let blur = obs.blur_estimate;
    let pose = obs.pose_frontality;
    let illum = obs.illumination;
    let landmark = obs.landmark_stability;

    if d < 20 {
        return Guidance::Centering;
    }
    if illum < 30 {
        return Guidance::BetterLighting;
    }
    if d < 50 {
        return Guidance::MoveCloser;
    }
    if landmark < 25 {
        return Guidance::MoveBack;
    }
    if blur < 30 {
        return Guidance::HoldStill;
    }
    if (20..50).contains(&pose) {
        return Guidance::FaceFront;
    }
    if pose < 20 {
        return Guidance::InsufficientPose;
    }
    if d >= 70 && blur >= 70 && pose >= 70 && illum >= 70 {
        return Guidance::LookingGood;
    }
    Guidance::Capturing
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
            embedding_spread: 80,
        };

        let cases: [(CaptureObservation, Guidance); 9] = [
            // 1. Centering: detection < 20.
            (
                CaptureObservation {
                    detection_suitability: 10,
                    ..high_rest
                },
                Guidance::Centering,
            ),
            // 2. BetterLighting: detection >= 20, illumination < 30.
            (
                CaptureObservation {
                    detection_suitability: 80,
                    illumination: 10,
                    ..high_rest
                },
                Guidance::BetterLighting,
            ),
            // 3. MoveCloser: detection in [20,50), all earlier branches clear.
            (
                CaptureObservation {
                    detection_suitability: 30,
                    ..high_rest
                },
                Guidance::MoveCloser,
            ),
            // 4. MoveBack: detection >= 50, landmark < 25.
            (
                CaptureObservation {
                    detection_suitability: 60,
                    landmark_stability: 10,
                    ..high_rest
                },
                Guidance::MoveBack,
            ),
            // 5. HoldStill: detection >= 50, landmark >= 25, blur < 30.
            (
                CaptureObservation {
                    detection_suitability: 60,
                    landmark_stability: 80,
                    blur_estimate: 10,
                    ..high_rest
                },
                Guidance::HoldStill,
            ),
            // 6. FaceFront: earlier branches clear, pose in [20,50).
            (
                CaptureObservation {
                    detection_suitability: 60,
                    landmark_stability: 80,
                    blur_estimate: 80,
                    pose_frontality: 30,
                    ..high_rest
                },
                Guidance::FaceFront,
            ),
            // 7. InsufficientPose: earlier branches clear, pose < 20.
            (
                CaptureObservation {
                    detection_suitability: 60,
                    landmark_stability: 80,
                    blur_estimate: 80,
                    pose_frontality: 10,
                    ..high_rest
                },
                Guidance::InsufficientPose,
            ),
            // 8. LookingGood: all key fields >= 70.
            (high_rest, Guidance::LookingGood),
            // 9. Capturing: falls through every branch; key fields not all >= 70
            //    and pose >= 50 (not in [20,50), not < 20).
            (
                CaptureObservation {
                    detection_suitability: 60,
                    landmark_stability: 80,
                    blur_estimate: 60,
                    pose_frontality: 60,
                    illumination: 60,
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
