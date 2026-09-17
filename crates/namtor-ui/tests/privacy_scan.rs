// SPDX-License-Identifier: GPL-3.0-only
//! Privacy scan: assert no UI-facing struct carries biometric / matcher
//! internals (embeddings, frames, scores, thresholds, landmarks, raw data).
//!
//! The scan serializes a representative sample of each public D-Bus type to
//! JSON and walks every field name recursively. If any forbidden substring
//! appears, the test fails and names the leaking field. This is the CI
//! enforcement of spec §9.2 ("no biometric data in the UI").

use namtor_ui::privacy::{forbidden_fields, FORBIDDEN_SUBSTRINGS};
use namtor_ui::{
    client::{PamServiceCompatibility, PamServiceInfo, ProtectionKeyKind, ProtectionStatus},
    EnrollmentProgress, EnrollmentProgressV3, EnrollmentReviewV3, ProfileInfo, QualityLabel,
    ReviewOutcome, StatusReport,
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

#[test]
fn enrollment_view_v3_carries_no_biometrics() {
    use namtor_ui::view_model::EnrollmentViewV3;
    let sample = EnrollmentViewV3 {
        phase: "capturing".into(),
        quality_percent: 80,
        diversity_percent: 70,
        frames_seen: 12,
        frames_accepted: 10,
        frames_rejected: 2,
        quality_bucket: 2,
        guidance: "Looking good — keep going".into(),
    };
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "EnrollmentViewV3 leaked fields: {hits:?}");
}

#[test]
fn review_view_v3_carries_no_biometrics() {
    use namtor_ui::view_model::ReviewViewV3;
    let sample = ReviewViewV3 {
        ready_to_save: true,
        quality_percent: 85,
        diversity_percent: 75,
        frames_accepted: 10,
        guidance: "Looking good — keep going".into(),
    };
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "ReviewViewV3 leaked fields: {hits:?}");
}

#[test]
fn enrollment_state_carries_no_biometrics() {
    use namtor_ui::view_model::EnrollmentState;
    let sample = EnrollmentState::Error("something went wrong".into());
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "EnrollmentState leaked fields: {hits:?}");
}

#[test]
fn presenter_model_carries_no_biometrics() {
    use namtor_ui::view_model::PresenterModel;
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
    assert!(FORBIDDEN_SUBSTRINGS.contains(&"score"));
    assert!(FORBIDDEN_SUBSTRINGS.contains(&"template"));
    assert!(FORBIDDEN_SUBSTRINGS.contains(&"biometric"));
    assert!(FORBIDDEN_SUBSTRINGS.contains(&"key"));
    assert!(FORBIDDEN_SUBSTRINGS.contains(&"nonce"));
    assert!(FORBIDDEN_SUBSTRINGS.contains(&"pixel"));
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
    let clean = namtor_ui::view_model::ProtectionView {
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
        namtor_ui::privacy::scan_value(&json, &mut hits);
        assert!(
            hits.iter()
                .any(|h: &String| h.to_lowercase().contains(forbidden)),
            "forbidden field '{forbidden}' was NOT flagged by the scan"
        );
    }
}

#[test]
fn mutation_test_forbidden_fields_in_readiness_view() {
    let clean = namtor_ui::view_model::ReadinessView {
        daemon: namtor_ui::view_model::ReadinessLevel::Ready,
        model: namtor_ui::view_model::ReadinessLevel::Ready,
        camera: namtor_ui::view_model::ReadinessLevel::Ready,
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
        namtor_ui::privacy::scan_value(&json, &mut hits);
        assert!(
            hits.iter()
                .any(|h: &String| h.to_lowercase().contains(forbidden)),
            "forbidden field '{forbidden}' was NOT flagged by the scan"
        );
    }
}

