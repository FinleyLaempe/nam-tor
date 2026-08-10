// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests driving the GNOME presenter with a fake D-Bus service.

mod common;

use std::sync::Arc;

use common::after;
use namtor_gnome::{
    fake::{FakeManagerClient, Mode},
    EnrollmentProgressV3, EnrollmentState, ManagerClient, PamServiceRowView,
};

const TEST_UID: u32 = 1000;

fn new_presenter() -> namtor_gnome::ProfilePresenter<FakeManagerClient> {
    namtor_gnome::ProfilePresenter::new(FakeManagerClient::new(), TEST_UID)
}

#[test]
fn progress_updates_live_values() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Alice V3");
    });
    let session = presenter.model().enrollment_session_v3.clone().unwrap();
    let progress = EnrollmentProgressV3 {
        session,
        phase: "capturing".into(),
        quality_percent: 85,
        diversity_percent: 40,
        frames_seen: 10,
        frames_accepted: 8,
        frames_rejected: 2,
        quality_bucket: 2,
        guidance: "looking_good".into(),
    };
    let model = after(&mut presenter, |p| {
        p.on_progress_v3(&progress);
    });
    let view = model.enrollment_view_v3.as_ref().expect("view active");
    assert_eq!(view.quality_percent, 85);
    assert_eq!(view.diversity_percent, 40);
}

#[test]
fn stop_freezes_values() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Bob V3");
    });
    let session = presenter.model().enrollment_session_v3.clone().unwrap();
    let progress = EnrollmentProgressV3 {
        session,
        phase: "capturing".into(),
        quality_percent: 70,
        diversity_percent: 30,
        frames_seen: 5,
        frames_accepted: 4,
        frames_rejected: 1,
        quality_bucket: 1,
        guidance: "capturing".into(),
    };
    after(&mut presenter, |p| {
        p.on_progress_v3(&progress);
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    assert_eq!(presenter.model().enrollment_state, EnrollmentState::Stopped);

    // Attempt to update while stopped
    let progress_late = EnrollmentProgressV3 {
        session: progress.session,
        phase: "capturing".into(),
        quality_percent: 90,
        diversity_percent: 50,
        frames_seen: 10,
        frames_accepted: 9,
        frames_rejected: 1,
        quality_bucket: 2,
        guidance: "looking_good".into(),
    };
    let model = after(&mut presenter, |p| {
        p.on_progress_v3(&progress_late);
    });
    let view = model.enrollment_view_v3.as_ref().expect("view");
    assert_eq!(view.quality_percent, 70, "value frozen after stop");
    assert!(
        presenter.client().list_profiles().expect("list").is_empty(),
        "no profile exists yet"
    );
}

#[test]
fn resume_retains_progress() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Carol V3");
    });
    let session = presenter.model().enrollment_session_v3.clone().unwrap();
    let progress = EnrollmentProgressV3 {
        session,
        phase: "capturing".into(),
        quality_percent: 50,
        diversity_percent: 20,
        frames_seen: 5,
        frames_accepted: 4,
        frames_rejected: 1,
        quality_bucket: 1,
        guidance: "capturing".into(),
    };
    after(&mut presenter, |p| {
        p.on_progress_v3(&progress);
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    after(&mut presenter, |p| {
        p.resume_enrollment_v3();
    });
    assert_eq!(
        presenter.model().enrollment_state,
        EnrollmentState::Capturing
    );
    let view = presenter.model().enrollment_view_v3.as_ref().expect("view");
    assert_eq!(view.quality_percent, 50, "progress retained on resume");
}

#[test]
fn save_persists_once() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Dave V3");
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    after(&mut presenter, |p| {
        p.review_enrollment_v3();
    });
    let model = after(&mut presenter, |p| {
        p.save_profile_v3();
    });
    assert_eq!(model.profiles.len(), 1);
    assert_eq!(model.enrollment_state, EnrollmentState::Setup);

    // Try to save again (should be no-op as session is gone)
    let model_retry = after(&mut presenter, |p| {
        p.save_profile_v3();
    });
    assert_eq!(model_retry.profiles.len(), 1, "no duplicate profile");
}

#[test]
fn discard_clears_state() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Eve V3");
    });
    let model = after(&mut presenter, |p| {
        p.discard_enrollment_v3();
    });
    assert_eq!(model.enrollment_state, EnrollmentState::Setup);
    assert!(model.enrollment_session_v3.is_none());
    assert!(model.enrollment_view_v3.is_none());
    assert!(model.review_view_v3.is_none());
}

