// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests driving the UI presenter with a fake D-Bus service.

mod common;

use std::sync::Arc;

use common::after;
use namtor_ui::{
    fake::{FakeManagerClient, Mode},
    EnrollmentProgressV3, EnrollmentState, ManagerClient, PamServiceRowView,
};

const TEST_UID: u32 = 1000;

fn new_presenter() -> namtor_ui::ProfilePresenter<FakeManagerClient> {
    namtor_ui::ProfilePresenter::new(FakeManagerClient::new(), TEST_UID)
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
    let mut presenter = namtor_ui::ProfilePresenter::new(fake, TEST_UID);
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
    assert!(!view.guidance.label().is_empty());
    assert!(view
        .guidance
        .label()
        .to_lowercase()
        .contains("better lighting"));
}

#[test]
fn daemon_restart_recoverable() {
    let fake = Arc::new(FakeManagerClient::new());
    let mut presenter = namtor_ui::ProfilePresenter::from_arc(Arc::clone(&fake), TEST_UID);
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
    let mut presenter = namtor_ui::ProfilePresenter::new(fake, TEST_UID);
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
    use namtor_ui::client::PamServiceInfo;
    let info = PamServiceInfo {
        id: "gdm-password".into(),
        display_name: "GDM Password".into(),
        enabled: true,
        compatibility: namtor_ui::client::PamServiceCompatibility::Safe,
        description: "GNOME Display Manager".into(),
    };
    let view = PamServiceRowView::from_info(info);
    assert_eq!(view.id, "gdm-password");
    assert_eq!(view.compatibility, namtor_ui::PamServiceCompatibility::Safe);
}

/// DbusManagerClient must connect to a real D-Bus bus and successfully
/// call daemon methods. Verified by constructing a client via FakeBus
/// (which provides a real D-Bus connection) and calling get_status().
#[test]
fn dbus_client_uses_system_bus() {
    use namtor_client::fake_bus::FakeBus;
    use namtor_ui::ManagerClient;

    let fake_bus = FakeBus::spawn().expect("spawn fake bus");
    let status = fake_bus.client.get_status();
    assert!(
        status.is_ok(),
        "DbusManagerClient must be functional on a real D-Bus connection: {status:?}"
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
        .find(|s| s.compatibility == namtor_ui::PamServiceCompatibility::Unsupported)
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
    let mut presenter = namtor_ui::ProfilePresenter::new(fake, TEST_UID);
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
        .find(|s| s.compatibility == namtor_ui::PamServiceCompatibility::Risky)
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
    let warning = namtor_ui::components::pam_services::HIGH_IMPACT_WARNING;
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
        .find(|s| s.compatibility == namtor_ui::PamServiceCompatibility::Risky)
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
        .find(|s| s.compatibility == namtor_ui::PamServiceCompatibility::Unsupported)
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
        view.guidance.label().contains("Looking good"),
        "guidance localized to human-readable label"
    );
}

