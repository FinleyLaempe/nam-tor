// SPDX-License-Identifier: GPL-3.0-only
//! Privacy scan: assert no UI-facing struct carries biometric / matcher
//! internals (embeddings, frames, scores, thresholds, landmarks, raw data).
//!
//! The scan serializes a representative sample of each public D-Bus type to
//! JSON and walks every field name recursively. If any forbidden substring
//! appears, the test fails and names the leaking field. This is the CI
//! enforcement of spec §9.2 ("no biometric data in the UI").

use namtor_gnome::privacy::{forbidden_fields, FORBIDDEN_SUBSTRINGS};
use namtor_gnome::{EnrollmentProgress, FinishResult, ProfileInfo, QualityLabel, StatusInfo};

#[test]
fn profile_info_carries_no_biometrics() {
    let sample = ProfileInfo {
        id: "id-1".into(),
        name: "Alice".into(),
        enabled: true,
        created: 1_700_000_000,
        last_used: Some(1_700_000_100),
    };
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "ProfileInfo leaked fields: {hits:?}");
}

#[test]
fn enrollment_progress_carries_no_biometrics() {
    let sample = EnrollmentProgress {
        session_id: "sess-1".into(),
        frames_captured: 12,
        quality_label: QualityLabel::High,
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
fn finish_result_carries_no_biometrics() {
    let sample = FinishResult {
        success: true,
        profile_id: Some("id-1".into()),
        reason: None,
    };
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "FinishResult leaked fields: {hits:?}");
}

#[test]
fn status_info_carries_no_biometrics() {
    let sample = StatusInfo {
        daemon_reachable: true,
        camera_present: true,
        active_profile: Some("Alice".into()),
        version: Some("0.1.0".into()),
        always_on_emitter: false,
    };
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "StatusInfo leaked fields: {hits:?}");
}

#[test]
fn forbidden_substrings_list_is_nonempty() {
    assert!(
        !FORBIDDEN_SUBSTRINGS.is_empty(),
        "scan list must not be vacuous"
    );
    // Sanity: the list must include the obvious biometric terms.
    assert!(FORBIDDEN_SUBSTRINGS.contains(&"embedding"));
    assert!(FORBIDDEN_SUBSTRINGS.contains(&"threshold"));
}

/// A negative control: a struct that *does* carry an embedding must be
/// flagged. This guards against the scan becoming a no-op.
#[test]
fn scan_flags_a_known_bad_struct() {
    use serde::Serialize;

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