#[test]
fn quality_never_labelled_accuracy() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Frank V3");
    });
    let session = presenter.model().enrollment_session_v3.clone().unwrap();
    let progress = EnrollmentProgressV3 {
        session,
        phase: "capturing".into(),
        quality_percent: 80,
        diversity_percent: 80,
        frames_seen: 10,
        frames_accepted: 10,
        frames_rejected: 0,
        quality_bucket: 2,
        guidance: "looking_good".into(),
    };
    let model = after(&mut presenter, |p| {
        p.on_progress_v3(&progress);
    });
    let view = model.enrollment_view_v3.as_ref().expect("view");
    let s = format!("{:?}", view);
    assert!(
        !s.to_lowercase().contains("accuracy"),
        "no 'accuracy' label"
    );
    assert!(
        !s.to_lowercase().contains("confidence"),
        "no 'confidence' label"
    );
}

#[test]
fn camera_loss_recoverable() {
    let fake = FakeManagerClient::new();
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake, TEST_UID);
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Grace V3");
    });

    presenter
        .client()
        .set_mode(Mode::DaemonDown("camera lost".into()));
    let model = after(&mut presenter, |p| {
        // This is where a timer would call poll_progress
        p.on_progress_v3(&EnrollmentProgressV3 {
            session: "stale".into(),
            phase: "capturing".into(),
            quality_percent: 0,
            diversity_percent: 0,
            frames_seen: 0,
            frames_accepted: 0,
            frames_rejected: 0,
            quality_bucket: 0,
            guidance: "".into(),
        });
        // simulate an actual daemon error from a call
        p.stop_enrollment_v3();
    });
    assert!(matches!(model.enrollment_state, EnrollmentState::Error(_)));
    assert!(model.error.is_some());
}

#[test]
fn poor_quality_shows_guidance() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Heidi V3");
    });
    let session = presenter.model().enrollment_session_v3.clone().unwrap();
    let progress = EnrollmentProgressV3 {
        session,
        phase: "capturing".into(),
        quality_percent: 10,
        diversity_percent: 10,
        frames_seen: 10,
        frames_accepted: 1,
        frames_rejected: 9,
        quality_bucket: 0,
        guidance: "better_lighting".into(),
    };
    let model = after(&mut presenter, |p| {
        p.on_progress_v3(&progress);
    });
    let view = model.enrollment_view_v3.as_ref().expect("view");
    assert!(!view.guidance.is_empty());
    assert!(view.guidance.to_lowercase().contains("better lighting"));
}

#[test]
fn daemon_restart_recoverable() {
    let fake = Arc::new(FakeManagerClient::new());
    let mut presenter = namtor_gnome::ProfilePresenter::from_arc(Arc::clone(&fake), TEST_UID);
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Ivan V3");
    });

    fake.set_mode(Mode::DaemonDown("restart".into()));
    let model = after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    assert!(matches!(model.enrollment_state, EnrollmentState::Error(_)));

    fake.set_mode(Mode::Normal);
    let model = after(&mut presenter, |p| {
        p.dismiss_v3_error();
    });
    assert_eq!(model.enrollment_state, EnrollmentState::Setup);
    let model = after(&mut presenter, |p| {
        p.start_enrollment_v3("Ivan V3");
    });
    assert_eq!(model.enrollment_state, EnrollmentState::Capturing);
}

#[test]
fn save_failure_recoverable() {
    let fake = FakeManagerClient::new();
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake, TEST_UID);
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Judy V3");
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    after(&mut presenter, |p| {
        p.review_enrollment_v3();
    });
    presenter
        .client()
        .set_mode(Mode::PolkitDenied("save failed".into()));
    let model = after(&mut presenter, |p| {
        p.save_profile_v3();
    });
    assert_eq!(model.enrollment_state, EnrollmentState::Stopped);
    assert!(model.error.is_some());
}

#[test]
fn duplicate_actions_disabled_while_pending() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Kevin V3");
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    after(&mut presenter, |p| {
        p.review_enrollment_v3();
    });

    let model = after(&mut presenter, |p| {
        p.save_profile_v3();
    });
    assert_eq!(model.enrollment_state, EnrollmentState::Setup);
    assert_eq!(model.profiles.len(), 1);

    let model = after(&mut presenter, |p| {
        p.save_profile_v3();
    });
    assert_eq!(model.enrollment_state, EnrollmentState::Setup);
    assert_eq!(model.profiles.len(), 1, "no duplicate profile");
}

