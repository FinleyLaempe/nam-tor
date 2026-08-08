// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests driving the GNOME presenter with a fake D-Bus service.
//!
//! These tests do not touch a real session bus or a real display server. The
//! presenter is the testable surface; the GTK view is a thin adapter over
//! [`namtor_gnome::PresenterModel`]. Each test names one user-visible
//! outcome.

mod common;

use common::after;
use namtor_gnome::{
    fake::{FakeManagerClient, Mode},
    ManagerClient, ManagerError, RecoveryHint,
};

fn new_presenter() -> namtor_gnome::ProfilePresenter<FakeManagerClient> {
    namtor_gnome::ProfilePresenter::new(FakeManagerClient::new())
}

fn enroll(p: &mut namtor_gnome::ProfilePresenter<FakeManagerClient>, name: &str) {
    p.create_profile(name);
    p.finish_enrollment();
}

/// Happy path: create → enroll → finish → list shows the new profile.
#[test]
fn happy_path_create_enroll_finish_lists_profile() {
    let mut presenter = new_presenter();

    let model = after(&mut presenter, |p| {
        p.refresh();
    });
    assert!(model.profiles.is_empty(), "no profiles at start");
    assert!(model.error.is_none(), "no error on fresh refresh");

    let model = after(&mut presenter, |p| {
        p.create_profile("Alice");
    });
    assert!(model.enrollment.is_some(), "enrollment view active");
    let enroll = model.enrollment.as_ref().expect("enrollment");
    assert_eq!(enroll.quality.label, "high", "first sample quality is high");
    assert!(!enroll.quality.show_guidance, "no guidance at high quality");

    let mut last_coverage = 0;
    for _ in 0..4 {
        let m = after(&mut presenter, |p| {
            p.poll_enrollment();
        });
        let e = m.enrollment.as_ref().expect("enrollment active");
        assert!(e.coverage_percent >= last_coverage, "coverage monotonic");
        last_coverage = e.coverage_percent;
    }
    assert!(last_coverage > 0, "coverage advanced");

    let model = after(&mut presenter, |p| {
        p.finish_enrollment();
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
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake);

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
        p.create_profile("Bob");
    });
    assert!(model.enrollment.is_some(), "enrollment started");

    let model = after(&mut presenter, |p| {
        p.cancel_enrollment();
    });
    assert!(model.enrollment.is_none(), "enrollment view cleared");
    assert!(model.message.as_deref().unwrap_or("").contains("cancel"));
    assert!(model.error.is_none(), "cancel is not an error");

    let list = presenter.client_mut().list_profiles().expect("list ok");
    assert!(list.is_empty(), "no profile persisted after cancel");
}

/// Polkit denied on create → authentication error shown.
#[test]
fn polkit_denied_shows_auth_error() {
    let fake = FakeManagerClient::new();
    fake.set_mode(Mode::PolkitDenied("enroll-other requires admin".into()));
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake);

    let model = after(&mut presenter, |p| {
        p.create_profile("Carol");
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
    let mut presenter = namtor_gnome::ProfilePresenter::new(fake);

    let _ = after(&mut presenter, |p| {
        p.create_profile("Dave");
    });
    let model = after(&mut presenter, |p| {
        p.poll_enrollment();
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
        p.finish_enrollment();
    });
    assert!(model.enrollment.is_none(), "enrollment cleared on finish");
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

/// `create_profile` with a duplicate name surfaces a conflict error, not a
/// crash.
#[test]
fn duplicate_profile_name_surfaces_conflict() {
    let mut presenter = new_presenter();
    enroll(&mut presenter, "Ivy");

    let model = after(&mut presenter, |p| {
        p.create_profile("Ivy");
    });
    let err = model.error.as_ref().expect("conflict surfaced");
    assert!(
        err.detail.to_lowercase().contains("conflict")
            || err.detail.to_lowercase().contains("already"),
        "conflict reason shown: {err:?}"
    );
    assert!(model.enrollment.is_none(), "no enrollment on conflict");
}
