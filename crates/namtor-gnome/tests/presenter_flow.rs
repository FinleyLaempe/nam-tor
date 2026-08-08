// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests driving the GNOME presenter with a fake D-Bus service.
//!
//! These tests do not touch a real system bus or a real display server. The
//! presenter is the testable surface; the GTK view is a thin adapter over
//! [`namtor_gnome::PresenterModel`]. Each test names one user-visible
//! outcome.

mod common;

use std::sync::Arc;

use common::after;
use namtor_gnome::{
    fake::{FakeManagerClient, Mode},
    ManagerClient, ManagerError, PamServiceRowView, ProtectionKeyKind, ProtectionStatus,
    RecoveryHint,
};

const TEST_UID: u32 = 1000;

fn new_presenter() -> namtor_gnome::ProfilePresenter<FakeManagerClient> {
    namtor_gnome::ProfilePresenter::new(FakeManagerClient::new(), TEST_UID)
}

fn enroll(p: &mut namtor_gnome::ProfilePresenter<FakeManagerClient>, name: &str) {
    p.start_enrollment(name);
    p.commit_profile();
}

/// Happy path: start → poll → commit → list shows the new profile.
#[test]
fn happy_path_start_enroll_commit_lists_profile() {
    let mut presenter = new_presenter();

    let model = after(&mut presenter, |p| {
        p.refresh();
    });
    assert!(model.profiles.is_empty(), "no profiles at start");
    assert!(model.error.is_none(), "no error on fresh refresh");

    let model = after(&mut presenter, |p| {
        p.start_enrollment("Alice");
    });
    assert!(model.enrollment.is_some(), "enrollment view active");
    let enroll = model.enrollment.as_ref().expect("enrollment");
    assert_eq!(enroll.quality.label, "high", "first sample quality is high");
    assert!(!enroll.quality.show_guidance, "no guidance at high quality");

    let mut last_coverage = 0;
    for _ in 0..4 {
        let m = after(&mut presenter, |p| {
            p.poll_progress();
        });
        let e = m.enrollment.as_ref().expect("enrollment active");
        assert!(e.coverage_percent >= last_coverage, "coverage monotonic");
        last_coverage = e.coverage_percent;
    }
    assert!(last_coverage > 0, "coverage advanced");

    let model = after(&mut presenter, |p| {
        p.commit_profile();
    });
    assert!(model.enrollment.is_none(), "enrollment cleared");
    assert!(model.message.as_deref().unwrap_or("").contains("complete"));
    assert_eq!(model.profiles.len(), 1, "one profile listed");
    assert_eq!(model.profiles[0].name, "Alice");
    assert!(model.profiles[0].enabled, "new profile enabled by default");
    assert!(model.error.is_none(), "no error on success");
}

/// Daemon unavailable → error shown with recovery guidance.
#[test]
fn daemon_unavailable_shows_error_and_recovery() {
    let fake = FakeManagerClient::new();
    fake.set_mode(Mode::DaemonDown("connection refused".into()));
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake, TEST_UID);

    let model = after(&mut presenter, |p| {
        p.refresh();
    });
    let err = model.error.as_ref().expect("error surfaced");
    assert!(
        err.title.to_lowercase().contains("daemon"),
        "title names daemon"
    );
    let recovery = err.recovery.as_ref().expect("recovery hint present");
    assert!(!recovery.steps.is_empty(), "recovery steps are actionable");
    assert!(
        recovery.title.to_lowercase().contains("not running")
            || recovery.title.to_lowercase().contains("daemon"),
        "recovery title is actionable"
    );
    assert!(model.profiles.is_empty(), "no profiles when daemon down");
}

/// Cancel mid-enrollment → cleanup, no orphan session, list still empty.
#[test]
fn cancel_mid_enrollment_cleans_up() {
    let mut presenter = new_presenter();

    let model = after(&mut presenter, |p| {
        p.start_enrollment("Bob");
    });
    assert!(model.enrollment.is_some(), "enrollment started");

    let model = after(&mut presenter, |p| {
        p.cancel_enrollment();
    });
    assert!(model.enrollment.is_none(), "enrollment view cleared");
    assert!(model.message.as_deref().unwrap_or("").contains("cancel"));
    assert!(model.error.is_none(), "cancel is not an error");

    let list = presenter.client().list_profiles().expect("list ok");
    assert!(list.is_empty(), "no profile persisted after cancel");
}