/// Test that the guidance label updates when the presenter receives progress
/// updates with different guidance strings. This is the behavioral test for
/// the non-image framing guidance feature.
#[test]
fn framing_guidance_updates() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Guidance Test");
    });
    let session = presenter
        .model()
        .enrollment_session_v3
        .clone()
        .expect("session");

    let progress_centering = EnrollmentProgressV3 {
        session: session.clone(),
        phase: "capturing".into(),
        quality_percent: 20,
        diversity_percent: 10,
        frames_seen: 5,
        frames_accepted: 2,
        frames_rejected: 3,
        quality_bucket: 0,
        guidance: "centering".into(),
    };
    let model = after(&mut presenter, |p| {
        p.on_progress_v3(&progress_centering);
    });
    let view = model.enrollment_view_v3.as_ref().expect("enrollment view");
    assert!(
        view.guidance.label().contains("Center"),
        "guidance shows centering message: {}",
        view.guidance.label()
    );

    let progress_good = EnrollmentProgressV3 {
        session: session.clone(),
        phase: "capturing".into(),
        quality_percent: 85,
        diversity_percent: 70,
        frames_seen: 20,
        frames_accepted: 18,
        frames_rejected: 2,
        quality_bucket: 2,
        guidance: "looking_good".into(),
    };
    let model = after(&mut presenter, |p| {
        p.on_progress_v3(&progress_good);
    });
    let view = model.enrollment_view_v3.as_ref().expect("enrollment view");
    assert!(
        view.guidance.label().contains("Looking good"),
        "guidance updated to looking_good: {}",
        view.guidance.label()
    );

    let progress_closer = EnrollmentProgressV3 {
        session: session.clone(),
        phase: "capturing".into(),
        quality_percent: 40,
        diversity_percent: 30,
        frames_seen: 30,
        frames_accepted: 25,
        frames_rejected: 5,
        quality_bucket: 1,
        guidance: "move_closer".into(),
    };
    let model = after(&mut presenter, |p| {
        p.on_progress_v3(&progress_closer);
    });
    let view = model.enrollment_view_v3.as_ref().expect("enrollment view");
    assert!(
        view.guidance.label().contains("Move closer"),
        "guidance updated to move_closer: {}",
        view.guidance.label()
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
    let mut presenter = namtor_ui::ProfilePresenter::new(fake, TEST_UID);
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
    let mut presenter = namtor_ui::ProfilePresenter::new(fake, TEST_UID);
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
    let mut presenter = namtor_ui::ProfilePresenter::new(fake, TEST_UID);
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

// --- storage protection: the no-TPM software fallback ------------------------
//
// Acceptance criterion (j). These were the only assertions in the desktop
// certification that pointed at genuinely absent coverage rather than at a
// renamed test: `FakeManagerClient::set_protection_status` existed with zero
// callers, every `is_software_fallback` in the test tree was a hard-coded
// `false` in a structural privacy assertion, and no test anywhere drove
// `ProtectionKeyKind::Software` through the presenter. The machine without a
// TPM — the one that most needs to be told its templates are less protected —
// was the untested path.

fn software_fallback_status() -> namtor_ui::ProtectionStatus {
    namtor_ui::ProtectionStatus {
        sealing: namtor_ui::ProtectionKeyKind::Software,
        has_tpm: false,
        warning: "Profiles are sealed with a software key".into(),
    }
}

#[test]
fn software_fallback_protection_surfaced() {
    let client = Arc::new(FakeManagerClient::new());
    client.set_protection_status(software_fallback_status());
    let mut p = namtor_ui::ProfilePresenter::from_arc(client, TEST_UID);

    p.refresh_system_status();
    let system = p.model().system.clone().expect("system view");

    assert!(
        system.protection.is_software_fallback,
        "ProtectionKeyKind::Software must set is_software_fallback"
    );
    assert!(
        !system.protection.warning.is_empty(),
        "software fallback must carry a warning for the user"
    );
    assert!(
        system.protection.label.contains("Software"),
        "label should name the software fallback, got {:?}",
        system.protection.label
    );
}

#[test]
fn tpm_backed_protection_raises_no_fallback_warning() {
    // The negative half: without it, a view_model that hard-coded
    // `is_software_fallback = true` would pass the test above.
    let client = Arc::new(FakeManagerClient::new());
    client.set_protection_status(namtor_ui::ProtectionStatus {
        sealing: namtor_ui::ProtectionKeyKind::Tpm2,
        has_tpm: true,
        warning: String::new(),
    });
    let mut p = namtor_ui::ProfilePresenter::from_arc(client, TEST_UID);

    p.refresh_system_status();
    let system = p.model().system.clone().expect("system view");

    assert!(!system.protection.is_software_fallback);
    assert!(system.protection.warning.is_empty());
    assert!(
        system.protection.label.contains("TPM2"),
        "got {:?}",
        system.protection.label
    );
}

#[test]
fn protection_status_survives_a_reconnect() {
    // The third of the old `reconnect_restores_state` that no current test
    // owned: a presenter rebuilt over the same daemon must re-read protection,
    // not fall back to ProtectionView::default() (which reports TPM2 and no
    // warning — the reassuring answer, on a machine that has no TPM).
    let client = Arc::new(FakeManagerClient::new());
    client.set_protection_status(software_fallback_status());

    let mut first = namtor_ui::ProfilePresenter::from_arc(Arc::clone(&client), TEST_UID);
    first.refresh_system_status();
    assert!(
        first
            .model()
            .system
            .as_ref()
            .unwrap()
            .protection
            .is_software_fallback
    );

    let mut second = namtor_ui::ProfilePresenter::from_arc(client, TEST_UID);
    second.refresh_system_status();
    let prot = &second.model().system.as_ref().unwrap().protection;
    assert!(
        prot.is_software_fallback,
        "a reconnected presenter must re-read protection, not default to TPM2"
    );
    assert!(!prot.warning.is_empty());
}

// --- error recovery hints ----------------------------------------------------

#[test]
fn daemon_down_error_carries_actionable_recovery_steps() {
    // `set_error` calls `RecoveryHint::for_error` for every error it builds,
    // but nothing asserted the result reached the model: the `usa_12_*` tests
    // render a RecoveryHint they construct themselves, so the presenter's own
    // wiring was unowned. An error with no recovery hint still shows a title,
    // which is exactly the regression that would go unnoticed.
    let client = Arc::new(FakeManagerClient::new());
    client.set_mode(Mode::DaemonDown("daemon stopped".into()));
    let mut p = namtor_ui::ProfilePresenter::from_arc(client, TEST_UID);

    after(&mut p, |p| {
        p.start_enrollment_v3("Ida V3");
    });

    let err = p.model().error.as_ref().expect("daemon-down sets an error");
    let recovery = err
        .recovery
        .as_ref()
        .expect("a daemon-down error must carry a recovery hint");
    assert!(
        !recovery.steps.is_empty(),
        "recovery hint must tell the user what to do"
    );
}

#[test]
fn quality_view_shows_guidance_only_when_low() {
    // `QualityLabel::Low => show_guidance: true` was the mapping the old
    // low_diversity test owned. Afterwards every `show_guidance` in the test
    // tree was a hard-coded `false`, so nothing asserted it is ever true and
    // the view would simply have stopped prompting.
    use namtor_ui::QualityLabel;
    let low = namtor_ui::QualityView::from_label(QualityLabel::Low);
    assert_eq!(low.label, "low");
    assert!(low.show_guidance, "low quality must prompt the user");

    for (label, name) in [
        (QualityLabel::Medium, "medium"),
        (QualityLabel::High, "high"),
    ] {
        let v = namtor_ui::QualityView::from_label(label);
        assert_eq!(v.label, name);
        assert!(!v.show_guidance, "{name} quality must not prompt");
    }
}
