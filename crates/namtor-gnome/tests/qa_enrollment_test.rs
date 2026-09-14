// SPDX-License-Identifier: GPL-3.0-only
//! F3 QA: Full enrollment flow end-to-end via FakeBus (real p2p D-Bus).
//!
//! Constructs a real ProfilePresenter wired to a real DbusManagerClient via
//! FakeBus (a hermetic p2p D-Bus service, NOT a mock trait object), then
//! drives the full enrollment flow:
//!   start_enrollment -> poll_progress -> request_review -> commit_profile
//!
//! FakeBus implements the v1 org.namtor.Manager interface. The v2/v3
//! interfaces (Manager2, Manager3) are not implemented, so refresh() after
//! commit will set a connection error for PAM/protection queries — but the
//! enrollment flow itself (the critical path) completes without panic.
//!
//! No display server required — GTK is initialised headlessly on a dedicated
//! thread.

mod common;

use namtor_client::fake_bus::FakeBus;
use namtor_gnome::presenter::ProfilePresenter;

const TEST_UID: u32 = 1000;

/// Full enrollment flow: start -> poll -> review -> commit.
/// Verifies no panic occurs at any step and the enrollment state machine
/// transitions correctly.
#[test]
fn fakebus_full_enrollment_flow_no_panic() {
    common::run_gtk_test(|| {
        let fake_bus = FakeBus::spawn().expect("FakeBus::spawn failed");
        let mut presenter = ProfilePresenter::new(fake_bus.client, TEST_UID);

        // 1. Start enrollment — calls StartEnrollment on the fake D-Bus service.
        let model = presenter.start_enrollment("QA Test User");
        assert!(
            model.enrollment.is_some(),
            "enrollment should be started after start_enrollment"
        );
        assert!(
            model.error.is_none(),
            "no error expected on start_enrollment, got: {:?}",
            model.error
        );

        let enrollment = model.enrollment.as_ref().expect("enrollment view");
        assert!(
            !enrollment.session_id.is_empty(),
            "session_id should be non-empty"
        );

        // 2. Poll progress — calls PollProgress on the fake D-Bus service.
        let model = presenter.poll_progress();
        assert!(
            model.error.is_none(),
            "no error expected on poll_progress, got: {:?}",
            model.error
        );

        // 3. Request review — stages a review view.
        let model = presenter.request_review();
        assert!(
            model.review.is_some(),
            "review should be staged after request_review"
        );
        assert!(
            model.error.is_none(),
            "no error expected on request_review, got: {:?}",
            model.error
        );

        let review = model.review.as_ref().expect("review view");
        assert!(
            !review.profile_id.is_empty(),
            "review profile_id should be non-empty"
        );

        // 4. Commit profile — calls CommitProfile on the fake D-Bus service.
        //    commit_profile internally calls refresh() which queries v2
        //    interfaces (Manager2) not implemented by FakeBus. The commit
        //    itself succeeds; the error (if any) is from the post-commit
        //    refresh of PAM/protection data.
        let model = presenter.commit_profile();
        assert!(
            model.enrollment.is_none(),
            "enrollment should be cleared after commit (got: {:?})",
            model.enrollment
        );
        assert!(
            model.review.is_none(),
            "review should be cleared after commit"
        );

        // The commit itself succeeded — verify the profile was persisted
        // by checking the profile list (v1 ListProfiles, which FakeBus supports).
        assert_eq!(
            model.profiles.len(),
            1,
            "exactly one profile should be persisted after commit (got: {:?})",
            model.profiles
        );

        // If we reached here, no panic occurred at any step of the enrollment flow.
        println!("FAKEBUS_ENROLLMENT_FLOW_OK: start->poll->review->commit completed without panic");
    });
}

/// Enrollment + delete flow: enroll, commit, then delete the profile.
#[test]
fn fakebus_enrollment_then_delete_no_panic() {
    common::run_gtk_test(|| {
        let fake_bus = FakeBus::spawn().expect("FakeBus::spawn failed");
        let mut presenter = ProfilePresenter::new(fake_bus.client, TEST_UID);

        // Enroll
        presenter.start_enrollment("Delete Me");
        assert!(presenter.model().enrollment.is_some());

        // Commit
        let model = presenter.commit_profile();
        assert!(model.enrollment.is_none());
        assert_eq!(model.profiles.len(), 1);

        // Delete the profile
        let profile_id = model.profiles[0].id.clone();
        let model = presenter.delete_profile(&profile_id);
        assert_eq!(model.profiles.len(), 0, "profile should be deleted");

        println!("FAKEBUS_ENROLLMENT_DELETE_OK: enroll->commit->delete completed without panic");
    });
}

/// Multiple sequential enrollments: verify no state corruption or panic
/// across three enrollment cycles.
#[test]
fn fakebus_multiple_enrollments_no_panic() {
    common::run_gtk_test(|| {
        let fake_bus = FakeBus::spawn().expect("FakeBus::spawn failed");
        let mut presenter = ProfilePresenter::new(fake_bus.client, TEST_UID);

        for name in &["Alice", "Bob", "Carol"] {
            presenter.start_enrollment(name);
            assert!(
                presenter.model().enrollment.is_some(),
                "enrollment started for {name}"
            );
            presenter.request_review();
            assert!(
                presenter.model().review.is_some(),
                "review staged for {name}"
            );
            let model = presenter.commit_profile();
            assert!(model.enrollment.is_none(), "enrollment cleared for {name}");
        }

        let model = presenter.refresh();
        assert_eq!(
            model.profiles.len(),
            3,
            "three profiles should be persisted"
        );

        println!("FAKEBUS_MULTI_ENROLLMENT_OK: 3 sequential enrollments completed without panic");
    });
}

/// Error recovery: starting enrollment with an empty name should not panic
/// and should set an error view.
#[test]
fn fakebus_empty_name_no_panic() {
    common::run_gtk_test(|| {
        let fake_bus = FakeBus::spawn().expect("FakeBus::spawn failed");
        let mut presenter = ProfilePresenter::new(fake_bus.client, TEST_UID);

        let model = presenter.start_enrollment("");
        assert!(model.enrollment.is_none(), "no enrollment for empty name");
        assert!(model.error.is_some(), "error expected for empty name");

        // Recovery: a valid name should work after the error.
        let model = presenter.start_enrollment("Valid Name");
        assert!(
            model.enrollment.is_some(),
            "enrollment should work after error recovery"
        );
        assert!(model.error.is_none(), "error should be cleared");

        println!("FAKEBUS_ERROR_RECOVERY_OK: empty name error + recovery completed without panic");
    });
}

/// Cancel enrollment: start, then cancel, verify no panic.
#[test]
fn fakebus_cancel_enrollment_no_panic() {
    common::run_gtk_test(|| {
        let fake_bus = FakeBus::spawn().expect("FakeBus::spawn failed");
        let mut presenter = ProfilePresenter::new(fake_bus.client, TEST_UID);

        presenter.start_enrollment("Cancel Me");
        assert!(presenter.model().enrollment.is_some());

        // Cancel should not panic
        presenter.cancel_pending();
        assert!(
            !presenter.model().is_loading,
            "loading should be cleared after cancel"
        );

        println!("FAKEBUS_CANCEL_OK: start->cancel completed without panic");
    });
}