/// Polkit denied on start_enrollment → authentication error shown.
#[test]
fn polkit_denied_shows_auth_error() {
    let fake = FakeManagerClient::new();
    fake.set_mode(Mode::PolkitDenied("enroll-other requires admin".into()));
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake, TEST_UID);

    let model = after(&mut presenter, |p| {
        p.start_enrollment("Carol");
    });
    let err = model.error.as_ref().expect("auth error surfaced");
    assert!(
        err.title.to_lowercase().contains("auth") || err.title.to_lowercase().contains("denied"),
        "title names the auth failure"
    );
    let recovery = err.recovery.as_ref().expect("polkit recovery hint");
    assert!(
        recovery.title.to_lowercase().contains("admin"),
        "recovery names admin approval"
    );
    assert!(
        model.enrollment.is_none(),
        "no enrollment started on denial"
    );
}

/// Low-diversity capture → quality label "low" → guidance shown.
#[test]
fn low_diversity_shows_guidance() {
    let fake = FakeManagerClient::new();
    fake.set_mode(Mode::LowDiversity);
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake, TEST_UID);

    let _ = after(&mut presenter, |p| {
        p.start_enrollment("Dave");
    });
    let model = after(&mut presenter, |p| {
        p.poll_progress();
    });
    let enroll = model.enrollment.as_ref().expect("still enrolling");
    assert_eq!(enroll.quality.label, "low", "quality label is 'low'");
    assert!(
        enroll.quality.show_guidance,
        "guidance flag raised on low diversity"
    );
    assert!(
        enroll.coverage_percent < 100,
        "low-diversity session does not reach full coverage"
    );

    let model = after(&mut presenter, |p| {
        p.commit_profile();
    });
    assert!(
        model.enrollment.is_none(),
        "enrollment cleared on commit failure"
    );
    assert!(model.error.is_some(), "failure surfaced as error");
}

/// Merge confirmation flow: stage → confirm → src removed, dst kept.
#[test]
fn merge_profiles_requires_confirmation() {
    let mut presenter = new_presenter();
    enroll(&mut presenter, "Eve");
    enroll(&mut presenter, "Fay");
    let model = presenter.model().clone();
    assert_eq!(model.profiles.len(), 2, "two profiles seeded");
    let eve_id = model.profiles[0].id.clone();
    let fay_id = model.profiles[1].id.clone();

    let model = after(&mut presenter, |p| {
        p.request_merge(&eve_id, &fay_id);
    });
    let pending = model.pending_merge.as_ref().expect("merge staged");
    assert_eq!(pending.src_name, "Eve");
    assert_eq!(pending.dst_name, "Fay");
    assert_eq!(
        presenter.model().profiles.len(),
        2,
        "merge not applied before confirm"
    );

    let model = after(&mut presenter, |p| {
        p.confirm_merge();
    });
    assert!(model.pending_merge.is_none(), "pending cleared");
    assert!(model.message.as_deref().unwrap_or("").contains("Merged"));
    assert_eq!(model.profiles.len(), 1, "src merged away");
    assert_eq!(model.profiles[0].name, "Fay", "dst retained");
}

/// Rename + enable/disable + delete flow.
#[test]
fn rename_enable_disable_delete_flow() {
    let mut presenter = new_presenter();
    enroll(&mut presenter, "Gina");
    let id = presenter.model().profiles[0].id.clone();

    let model = after(&mut presenter, |p| {
        p.rename_profile(&id, "Georgia");
    });
    assert_eq!(model.profiles[0].name, "Georgia");

    let model = after(&mut presenter, |p| {
        p.enable_profile(&id, false);
    });
    assert!(!model.profiles[0].enabled, "disabled");

    let model = after(&mut presenter, |p| {
        p.enable_profile(&id, true);
    });
    assert!(model.profiles[0].enabled, "re-enabled");

    let model = after(&mut presenter, |p| {
        p.delete_profile(&id);
    });
    assert!(model.profiles.is_empty(), "deleted");
    assert!(model.message.as_deref().unwrap_or("").contains("deleted"));
}