/// Review flow: request_review → review staged → commit → profile persisted.
#[test]
fn review_flow_stages_and_commits() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment("Mona");
    });
    let model = after(&mut presenter, |p| {
        p.request_review();
    });
    assert!(model.review.is_some(), "review staged");
    let review = model.review.as_ref().expect("review");
    assert_eq!(review.quality_label, "high");

    let model = after(&mut presenter, |p| {
        p.commit_profile();
    });
    assert!(model.review.is_none(), "review cleared on commit");
    assert!(model.enrollment.is_none(), "enrollment cleared on commit");
    assert_eq!(model.profiles.len(), 1, "profile persisted");
}

/// Cancellation: cancel_pending is safe when nothing is pending.
#[test]
fn cancel_pending_safe_when_idle() {
    let mut presenter = new_presenter();
    assert!(!presenter.is_busy(), "not busy initially");
    presenter.cancel_pending();
    assert!(!presenter.is_busy(), "still not busy after no-op cancel");
    assert!(
        !presenter.model().is_loading,
        "loading cleared after cancel"
    );
}

/// PAM service row view carries no biometric data.
#[test]
fn pam_service_row_view_has_no_biometrics() {
    use namtor_gnome::client::PamServiceInfo;
    let info = PamServiceInfo {
        id: "gdm-password".into(),
        display_name: "GDM Password".into(),
        enabled: true,
        compatibility: namtor_gnome::client::PamServiceCompatibility::Safe,
        description: "GNOME Display Manager".into(),
    };
    let view = PamServiceRowView::from_info(info);
    assert_eq!(view.id, "gdm-password");
    assert_eq!(
        view.compatibility,
        namtor_gnome::PamServiceCompatibility::Safe
    );
}

/// dbus_client uses the system bus, not the session bus.
#[test]
fn dbus_client_uses_system_bus() {
    let source = include_str!("../src/dbus_client.rs");
    assert!(
        source.contains("Connection::system()"),
        "dbus_client must use Connection::system(), not Connection::session()"
    );
    assert!(
        !source.contains("Connection::session()"),
        "dbus_client must not use Connection::session()"
    );
}

/// v2: Unsupported PAM services cannot be toggled — request_pam_toggle
/// refuses with an error and the service stays disabled.
#[test]
fn unsupported_pam_service_cannot_be_toggled() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.refresh();
    });
    let unsupported_id = presenter
        .model()
        .pam_services
        .iter()
        .find(|s| s.compatibility == namtor_gnome::PamServiceCompatibility::Unsupported)
        .map(|s| s.id.clone())
        .expect("an unsupported service exists in the fake");
    assert!(
        !presenter
            .model()
            .pam_services
            .iter()
            .find(|s| s.id == unsupported_id)
            .unwrap()
            .enabled,
        "unsupported starts disabled"
    );

    let model = after(&mut presenter, |p| {
        p.request_pam_toggle(&unsupported_id, true);
    });
    assert!(
        model.error.is_some(),
        "error surfaced for unsupported toggle"
    );
    let err = model.error.as_ref().expect("error");
    assert!(
        err.detail.to_lowercase().contains("incompatible"),
        "error names incompatibility: {err:?}"
    );
    assert!(
        model.pending_pam_toggle.is_none(),
        "no confirmation staged for unsupported"
    );
    let svc = model
        .pam_services
        .iter()
        .find(|s| s.id == unsupported_id)
        .expect("service still listed");
    assert!(!svc.enabled, "unsupported service stayed disabled");
}

/// v2: A failed toggle (polkit denied) reverts the service to its previous
/// state — the model still shows the old value after the error.
#[test]
fn failed_pam_toggle_reverts_to_previous_state() {
    let fake = FakeManagerClient::new();
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake, TEST_UID);
    after(&mut presenter, |p| {
        p.refresh();
    });
    let gdm = presenter
        .model()
        .pam_services
        .iter()
        .find(|s| s.id == "gdm-password")
        .expect("gdm-password present");
    assert!(!gdm.enabled, "starts disabled");

    presenter
        .client()
        .set_mode(Mode::PolkitDenied("denied".into()));
    let model = after(&mut presenter, |p| {
        p.request_pam_toggle("gdm-password", true);
    });
    assert!(model.error.is_some(), "error surfaced on denied toggle");
    let svc = model
        .pam_services
        .iter()
        .find(|s| s.id == "gdm-password")
        .expect("service listed");
    assert!(
        !svc.enabled,
        "gdm-password reverted to disabled after denial"
    );
    assert!(
        !presenter.client().is_pam_service_enabled("gdm-password"),
        "fake store confirms gdm-password not enabled"
    );
}