#[test]
fn mutation_test_forbidden_fields_in_enrollment_view_v3() {
    let clean = namtor_ui::view_model::EnrollmentViewV3 {
        phase: "capturing".into(),
        quality_percent: 80,
        diversity_percent: 70,
        frames_seen: 12,
        frames_accepted: 10,
        frames_rejected: 2,
        quality_bucket: 2,
        guidance: "Looking good".into(),
    };
    assert!(
        forbidden_fields(&clean).is_empty(),
        "clean EnrollmentViewV3 must pass scan"
    );

    for forbidden in FORBIDDEN_SUBSTRINGS {
        let key = (*forbidden).to_string();
        let json = serde_json::json!({
            "phase": "capturing",
            "quality_percent": 80,
            "diversity_percent": 70,
            "frames_seen": 12,
            "frames_accepted": 10,
            "frames_rejected": 2,
            "quality_bucket": 2,
            "guidance": "Looking good",
            key: "injected",
        });
        let mut hits = Vec::new();
        namtor_ui::privacy::scan_value(&json, &mut hits);
        assert!(
            hits.iter()
                .any(|h: &String| h.to_lowercase().contains(forbidden)),
            "forbidden field '{forbidden}' was NOT flagged by the scan"
        );
    }
}

#[test]
fn mutation_test_forbidden_fields_in_review_view_v3() {
    let clean = namtor_ui::view_model::ReviewViewV3 {
        ready_to_save: true,
        quality_percent: 85,
        diversity_percent: 75,
        frames_accepted: 10,
        guidance: "Looking good".into(),
    };
    assert!(
        forbidden_fields(&clean).is_empty(),
        "clean ReviewViewV3 must pass scan"
    );

    for forbidden in FORBIDDEN_SUBSTRINGS {
        let key = (*forbidden).to_string();
        let json = serde_json::json!({
            "ready_to_save": true,
            "quality_percent": 85,
            "diversity_percent": 75,
            "frames_accepted": 10,
            "guidance": "Looking good",
            key: "injected",
        });
        let mut hits = Vec::new();
        namtor_ui::privacy::scan_value(&json, &mut hits);
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
    use namtor_ui::view_model::{
        AuthenticationView, EnrollmentState, EnrollmentView, EnrollmentViewV3, ErrorView,
        PendingMerge, PendingPamToggle, PendingReplace, PresenterModel, ProtectionView,
        QualityView, ReadinessLevel, ReadinessView, ReviewView, ReviewViewV3, SystemView,
    };

    let model = PresenterModel {
        profiles: vec![namtor_ui::view_model::ProfileRowView {
            id: "id-1".into(),
            name: "Alice".into(),
            enabled: true,
            created: 1_700_000_000,
            last_used: 0,
        }],
        status: Some(namtor_ui::StatusReport {
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
        authentication: Some(AuthenticationView::default()),
        system: Some(SystemView::default()),
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
        enrollment_state: EnrollmentState::Capturing,
        enrollment_session_v3: Some("sess-v3-1".into()),
        enrollment_view_v3: Some(EnrollmentViewV3 {
            phase: "capturing".into(),
            quality_percent: 80,
            diversity_percent: 70,
            frames_seen: 12,
            frames_accepted: 10,
            frames_rejected: 2,
            quality_bucket: 2,
            guidance: "Looking good — keep going".into(),
        }),
        review_view_v3: Some(ReviewViewV3 {
            ready_to_save: true,
            quality_percent: 85,
            diversity_percent: 75,
            frames_accepted: 10,
            guidance: "Looking good — keep going".into(),
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

// ===========================================================================
// Task 13: accessibility / privacy scan coverage for new view types and
// user-facing strings. The tests below assert no biometric / internal
// wording leaks into UI strings and that the new view types
// (AuthenticationView, SystemView, ProfileSetupView) are scanned.
// ===========================================================================

use namtor_ui::privacy::forbidden_words_in_ui_string;
use namtor_ui::view_model::{AuthenticationView, ProfileSetupView};

/// `AuthenticationView` carries no biometric field names.
#[test]
fn authentication_view_carries_no_biometrics() {
    let sample = AuthenticationView {
        recommended_service: Some(namtor_ui::view_model::PamServiceRowView {
            id: "gdm-password".into(),
            display_name: "GDM Password".into(),
            enabled: false,
            compatibility: PamServiceCompatibility::Safe,
            description: "GNOME Display Manager".into(),
        }),
        safe_services: vec![],
        risky_services: vec![],
        unsupported_services: vec![],
    };
    let hits = forbidden_fields(&sample);
    assert!(
        hits.is_empty(),
        "AuthenticationView leaked fields: {hits:?}"
    );
}

/// `SystemView` carries no biometric field names.
#[test]
fn system_view_carries_no_biometrics() {
    use namtor_ui::view_model::{ProtectionView, SystemView};
    let sample = SystemView {
        daemon_running: true,
        camera_detected: true,
        models_loaded: true,
        protection: ProtectionView {
            label: "TPM2 (hardware-bound)".into(),
            warning: String::new(),
            is_software_fallback: false,
            emitter_reduced: false,
        },
    };
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "SystemView leaked fields: {hits:?}");
}

/// `ProfileSetupView` carries no biometric field names.
#[test]
fn profile_setup_view_carries_no_biometrics() {
    let sample = ProfileSetupView {
        name_placeholder: "e.g. Home Office".into(),
        privacy_acknowledged: false,
    };
    let hits = forbidden_fields(&sample);
    assert!(hits.is_empty(), "ProfileSetupView leaked fields: {hits:?}");
}

/// Mutation test: every forbidden substring injected into a
/// `ProfileSetupView`-shaped JSON object is caught by the scan. This proves
/// adding any forbidden field to the new setup view would fail CI.
#[test]
fn mutation_test_forbidden_fields_in_profile_setup_view() {
    let clean = ProfileSetupView {
        name_placeholder: "e.g. Home Office".into(),
        privacy_acknowledged: false,
    };
    assert!(
        forbidden_fields(&clean).is_empty(),
        "clean ProfileSetupView must pass scan"
    );

    for forbidden in FORBIDDEN_SUBSTRINGS {
        let key = (*forbidden).to_string();
        let json = serde_json::json!({
            "name_placeholder": "e.g. Home Office",
            "privacy_acknowledged": false,
            key: "injected",
        });
        let mut hits = Vec::new();
        namtor_ui::privacy::scan_value(&json, &mut hits);
        assert!(
            hits.iter()
                .any(|h: &String| h.to_lowercase().contains(forbidden)),
            "forbidden field '{forbidden}' was NOT flagged in ProfileSetupView"
        );
    }
}

/// Mutation test: every forbidden substring injected into a `SystemView`-
/// shaped JSON object is caught by the scan.
#[test]
fn mutation_test_forbidden_fields_in_system_view() {
    use namtor_ui::view_model::{ProtectionView, SystemView};
    let clean = SystemView {
        daemon_running: true,
        camera_detected: true,
        models_loaded: true,
        protection: ProtectionView::default(),
    };
    assert!(
        forbidden_fields(&clean).is_empty(),
        "clean SystemView must pass scan"
    );

    for forbidden in FORBIDDEN_SUBSTRINGS {
        let key = (*forbidden).to_string();
        let json = serde_json::json!({
            "daemon_running": true,
            "camera_detected": true,
            "models_loaded": true,
            "protection": {
                "label": "TPM2 (hardware-bound)",
                "warning": "",
                "is_software_fallback": false,
                "emitter_reduced": false
            },
            key: "injected",
        });
        let mut hits = Vec::new();
        namtor_ui::privacy::scan_value(&json, &mut hits);
        assert!(
            hits.iter()
                .any(|h: &String| h.to_lowercase().contains(forbidden)),
            "forbidden field '{forbidden}' was NOT flagged in SystemView"
        );
    }
}

/// Mutation test: every forbidden substring injected into an
/// `AuthenticationView`-shaped JSON object is caught by the scan.
#[test]
fn mutation_test_forbidden_fields_in_authentication_view() {
    let clean = AuthenticationView::default();
    assert!(
        forbidden_fields(&clean).is_empty(),
        "clean AuthenticationView must pass scan"
    );

    for forbidden in FORBIDDEN_SUBSTRINGS {
        let key = (*forbidden).to_string();
        let json = serde_json::json!({
            "recommended_service": null,
            "safe_services": [],
            "risky_services": [],
            "unsupported_services": [],
            key: "injected",
        });
        let mut hits = Vec::new();
        namtor_ui::privacy::scan_value(&json, &mut hits);
        assert!(
            hits.iter()
                .any(|h: &String| h.to_lowercase().contains(forbidden)),
            "forbidden field '{forbidden}' was NOT flagged in AuthenticationView"
        );
    }
}

/// Registry of every user-facing string literal in the UI source.
/// Each entry is `(source_file, string)`. The biometric-wording scan and
/// mutation tests operate on this registry. Keeping it in the test file
/// makes the scan deterministic and CI-safe (no GTK init needed).
const UI_STRING_REGISTRY: &[(&str, &str)] = &[
    // scan_feedback.rs — the shared scan widgets extracted during the GTK4
    // consolidation. These were absent from the registry entirely, so the
    // privacy scan never checked the guidance text the enrollment screen
    // actually shows. Caught by ui_string_registry_covers_all_ui_sources
    // once it started deriving its expectations from the source tree.
    ("scan_feedback.rs", "Every signal is good enough right now"),
    ("scan_feedback.rs", "Face position indicator"),
    ("scan_feedback.rs", "Needs attention"),
    ("scan_feedback.rs", "Nothing"),
    ("scan_feedback.rs", "Scan guidance"),
    ("scan_feedback.rs", "Signal needing attention"),
    ("scan_feedback.rs", "Views kept"),
    ("scan_feedback.rs", "Waiting for the first view"),
    ("scan_feedback.rs", "What the scan is waiting on"),
    ("scan_feedback.rs", "What to do right now to improve the scan"),
    ("scan_feedback.rs", "—"),
    ("shell.rs", "Nam-tor"),
    ("shell.rs", "Nam-tor settings"),
    ("shell.rs", "Loading…"),
    ("shell.rs", "Loading Nam-tor settings"),
    ("shell.rs", "Cannot reach the Nam-tor daemon"),
    ("shell.rs", "Nam-tor daemon unreachable"),
    ("shell.rs", "Retry"),
    ("shell.rs", "Retry connection"),
    ("shell.rs", "Try reconnecting to the Nam-tor daemon"),
    ("shell.rs", "Profiles"),
    ("shell.rs", "Authentication"),
    ("shell.rs", "System"),
    ("enrollment.rs", "Add New Profile"),
    ("enrollment.rs", "Profile Name"),
    ("enrollment.rs", "Profile name"),
    ("enrollment.rs", "Enter a descriptive name for this profile"),
    ("enrollment.rs", "e.g. Home Office"),
    (
        "enrollment.rs",
        "Your face model is encrypted and stored locally on this device. No face data leaves the system.",
    ),
    ("enrollment.rs", "Start Enrollment"),
    ("enrollment.rs", "Start enrollment"),
    ("enrollment.rs", "Begin the face enrollment capture process"),
    ("enrollment.rs", "Cancel"),
    ("enrollment.rs", "Cancel enrollment"),
    ("enrollment.rs", "Discard the new profile and return to the list"),
    ("enrollment.rs", "Cancel profile creation"),
    ("enrollment.rs", "Quality:"),
    ("enrollment.rs", "Capture quality percentage"),
    ("enrollment.rs", "Diversity:"),
    ("enrollment.rs", "Pose diversity percentage"),
    ("enrollment.rs", "Accepted:"),
    ("enrollment.rs", "Frames accepted and seen counts"),
    ("enrollment.rs", "Live capture guidance"),
    ("enrollment.rs", "Enrollment diversity progress"),
    ("enrollment.rs", "Stop"),
    ("enrollment.rs", "Stop capture"),
    ("enrollment.rs", "Stop capturing and review the enrolled profile"),
    ("enrollment.rs", "Cancel enrollment"),
    ("enrollment.rs", "Discard the enrollment and return to the profile list"),
    ("enrollment.rs", "Cancel enrollment and discard frames"),
    ("review.rs", "Review Enrollment"),
    ("review.rs", "Quality:"),
    ("review.rs", "Capture quality percentage"),
    ("review.rs", "Diversity:"),
    ("review.rs", "Pose diversity percentage"),
    ("review.rs", "Frames accepted:"),
    ("review.rs", "Number of accepted frames"),
    ("review.rs", "Enrollment guidance"),
    ("review.rs", "Save Profile"),
    ("review.rs", "Save profile"),
    ("review.rs", "Commit the captured profile to storage"),
    ("review.rs", "Resume Capture"),
    ("review.rs", "Resume capture"),
    ("review.rs", "Return to capturing to add more frames"),
    ("review.rs", "Discard"),
    ("review.rs", "Discard profile"),
    ("review.rs", "Throw away the captured profile without saving"),
    ("profile_list.rs", "Reduced protection: Always-on emitter detected"),
    ("profile_list.rs", "Reduced emitter protection warning"),
    ("profile_list.rs", "No profiles enrolled yet."),
    ("profile_list.rs", "Empty profile list"),
    ("profile_list.rs", "Enrolled profile row"),
    ("profile_list.rs", "Enabled"),
    ("profile_list.rs", "Enable profile"),
    ("profile_list.rs", "Toggle profile"),
    ("profile_list.rs", "on or off"),
    ("profile_list.rs", "last used:"),
    ("profile_list.rs", "never"),
    ("profile_list.rs", "Timestamp of last successful authentication"),
    ("system_status.rs", "System status unavailable."),
    ("system_status.rs", "System status unavailable"),
    ("system_status.rs", "System status"),
    ("system_status.rs", "Nam-tor Daemon"),
    ("system_status.rs", "Whether the Nam-tor daemon process is running"),
    ("system_status.rs", "Running"),
    ("system_status.rs", "Stopped"),
    ("system_status.rs", "IR Camera"),
    ("system_status.rs", "Whether an infrared camera was detected"),
    ("system_status.rs", "Detected"),
    ("system_status.rs", "Not detected"),
    ("system_status.rs", "Inference Models"),
    ("system_status.rs", "Whether the inference models are loaded"),
    ("system_status.rs", "Loaded"),
    ("system_status.rs", "Missing"),
    ("system_status.rs", "Software (fallback)"),
    ("system_status.rs", "TPM2 Hardware-bound"),
    ("system_status.rs", "Storage Protection"),
    (
        "system_status.rs",
        "How the enrolled face profiles are protected at rest",
    ),
    ("system_status.rs", "Storage protection warning"),
    ("system_status.rs", "Troubleshooting"),
    ("system_status.rs", "Restart Daemon"),
    ("system_status.rs", "Restart the Nam-tor daemon"),
    ("system_status.rs", "Restart the system daemon service"),
    ("system_status.rs", "View System Logs"),
    ("system_status.rs", "View Nam-tor system logs"),
    ("system_status.rs", "Open the system journal for the Nam-tor daemon"),
    ("pam_services.rs", "Authentication uses"),
    ("pam_services.rs", "No PAM services found."),
    ("pam_services.rs", "Empty PAM service list"),
    ("pam_services.rs", "Compatibility:"),
    ("pam_services.rs", "Service compatibility:"),
    ("pam_services.rs", "Toggle"),
    ("pam_services.rs", "Enable"),
    ("pam_services.rs", "authentication"),
    ("pam_services.rs", "Toggle whether"),
    ("pam_services.rs", "uses face authentication"),
    ("pam_services.rs", "is incompatible and cannot be enabled."),
    ("recovery.rs", "Recovery Steps:"),
    ("recovery.rs", "Detailed explanation of the recovery state"),
    ("recovery.rs", "Recovery guidance steps"),
];

/// Test 7: no user-facing UI string in the registry contains a forbidden
/// biometric / internal term.
#[test]
fn no_biometric_wording_in_ui_strings() {
    let mut leaks = Vec::new();
    for (file, s) in UI_STRING_REGISTRY {
        let hits = forbidden_words_in_ui_string(s);
        if !hits.is_empty() {
            leaks.push(format!("{file}: {s:?} -> {hits:?}"));
        }
    }
    assert!(
        leaks.is_empty(),
        "forbidden biometric/internal wording in UI strings:\n{}",
        leaks.join("\n")
    );
}

/// Test 8: the privacy scan covers the new view types
/// (AuthenticationView, SystemView, ProfileSetupView) — each has a dedicated
/// scan test above, and each is exercised by the populated model test.
#[test]
fn privacy_scan_covers_new_view_types() {
    use namtor_ui::view_model::{
        AuthenticationView, PendingPamToggle, PresenterModel, ProfileSetupView, SystemView,
    };

    let model = PresenterModel {
        authentication: Some(AuthenticationView::default()),
        system: Some(SystemView::default()),
        ..Default::default()
    };
    assert!(forbidden_fields(&model).is_empty());

    let setup = ProfileSetupView {
        name_placeholder: "e.g. Home".into(),
        privacy_acknowledged: true,
    };
    assert!(forbidden_fields(&setup).is_empty());

    // All three new view types must be reachable from the public API.
    let _auth = AuthenticationView::default();
    let _sys = SystemView::default();
    let _setup = ProfileSetupView::default();
    let _toggle = PendingPamToggle {
        id: "x".into(),
        display_name: "X".into(),
        enabled: false,
        high_impact: false,
    };
}

/// Test 10 (mutation): inject a forbidden word into a copy of a registry
/// string and verify the scan catches it. This proves the
/// `no_biometric_wording_in_ui_strings` test is not vacuous — if a
/// contributor adds a forbidden term to any UI string, the test fails.
#[test]
fn mutation_forbidden_word_rejected() {
    let baseline = "Save the enrolled profile";
    assert!(
        forbidden_words_in_ui_string(baseline).is_empty(),
        "baseline must be clean"
    );

    for forbidden in FORBIDDEN_SUBSTRINGS {
        let poisoned = format!("{baseline} ({forbidden} leaked)");
        let hits = forbidden_words_in_ui_string(&poisoned);
        assert!(
            hits.contains(forbidden),
            "forbidden word '{forbidden}' was NOT caught in UI string"
        );
    }
}

/// The UI string registry is non-empty and covers every UI source file
/// that renders user-facing text.
#[test]
fn ui_string_registry_covers_all_ui_sources() {
    assert!(
        !UI_STRING_REGISTRY.is_empty(),
        "registry must not be vacuous"
    );
    let files: std::collections::BTreeSet<&str> =
        UI_STRING_REGISTRY.iter().map(|(f, _)| *f).collect();

    // Derive the expected set from the source tree, not from a literal list.
    //
    // The list was hardcoded, so it could only ever check that the registry
    // still mentioned files someone had thought of once. It required
    // "navigation.rs" — deleted in 196d4a4 when the two divergent enrollment
    // UIs were consolidated — so the registry kept 31 strings belonging to
    // code that no longer ships, and this test passed while asserting
    // coverage of a file that does not exist. It equally could not notice a
    // NEW component landing with no registry entries, which is the only
    // regression it exists to catch.
    let components = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/components");
    let mut expected: Vec<String> = std::fs::read_dir(&components)
        .expect("read src/components")
        .filter_map(|e| {
            let name = e.ok()?.file_name().to_string_lossy().into_owned();
            // mod.rs only re-exports; it holds no user-facing strings.
            (name.ends_with(".rs") && name != "mod.rs").then_some(name)
        })
        .collect();
    expected.push("shell.rs".to_string());
    expected.sort();

    let missing: Vec<&String> = expected
        .iter()
        .filter(|f| !files.contains(f.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "UI sources with no strings in the registry: {missing:?}\n\
         Add their user-facing strings so the privacy scan covers them."
    );

    // And the converse: a registry key naming a file that no longer exists
    // means the scan is guarding deleted code.
    let stale: Vec<&&str> = files
        .iter()
        .filter(|f| !expected.iter().any(|e| e == *f))
        .collect();
    assert!(
        stale.is_empty(),
        "registry names UI sources that no longer exist: {stale:?}"
    );
}