/// Replace confirmation flow: stage → cancel (no mutation) → stage → confirm.
#[test]
fn replace_profile_requires_confirmation() {
    let mut presenter = new_presenter();
    enroll(&mut presenter, "Henry");
    let id = presenter.model().profiles[0].id.clone();
    let original_name = presenter.model().profiles[0].name.clone();

    let model = after(&mut presenter, |p| {
        p.request_replace(&id, "Henry II");
    });
    assert!(model.pending_replace.is_some(), "replace staged");
    let model = after(&mut presenter, |p| {
        p.cancel_replace();
    });
    assert!(model.pending_replace.is_none(), "replace cancelled");
    assert_eq!(
        presenter.model().profiles[0].name,
        original_name,
        "name unchanged after cancel"
    );

    let _ = after(&mut presenter, |p| {
        p.request_replace(&id, "Henry II");
    });
    let model = after(&mut presenter, |p| {
        p.confirm_replace();
    });
    assert!(model.pending_replace.is_none(), "replace confirmed");
    assert_eq!(model.profiles[0].name, "Henry II");
}

/// `ManagerError::ServiceNotRegistered` surfaces a distinct recovery hint.
#[test]
fn service_not_registered_has_recovery_hint() {
    let hint = RecoveryHint::for_error(&ManagerError::ServiceNotRegistered);
    assert!(hint.is_some(), "service-not-registered yields recovery");
    let hint = hint.expect("hint");
    assert!(hint.title.to_lowercase().contains("service"));
}

/// `start_enrollment` with a duplicate name surfaces an exists error.
#[test]
fn duplicate_profile_name_surfaces_exists() {
    let mut presenter = new_presenter();
    enroll(&mut presenter, "Ivy");

    let model = after(&mut presenter, |p| {
        p.start_enrollment("Ivy");
    });
    let err = model.error.as_ref().expect("exists error surfaced");
    assert!(
        err.detail.to_lowercase().contains("exists")
            || err.detail.to_lowercase().contains("already"),
        "exists reason shown: {err:?}"
    );
    assert!(model.enrollment.is_none(), "no enrollment on exists");
}

/// commit_profile actually calls the client (no-op removed): after commit,
/// the profile persists in the client's store.
#[test]
fn commit_profile_persists_to_store() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment("Jana");
    });
    after(&mut presenter, |p| {
        p.commit_profile();
    });
    assert!(
        presenter.client().has_profile("Jana"),
        "commit_profile persisted 'Jana' to the store"
    );
}

/// Enrollment persists across reconnect: a new presenter backed by the same
/// fake sees the profile after "reconnecting".
#[test]
fn enrollment_persists_across_reconnect() {
    let fake = Arc::new(FakeManagerClient::new());
    let mut presenter = namtor_gnome::ProfilePresenter::from_arc(Arc::clone(&fake), TEST_UID);
    after(&mut presenter, |p| {
        p.start_enrollment("Karl");
    });
    after(&mut presenter, |p| {
        p.commit_profile();
    });
    assert_eq!(
        presenter.model().profiles.len(),
        1,
        "one profile after commit"
    );

    let mut presenter2 = namtor_gnome::ProfilePresenter::from_arc(Arc::clone(&fake), TEST_UID);
    let model = after(&mut presenter2, |p| {
        p.refresh();
    });
    assert_eq!(model.profiles.len(), 1, "profile survives reconnect");
    assert_eq!(model.profiles[0].name, "Karl");
}

/// Reconnect restores full state: profiles, status, PAM services, protection.
#[test]
fn reconnect_restores_state() {
    let fake = Arc::new(FakeManagerClient::new());
    fake.set_protection_status(ProtectionStatus {
        sealing: ProtectionKeyKind::Tpm2,
        has_tpm: true,
        warning: String::new(),
    });

    let mut presenter = namtor_gnome::ProfilePresenter::from_arc(Arc::clone(&fake), TEST_UID);
    after(&mut presenter, |p| {
        p.start_enrollment("Lena");
    });
    after(&mut presenter, |p| {
        p.commit_profile();
    });

    fake.set_mode(Mode::DaemonDown("daemon stopped".into()));
    let model = after(&mut presenter, |p| {
        p.refresh();
    });
    assert!(model.error.is_some(), "error when daemon down");

    fake.set_mode(Mode::Normal);
    let mut presenter2 = namtor_gnome::ProfilePresenter::from_arc(Arc::clone(&fake), TEST_UID);
    let model = after(&mut presenter2, |p| {
        p.refresh();
    });
    assert!(model.error.is_none(), "no error after reconnect");
    assert_eq!(model.profiles.len(), 1, "profiles restored");
    assert_eq!(model.profiles[0].name, "Lena");
    assert!(!model.pam_services.is_empty(), "PAM services restored");
    assert!(model.protection.is_some(), "protection status restored");
    let prot = model.protection.as_ref().expect("protection");
    assert!(prot.label.contains("TPM2"), "TPM2 sealing restored");
    assert!(!prot.is_software_fallback);
}