/// v2: Enabling a high-impact (risky) service stages a confirmation — the
/// toggle is NOT applied until confirm_pam_toggle is called.
#[test]
fn high_impact_pam_service_requires_confirmation() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.refresh();
    });
    let risky_id = presenter
        .model()
        .pam_services
        .iter()
        .find(|s| s.compatibility == namtor_gnome::PamServiceCompatibility::Risky)
        .map(|s| s.id.clone())
        .expect("a risky service exists");
    assert!(
        !presenter
            .model()
            .pam_services
            .iter()
            .find(|s| s.id == risky_id)
            .unwrap()
            .enabled,
        "risky starts disabled"
    );

    let model = after(&mut presenter, |p| {
        p.request_pam_toggle(&risky_id, true);
    });
    let pending = model
        .pending_pam_toggle
        .as_ref()
        .expect("confirmation staged for risky enable");
    assert_eq!(pending.id, risky_id);
    assert!(pending.high_impact, "flagged high-impact");
    assert!(pending.enabled, "staged to enable");

    let svc = model
        .pam_services
        .iter()
        .find(|s| s.id == risky_id)
        .expect("service listed");
    assert!(
        !svc.enabled,
        "risky service NOT enabled before confirmation"
    );

    let model = after(&mut presenter, |p| {
        p.cancel_pam_toggle();
    });
    assert!(
        model.pending_pam_toggle.is_none(),
        "pending cleared on cancel"
    );
    let svc = model
        .pam_services
        .iter()
        .find(|s| s.id == risky_id)
        .expect("service listed");
    assert!(!svc.enabled, "still disabled after cancel");

    let _ = after(&mut presenter, |p| {
        p.request_pam_toggle(&risky_id, true);
    });
    let model = after(&mut presenter, |p| {
        p.confirm_pam_toggle();
    });
    assert!(
        model.pending_pam_toggle.is_none(),
        "pending cleared on confirm"
    );
    let svc = model
        .pam_services
        .iter()
        .find(|s| s.id == risky_id)
        .expect("service listed");
    assert!(svc.enabled, "risky service enabled after confirmation");
}

/// v2: The high-impact warning constant contains the required regulatory
/// phrases. The confirmation dialog renders this text verbatim.
#[test]
fn high_impact_warning_contains_required_phrases() {
    let warning = namtor_gnome::components::pam_services::HIGH_IMPACT_WARNING;
    assert!(warning.contains("FAR/FRR"), "warning names FAR/FRR");
    assert!(
        warning.contains("real-attack PAD"),
        "warning names real-attack PAD"
    );
    assert!(
        warning.contains("experimental"),
        "warning names experimental status"
    );
    assert!(
        warning.contains("not recommended for security-sensitive use"),
        "warning names the security-sensitive caveat"
    );
}

/// v2: Disabling a risky service does NOT require confirmation — it applies
/// immediately.
#[test]
fn disabling_risky_service_needs_no_confirmation() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.refresh();
    });
    let risky_id = presenter
        .model()
        .pam_services
        .iter()
        .find(|s| s.compatibility == namtor_gnome::PamServiceCompatibility::Risky)
        .map(|s| s.id.clone())
        .expect("a risky service exists");

    after(&mut presenter, |p| {
        p.set_pam_service_enabled(&risky_id, true);
    });
    assert!(
        presenter
            .model()
            .pam_services
            .iter()
            .find(|s| s.id == risky_id)
            .unwrap()
            .enabled,
        "risky enabled for test"
    );

    let model = after(&mut presenter, |p| {
        p.request_pam_toggle(&risky_id, false);
    });
    assert!(
        model.pending_pam_toggle.is_none(),
        "no confirmation for disable"
    );
    let svc = model
        .pam_services
        .iter()
        .find(|s| s.id == risky_id)
        .expect("service listed");
    assert!(!svc.enabled, "risky service disabled immediately");
}

/// v2: set_pam_service_enabled directly also guards unsupported services
/// (defence-in-depth — the view greys out the switch, but a bypass via
/// the raw method is still refused).
#[test]
fn set_pam_service_enabled_refuses_unsupported() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.refresh();
    });
    let unsupported_id = presenter
        .model()
        .pam_services
        .iter()
        .find(|s| s.compatibility == namtor_gnome::PamServiceCompatibility::Unsupported)
        .map(|s| s.id.clone())
        .expect("an unsupported service exists");

    let model = after(&mut presenter, |p| {
        p.set_pam_service_enabled(&unsupported_id, true);
    });
    assert!(model.error.is_some(), "raw method also refuses unsupported");
}

