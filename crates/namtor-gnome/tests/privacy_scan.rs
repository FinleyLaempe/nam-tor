// SPDX-License-Identifier: GPL-3.0-only
//! Privacy scan: assert no UI-facing struct carries biometric / matcher
//! internals (embeddings, frames, scores, thresholds, landmarks, raw data).
//!
//! The scan serializes a representative sample of each public D-Bus type to
//! JSON and walks every field name recursively. If any forbidden substring
//! appears, the test fails and names the leaking field. This is the CI
//! enforcement of spec §9.2 ("no biometric data in the UI").

use namtor_gnome::privacy::{forbidden_fields, FORBIDDEN_SUBSTRINGS};
use namtor_gnome::{
    client::{PamServiceCompatibility, PamServiceInfo, ProtectionKeyKind, ProtectionStatus},
    EnrollmentProgress, ProfileInfo, QualityLabel, ReviewOutcome, StatusReport,
};

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
fn presenter_model_carries_no_biometrics() {
    use namtor_gnome::view_model::PresenterModel;
    let sample = PresenterModel::default();
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "PresenterModel leaked fields: {hits:?}");
}

#[test]
fn forbidden_substrings_list_is_nonempty() {
    assert!(
        !FORBIDDEN_SUBSTRINGS.is_empty(),
        "scan list must not be vacuous"
    );
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

/// Mutation test: for every forbidden substring, inject it as a field name
/// into a struct shaped like `ProtectionView` and `ReadinessView` and assert
/// the scan catches it. This proves that adding any forbidden field to the
/// view model types would fail the privacy scan.
#[test]
fn mutation_test_forbidden_fields_in_protection_view() {
    let clean = namtor_gnome::view_model::ProtectionView {
        label: "TPM2 (hardware-bound)".into(),
        warning: String::new(),
        is_software_fallback: false,
        emitter_reduced: false,
    };
    assert!(
        forbidden_fields(&clean).is_empty(),
        "clean ProtectionView must pass scan"
    );

    for forbidden in FORBIDDEN_SUBSTRINGS {
        let key = (*forbidden).to_string();
        let json = serde_json::json!({
            "label": "TPM2 (hardware-bound)",
            "warning": "",
            "is_software_fallback": false,
            "emitter_reduced": false,
            key: "injected",
        });
        let mut hits = Vec::new();
        namtor_gnome::privacy::scan_value(&json, &mut hits);
        assert!(
            hits.iter()
                .any(|h: &String| h.to_lowercase().contains(forbidden)),
            "forbidden field '{forbidden}' was NOT flagged by the scan"
        );
    }
}

#[test]
fn mutation_test_forbidden_fields_in_readiness_view() {
    let clean = namtor_gnome::view_model::ReadinessView {
        daemon: namtor_gnome::view_model::ReadinessLevel::Ready,
        model: namtor_gnome::view_model::ReadinessLevel::Ready,
        camera: namtor_gnome::view_model::ReadinessLevel::Ready,
        has_enrolled_profiles: true,
        enabled_pam_count: 2,
    };
    assert!(
        forbidden_fields(&clean).is_empty(),
        "clean ReadinessView must pass scan"
    );

    for forbidden in FORBIDDEN_SUBSTRINGS {
        let key = (*forbidden).to_string();
        let json = serde_json::json!({
            "daemon": "ready",
            "model": "ready",
            "camera": "ready",
            "has_enrolled_profiles": true,
            "enabled_pam_count": 2,
            key: "injected",
        });
        let mut hits = Vec::new();
        namtor_gnome::privacy::scan_value(&json, &mut hits);
        assert!(
            hits.iter()
                .any(|h: &String| h.to_lowercase().contains(forbidden)),
            "forbidden field '{forbidden}' was NOT flagged by the scan"
        );
    }
}

/// The full `PresenterModel` (with all nested views populated) must pass the
/// privacy scan. This is the integration-level assertion that the complete
/// view model — including the new readiness and protection fields — carries
/// no biometric internals.
#[test]
fn populated_presenter_model_carries_no_biometrics() {
    use namtor_gnome::view_model::{
        EnrollmentView, ErrorView, PendingMerge, PendingPamToggle, PendingReplace, PresenterModel,
        ProtectionView, QualityView, ReadinessLevel, ReadinessView, ReviewView,
    };

    let model = PresenterModel {
        profiles: vec![namtor_gnome::view_model::ProfileRowView {
            id: "id-1".into(),
            name: "Alice".into(),
            enabled: true,
            created: 1_700_000_000,
            last_used: 0,
        }],
        status: Some(namtor_gnome::StatusReport {
            api_version: 2,
            profiles: vec![],
            emitter_armed: false,
        }),
        protection: Some(ProtectionView {
            label: "TPM2 (hardware-bound)".into(),
            warning: String::new(),
            is_software_fallback: false,
            emitter_reduced: false,
        }),
        readiness: Some(ReadinessView {
            daemon: ReadinessLevel::Ready,
            model: ReadinessLevel::Ready,
            camera: ReadinessLevel::Ready,
            has_enrolled_profiles: true,
            enabled_pam_count: 1,
        }),
        pam_services: vec![],
        enrollment: Some(EnrollmentView {
            session_id: "sess-1".into(),
            frames_captured: 10,
            quality: QualityView {
                label: "high".into(),
                show_guidance: false,
            },
            coverage_percent: 80,
        }),
        review: Some(ReviewView {
            profile_id: "id-1".into(),
            needs_reenroll: false,
            quality_label: "high".into(),
            coverage_percent: 80,
            frames_captured: 10,
        }),
        pending_merge: Some(PendingMerge {
            src_id: "id-a".into(),
            src_name: "A".into(),
            dst_id: "id-b".into(),
            dst_name: "B".into(),
        }),
        pending_replace: Some(PendingReplace {
            id: "id-1".into(),
            old_name: "Old".into(),
            new_name: "New".into(),
        }),
        pending_pam_toggle: Some(PendingPamToggle {
            id: "sddm".into(),
            display_name: "SDDM".into(),
            enabled: true,
            high_impact: true,
        }),
        error: Some(ErrorView {
            title: "Test".into(),
            detail: "detail".into(),
            recovery: None,
        }),
        message: Some("ok".into()),
        is_loading: false,
        emitter_armed_warning: false,
    };
    let hits = forbidden_fields(&model);
    assert!(hits.is_empty(), "populated PresenterModel leaked: {hits:?}");
}