/// v2: PAM service toggle works through the presenter.
#[test]
fn pam_service_toggle_works() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.refresh();
    });
    let services = presenter.model().pam_services.clone();
    assert!(!services.is_empty(), "PAM services listed");

    let gdm = services
        .iter()
        .find(|s| s.id == "gdm-password")
        .expect("gdm-password service present");
    assert!(!gdm.enabled, "gdm-password starts disabled");

    let model = after(&mut presenter, |p| {
        p.set_pam_service_enabled("gdm-password", true);
    });
    assert!(
        model.message.as_deref().unwrap_or("").contains("enabled"),
        "toggle success message"
    );
    let gdm = model
        .pam_services
        .iter()
        .find(|s| s.id == "gdm-password")
        .expect("gdm-password after toggle");
    assert!(gdm.enabled, "gdm-password now enabled");
}

/// v2: Protection status is surfaced in the model as a privacy-safe view.
#[test]
fn protection_status_surfaced() {
    let fake = FakeManagerClient::new();
    fake.set_protection_status(ProtectionStatus {
        sealing: ProtectionKeyKind::Tpm2,
        has_tpm: true,
        warning: String::new(),
    });
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake, TEST_UID);
    let model = after(&mut presenter, |p| {
        p.refresh();
    });
    let prot = model.protection.as_ref().expect("protection status");
    assert!(prot.label.contains("TPM2"), "label reflects TPM2 sealing");
    assert!(!prot.is_software_fallback, "not a software fallback");
    assert!(prot.warning.is_empty(), "no warning when hardware-bound");
}

/// v2: Software fallback protection surfaces a warning and the fallback flag.
#[test]
fn software_fallback_protection_surfaced() {
    let fake = FakeManagerClient::new();
    fake.set_protection_status(ProtectionStatus {
        sealing: ProtectionKeyKind::Software,
        has_tpm: false,
        warning: "TPM2 not available; using software fallback".into(),
    });
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake, TEST_UID);
    let model = after(&mut presenter, |p| {
        p.refresh();
    });
    let prot = model.protection.as_ref().expect("protection status");
    assert!(prot.is_software_fallback, "software fallback flagged");
    assert!(!prot.warning.is_empty(), "warning shown for fallback");
    assert!(
        prot.label.contains("Software"),
        "label reflects software sealing"
    );
}

/// v2: Readiness view is populated after refresh with daemon running.
#[test]
fn readiness_view_populated_when_daemon_running() {
    let mut presenter = new_presenter();
    let model = after(&mut presenter, |p| {
        p.refresh();
    });
    let readiness = model.readiness.as_ref().expect("readiness view");
    assert_eq!(readiness.daemon, namtor_gnome::ReadinessLevel::Ready);
    assert_eq!(readiness.model, namtor_gnome::ReadinessLevel::Ready);
    assert_eq!(readiness.camera, namtor_gnome::ReadinessLevel::Ready);
    assert!(!readiness.has_enrolled_profiles, "no profiles at start");
    assert_eq!(readiness.enabled_pam_count, 0, "no PAM services enabled");
}

/// v2: Readiness view shows NotReady when daemon is down.
#[test]
fn readiness_view_when_daemon_down() {
    let fake = FakeManagerClient::new();
    fake.set_mode(Mode::DaemonDown("daemon stopped".into()));
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake, TEST_UID);
    let model = after(&mut presenter, |p| {
        p.refresh();
    });
    let readiness = model.readiness.as_ref().expect("readiness view");
    assert_eq!(readiness.daemon, namtor_gnome::ReadinessLevel::NotReady);
    assert_eq!(readiness.model, namtor_gnome::ReadinessLevel::Unknown);
    assert_eq!(readiness.camera, namtor_gnome::ReadinessLevel::Unknown);
}

/// v2: Readiness view reflects enrolled profiles and enabled PAM count.
#[test]
fn readiness_view_counts_profiles_and_pam() {
    let mut presenter = new_presenter();
    after(&mut presenter, |p| {
        p.start_enrollment("Alice");
    });
    after(&mut presenter, |p| {
        p.commit_profile();
    });
    after(&mut presenter, |p| {
        p.set_pam_service_enabled("gdm-password", true);
    });
    let model = presenter.model().clone();
    let readiness = model.readiness.as_ref().expect("readiness view");
    assert!(readiness.has_enrolled_profiles, "profile enrolled");
    assert_eq!(readiness.enabled_pam_count, 1, "one PAM service enabled");
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