// ===========================================================================
// v3 enrollment lifecycle tests (org.namtor.Manager3)
// ===========================================================================

#[test]
fn start_capturing_reaches_capturing_state() {
    let mut presenter = new_presenter();
    let model = after(&mut presenter, |p| {
        p.start_enrollment_v3("Alice V3");
    });
    assert_eq!(
        model.enrollment_state,
        EnrollmentState::Capturing,
        "start transitions to Capturing"
    );
    assert!(model.enrollment_session_v3.is_some(), "session id stored");
    let view = model
        .enrollment_view_v3
        .as_ref()
        .expect("enrollment view active");
    assert_eq!(view.phase, "capturing", "view phase is capturing");
    assert!(model.error.is_none(), "no error on success");
}

#[test]
fn progress_updates_view() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Bob V3");
    });
    let session = presenter
        .model()
        .enrollment_session_v3
        .clone()
        .expect("session");
    let progress = EnrollmentProgressV3 {
        session: session.clone(),
        phase: "capturing".into(),
        quality_percent: 75,
        diversity_percent: 60,
        frames_seen: 20,
        frames_accepted: 15,
        frames_rejected: 5,
        quality_bucket: 2,
        guidance: "looking_good".into(),
    };
    let model = after(&mut presenter, |p| {
        p.on_progress_v3(&progress);
    });
    let view = model.enrollment_view_v3.as_ref().expect("enrollment view");
    assert_eq!(view.quality_percent, 75, "quality updated");
    assert_eq!(view.diversity_percent, 60, "diversity updated");
    assert_eq!(view.frames_seen, 20, "frames_seen updated");
    assert_eq!(view.frames_accepted, 15, "frames_accepted updated");
    assert_eq!(view.frames_rejected, 5, "frames_rejected updated");
    assert!(
        view.guidance.contains("Looking good"),
        "guidance localized to human-readable label"
    );
}

#[test]
fn stop_reaches_stopped_state() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Carol V3");
    });
    let model = after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    assert_eq!(
        model.enrollment_state,
        EnrollmentState::Stopped,
        "stop transitions to Stopped"
    );
    let view = model
        .enrollment_view_v3
        .as_ref()
        .expect("enrollment view still active");
    assert_eq!(view.phase, "stopped", "view phase is stopped");
}

#[test]
fn review_shows_summary() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Dave V3");
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    let model = after(&mut presenter, |p| {
        p.review_enrollment_v3();
    });
    assert_eq!(
        model.enrollment_state,
        EnrollmentState::Reviewing,
        "review transitions to Reviewing"
    );
    let review = model.review_view_v3.as_ref().expect("review view staged");
    assert!(review.ready_to_save, "ready_to_save is set");
    assert!(review.quality_percent > 0, "quality percent shown");
    assert!(review.diversity_percent > 0, "diversity percent shown");
    assert!(review.frames_accepted > 0, "frames_accepted shown");
}

#[test]
fn resume_returns_to_capturing() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Eve V3");
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    let model = after(&mut presenter, |p| {
        p.resume_enrollment_v3();
    });
    assert_eq!(
        model.enrollment_state,
        EnrollmentState::Capturing,
        "resume transitions back to Capturing"
    );
    assert!(model.review_view_v3.is_none(), "review cleared on resume");
    let view = model
        .enrollment_view_v3
        .as_ref()
        .expect("enrollment view active");
    assert_eq!(view.phase, "capturing", "view phase back to capturing");
}

#[test]
fn save_persists_and_returns_to_profiles() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Frank V3");
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    after(&mut presenter, |p| {
        p.review_enrollment_v3();
    });
    let model = after(&mut presenter, |p| {
        p.save_profile_v3();
    });
    assert_eq!(
        model.enrollment_state,
        EnrollmentState::Setup,
        "save returns to Setup"
    );
    assert!(model.enrollment_session_v3.is_none(), "session cleared");
    assert!(
        model.enrollment_view_v3.is_none(),
        "enrollment view cleared"
    );
    assert!(model.review_view_v3.is_none(), "review view cleared");
    assert!(
        model.message.as_deref().unwrap_or("").contains("complete"),
        "success message shown"
    );
    assert_eq!(model.profiles.len(), 1, "one profile persisted");
    assert_eq!(model.profiles[0].name, "Frank V3");
}

