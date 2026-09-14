// SPDX-License-Identifier: GPL-3.0-only
//! Privacy scan: assert no wire type in `namtor_client::types` carries
//! biometric / matcher internals (embeddings, raw frames, match scores,
//! thresholds, templates, pixels).
//!
//! The scan serializes a representative sample of each public D-Bus type to
//! JSON and walks every field name recursively. If any forbidden substring
//! appears, the test fails and names the leaking field. This is the CI
//! enforcement of spec §9.2 ("no biometric data in the UI").

use namtor_client::{
    EnrollmentProgress, EnrollmentProgressV3, EnrollmentReviewV3, PamServiceCompatibility,
    PamServiceInfo, ProfileInfo, ProtectionKeyKind, ProtectionStatus, QualityLabel, ReviewOutcome,
    StatusReport,
};
use serde::Serialize;

/// Forbidden biometric field-name substrings. A field whose name contains any
/// of these (case-insensitive) would carry biometric material across the D-Bus
/// surface — a spec §9.2 violation.
///
/// Note: "frame" is intentionally absent — `frames_captured`, `frames_seen`,
/// `frames_accepted`, and `frames_rejected` are approved coarse counts, not
/// raw frame data. The forbidden targets are `raw_frame`, `pixel`, `image`,
/// `template`, `embedding`, `score`, `threshold`, `biometric`, `key`, `nonce`.
const FORBIDDEN: &[&str] = &[
    "raw_frame",
    "pixel",
    "image",
    "template",
    "embedding",
    "score",
    "threshold",
    "biometric",
    "key",
    "nonce",
    "vector",
];

/// Recursively walk a JSON value and collect field names that contain any
/// forbidden substring.
fn scan_value(value: &serde_json::Value, hits: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                let lower = key.to_lowercase();
                for forbidden in FORBIDDEN {
                    if lower.contains(forbidden) {
                        hits.push(key.clone());
                    }
                }
                scan_value(val, hits);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                scan_value(item, hits);
            }
        }
        _ => {}
    }
}

/// Serialize a sample of each type and scan its JSON field names for
/// forbidden biometric substrings.
fn forbidden_fields<T: Serialize>(sample: &T) -> Vec<String> {
    let json = serde_json::to_value(sample).expect("serialize sample");
    let mut hits = Vec::new();
    scan_value(&json, &mut hits);
    hits
}

#[test]
fn profile_info_carries_no_biometrics() {
    let sample = ProfileInfo {
        id: "id-1".into(),
        name: "Alice".into(),
        enabled: true,
        created: 1_700_000_000,
        last_used: 1_700_000_100,
    };
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "ProfileInfo leaked fields: {hits:?}");
}

#[test]
fn enrollment_progress_carries_no_biometrics() {
    let sample = EnrollmentProgress {
        session_id: "sess-1".into(),
        frames_captured: 12,
        quality: QualityLabel::High,
        coverage_percent: 80,
    };
    let hits = forbidden_fields(&sample);
    assert!(
        hits.is_empty(),
        "EnrollmentProgress leaked fields: {hits:?} (note: 'frames_captured' is a coarse \
         count, not a frame — the forbidden list targets raw biometric field names)"
    );
}

#[test]
fn review_outcome_carries_no_biometrics() {
    let sample = ReviewOutcome {
        profile_id: "id-1".into(),
        needs_reenroll: false,
        quality: QualityLabel::High,
    };
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "ReviewOutcome leaked fields: {hits:?}");
}

#[test]
fn status_report_carries_no_biometrics() {
    let sample = StatusReport {
        api_version: 2,
        profiles: vec![ProfileInfo {
            id: "id-1".into(),
            name: "Alice".into(),
            enabled: true,
            created: 1_700_000_000,
            last_used: 0,
        }],
        emitter_armed: false,
    };
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "StatusReport leaked fields: {hits:?}");
}

#[test]
fn pam_service_info_carries_no_biometrics() {
    let sample = PamServiceInfo {
        id: "gdm-password".into(),
        display_name: "GDM Password".into(),
        enabled: true,
        compatibility: PamServiceCompatibility::Safe,
        description: "GNOME Display Manager".into(),
    };
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "PamServiceInfo leaked fields: {hits:?}");
}

#[test]
fn protection_status_carries_no_biometrics() {
    let sample = ProtectionStatus {
        sealing: ProtectionKeyKind::Tpm2,
        has_tpm: true,
        warning: String::new(),
    };
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "ProtectionStatus leaked fields: {hits:?}");
}

#[test]
fn enrollment_progress_v3_carries_no_biometrics() {
    let sample = EnrollmentProgressV3 {
        session: "sess-1".into(),
        phase: "capturing".into(),
        quality_percent: 80,
        diversity_percent: 70,
        frames_seen: 12,
        frames_accepted: 10,
        frames_rejected: 2,
        quality_bucket: 2,
        guidance: "looking_good".into(),
    };
    let hits = forbidden_fields(&sample);
    assert!(
        hits.is_empty(),
        "EnrollmentProgressV3 leaked fields: {hits:?} (note: 'frames_seen'/'frames_accepted'/'frames_rejected' are approved coarse counts, not raw frames)"
    );
}

#[test]
fn enrollment_review_v3_carries_no_biometrics() {
    let sample = EnrollmentReviewV3 {
        session: "sess-1".into(),
        ready_to_save: true,
        quality_percent: 85,
        diversity_percent: 75,
        frames_accepted: 10,
        guidance: "looking_good".into(),
    };
    let hits = forbidden_fields(&sample);
    assert!(
        hits.is_empty(),
        "EnrollmentReviewV3 leaked fields: {hits:?} (note: 'frames_accepted' is an approved coarse count)"
    );
}

/// Mutation test: a struct with forbidden field names IS flagged. This
/// guards against the scan becoming a no-op.
#[test]
fn scan_flags_a_known_bad_struct() {
    #[derive(Serialize)]
    struct KnownBad {
        name: String,
        embedding: Vec<f32>,
        raw_frame: Vec<u8>,
        match_score: f32,
    }
    let bad = KnownBad {
        name: "evil".into(),
        embedding: vec![0.0; 4],
        raw_frame: vec![0; 4],
        match_score: 0.99,
    };
    let hits = forbidden_fields(&bad);
    assert!(hits.contains(&"embedding".into()), "embedding flagged");
    assert!(hits.contains(&"raw_frame".into()), "raw_frame flagged");
    assert!(hits.contains(&"match_score".into()), "match_score flagged");
}