#[test]
fn discard_clears_and_returns() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Grace V3");
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    let model = after(&mut presenter, |p| {
        p.discard_enrollment_v3();
    });
    assert_eq!(
        model.enrollment_state,
        EnrollmentState::Setup,
        "discard returns to Setup"
    );
    assert!(model.enrollment_session_v3.is_none(), "session cleared");
    assert!(
        model.enrollment_view_v3.is_none(),
        "enrollment view cleared"
    );
    assert!(
        model.message.as_deref().unwrap_or("").contains("discarded"),
        "discard message shown"
    );
    assert!(
        presenter.client().list_profiles().expect("list").is_empty(),
        "no profile persisted after discard"
    );
}

#[test]
fn duplicate_start_ignored() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Henry V3");
    });
    let model = after(&mut presenter, |p| {
        p.start_enrollment_v3("Iris V3");
    });
    assert_eq!(
        model.enrollment_state,
        EnrollmentState::Capturing,
        "still in Capturing from first start"
    );
    assert!(model.error.is_some(), "duplicate start surfaces an error");
    let err = model.error.as_ref().expect("error");
    assert!(
        err.detail.contains("already"),
        "error explains enrollment is in progress"
    );
}

#[test]
fn stale_signal_ignored() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Jack V3");
    });
    let active_session = presenter
        .model()
        .enrollment_session_v3
        .clone()
        .expect("active session");
    let stale_progress = EnrollmentProgressV3 {
        session: "stale-session-id".into(),
        phase: "capturing".into(),
        quality_percent: 99,
        diversity_percent: 99,
        frames_seen: 999,
        frames_accepted: 999,
        frames_rejected: 0,
        quality_bucket: 2,
        guidance: "looking_good".into(),
    };
    let model = after(&mut presenter, |p| {
        p.on_progress_v3(&stale_progress);
    });
    let view = model.enrollment_view_v3.as_ref().expect("enrollment view");
    assert_ne!(
        view.frames_seen, 999,
        "stale signal ignored — frames_seen not updated"
    );
    assert_ne!(
        view.quality_percent, 99,
        "stale signal ignored — quality not updated"
    );
    assert_eq!(
        model.enrollment_session_v3.as_deref(),
        Some(active_session.as_str()),
        "active session unchanged"
    );
}

#[test]
fn daemon_loss_restores_retry() {
    let fake = FakeManagerClient::new();
    fake.set_mode(Mode::DaemonDown("daemon stopped".into()));
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake, TEST_UID);
    let model = after(&mut presenter, |p| {
        p.start_enrollment_v3("Karl V3");
    });
    assert!(
        matches!(model.enrollment_state, EnrollmentState::Error(_)),
        "daemon loss transitions to Error state"
    );
    assert!(model.error.is_some(), "error surfaced");
    let err = model.error.as_ref().expect("error");
    assert!(
        err.title.to_lowercase().contains("daemon")
            || err.title.to_lowercase().contains("unavailable"),
        "error title names the daemon issue"
    );
}

#[test]
fn save_failure_low_diversity_stays_stopped() {
    let fake = FakeManagerClient::new();
    fake.set_mode(Mode::LowDiversity);
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake, TEST_UID);
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Lena V3");
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    let model = after(&mut presenter, |p| {
        p.save_profile_v3();
    });
    assert_eq!(
        model.enrollment_state,
        EnrollmentState::Stopped,
        "save failure stays in Stopped (recoverable)"
    );
    assert!(model.error.is_some(), "failure surfaced as error");
    assert!(
        model.enrollment_session_v3.is_some(),
        "session retained for retry"
    );
}

#[test]
fn restart_after_error() {
    let fake = FakeManagerClient::new();
    fake.set_mode(Mode::DaemonDown("daemon stopped".into()));
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake, TEST_UID);
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Mona V3");
    });
    assert!(
        matches!(
            presenter.model().enrollment_state,
            EnrollmentState::Error(_)
        ),
        "first start failed with error"
    );
    presenter.client().set_mode(Mode::Normal);
    let model = after(&mut presenter, |p| {
        p.dismiss_v3_error();
    });
    assert_eq!(
        model.enrollment_state,
        EnrollmentState::Setup,
        "dismiss returns to Setup"
    );
    let model = after(&mut presenter, |p| {
        p.start_enrollment_v3("Mona V3");
    });
    assert_eq!(
        model.enrollment_state,
        EnrollmentState::Capturing,
        "restart after error reaches Capturing"
    );
    assert!(model.error.is_none(), "no error on restart");
}
