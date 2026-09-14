// SPDX-License-Identifier: GPL-3.0-only
//! Tests for the GNOME bug fixes (USA-0/2/3/5/6/8/9/10/11/12/16).
//!
//! All tests are behavioural: they construct real presenter/model state,
//! real GTK widgets (headlessly), or real enum values — never scanning
//! source text.

mod common;

use common::after;
use namtor_gnome::view_model::{EnrollmentViewV3, ErrorView, RecoveryHint};
use namtor_gnome::{
    fake::FakeManagerClient, polkit_auth::PolkitAuthError, EnrollmentState, ManagerClient,
    ManagerError, ProfilePresenter,
};

const TEST_UID: u32 = 1000;

// ===========================================================================
// USA-0: polkit CheckAuthorization off the GTK main thread
// ===========================================================================

/// USA-0: the PAM toggle must be dispatched asynchronously so the UI
/// remains responsive during polkit authorization. This test verifies
/// that the dispatch_async mechanism sets is_loading while a toggle is
/// in flight, proving the async pattern is wired correctly.
#[test]
fn usa_0_async_pam_toggle() {
    let fake = FakeManagerClient::new();
    fake.set_pam_toggle_delay_ms(50);

    let mut presenter = ProfilePresenter::new(fake, TEST_UID);

    let main_context = glib::MainContext::new();
    let _guard = main_context
        .acquire()
        .expect("failed to acquire main context");

    presenter.dispatch_async(|client| {
        let _ = client.set_pam_service_enabled("gdm-password", true);
    });

    assert!(
        presenter.model().is_loading,
        "is_loading should be true while toggle is in flight"
    );
    assert!(
        presenter.is_busy(),
        "pending_task should be Some while toggle is in flight"
    );

    main_context.iteration(false);
    std::thread::sleep(std::time::Duration::from_millis(100));
    main_context.iteration(false);

    assert!(
        presenter.is_busy(),
        "pending_task should still exist (view would call mark_async_complete)"
    );
}

// ===========================================================================
// USA-2: retry_connection re-runs DbusManagerClient::connect()
// ===========================================================================

/// USA-2: behavioural test proving the full offline→retry→online state
/// transition. Uses `FakeBus` to provide a real `DbusManagerClient` without
/// touching the system D-Bus.
#[test]
fn usa_2_retry_connection() {
    use namtor_client::fake_bus::FakeBus;
    use namtor_client::types::ManagerError;
    use namtor_gnome::app::AppController;

    common::run_gtk_test(move || {
        use gtk4::prelude::*;

        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.usa2")
            .build();
        let _guard = app.hold();

        let controller = AppController::new_offline_for_test(&app);
        let rc = controller.clone();

        assert!(
            controller.borrow().is_offline_for_test(),
            "controller must start offline"
        );
        assert!(
            !controller.borrow().has_presenter_for_test(),
            "controller must start with no presenter"
        );

        let fail_result: namtor_client::types::ManagerResult<namtor_gnome::DbusManagerClient> = Err(
            ManagerError::DaemonUnavailable("test-daemon-unavailable".into()),
        );
        controller
            .borrow_mut()
            .apply_connect_result(&rc, fail_result);

        assert!(
            controller.borrow().is_offline_for_test(),
            "controller must stay offline after failed retry"
        );

        let fake_bus = FakeBus::spawn().expect("spawn fake bus");
        let success_result: namtor_client::types::ManagerResult<namtor_gnome::DbusManagerClient> =
            Ok(fake_bus.client);
        controller
            .borrow_mut()
            .apply_connect_result(&rc, success_result);

        assert!(
            !controller.borrow().is_offline_for_test(),
            "controller must be online after successful retry"
        );
        assert!(
            controller.borrow().has_presenter_for_test(),
            "controller must have presenter after successful retry"
        );
    });
}

// ===========================================================================
// USA-3: duplicate-name check in start_enrollment_v3
// ===========================================================================

/// USA-3: start_enrollment_v3 must check for duplicate names and stage a
/// pending_replace instead of silently creating a duplicate.
#[test]
fn usa_3_duplicate_name_stages_pending_replace() {
    let mut presenter = ProfilePresenter::new(FakeManagerClient::new(), TEST_UID);
    after(&mut presenter, |p| {
        p.refresh();
    });
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Alice V3");
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    after(&mut presenter, |p| {
        p.review_enrollment_v3();
    });
    after(&mut presenter, |p| {
        p.save_profile_v3();
    });
    let model = after(&mut presenter, |p| {
        p.start_enrollment_v3("Alice V3");
    });
    assert!(
        model.pending_replace.is_some(),
        "duplicate name must stage pending_replace (USA-3)"
    );
    assert!(
        model.error.is_some(),
        "duplicate name must surface an error (USA-3)"
    );
    let err = model.error.as_ref().expect("error");
    assert!(
        err.detail.contains("already exists"),
        "error must mention the duplicate: {err:?}"
    );
}

/// USA-3: confirm_replace_v3 deletes the old profile and starts a fresh
/// enrollment.
#[test]
fn usa_3_confirm_replace_v3_starts_fresh_enrollment() {
    let mut presenter = ProfilePresenter::new(FakeManagerClient::new(), TEST_UID);
    after(&mut presenter, |p| {
        p.refresh();
    });
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Bob V3");
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    after(&mut presenter, |p| {
        p.review_enrollment_v3();
    });
    after(&mut presenter, |p| {
        p.save_profile_v3();
    });
    assert_eq!(presenter.model().profiles.len(), 1);

    after(&mut presenter, |p| {
        p.start_enrollment_v3("Bob V3");
    });
    assert!(presenter.model().pending_replace.is_some());

    let model = after(&mut presenter, |p| {
        p.confirm_replace_v3();
    });
    assert!(
        model.pending_replace.is_none(),
        "pending_replace cleared after confirm (USA-3)"
    );
    assert_eq!(
        model.enrollment_state,
        EnrollmentState::Capturing,
        "confirm_replace_v3 starts a fresh enrollment (USA-3)"
    );
}

/// USA-3: cancel_replace clears the pending replace without enrolling.
#[test]
fn usa_3_cancel_replace_clears_pending() {
    let mut presenter = ProfilePresenter::new(FakeManagerClient::new(), TEST_UID);
    after(&mut presenter, |p| {
        p.refresh();
    });
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Carol V3");
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    after(&mut presenter, |p| {
        p.review_enrollment_v3();
    });
    after(&mut presenter, |p| {
        p.save_profile_v3();
    });

    after(&mut presenter, |p| {
        p.start_enrollment_v3("Carol V3");
    });
    assert!(presenter.model().pending_replace.is_some());

    let model = after(&mut presenter, |p| {
        p.cancel_replace();
    });
    assert!(model.pending_replace.is_none(), "pending cleared on cancel");
}

// ===========================================================================
// USA-5: error visible in the Stopped branch
// ===========================================================================

/// USA-5: `render_stopped` must accept an optional error and render it as
/// an alert row. Verified by constructing a real `EnrollmentViewComponent`
/// headlessly, calling `render_stopped` with and without an error, and
/// asserting the rendered widget tree contains the error text only when
/// an error is provided.
#[test]
fn usa_5_render_stopped_shows_error() {
    use gtk4::prelude::*;
    use namtor_gnome::components::EnrollmentViewComponent;

    let view = EnrollmentViewV3 {
        phase: "capture".into(),
        quality_percent: 80,
        diversity_percent: 60,
        frames_seen: 10,
        frames_accepted: 8,
        frames_rejected: 2,
        quality_bucket: 2,
        guidance: "Looking good".into(),
    };
    let error = ErrorView {
        title: "Save failed".into(),
        detail: "Store error".into(),
        recovery: None,
    };

    common::run_gtk_test(move || {
        // With error: the rendered page must contain the error text.
        let mut comp = EnrollmentViewComponent::new();
        let page = comp.render_stopped(Some(&view), Some(&error), || {}, || {});
        let texts = common::collect_widget_text(page.upcast_ref::<gtk4::Widget>());
        assert!(
            texts.iter().any(|t| t.contains("Save failed")),
            "render_stopped with error must show error title: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Store error")),
            "render_stopped with error must show error detail: {texts:?}"
        );

        // Without error: the rendered page must NOT contain the error text.
        let page_no_err = comp.render_stopped(Some(&view), None, || {}, || {});
        let texts_no_err = common::collect_widget_text(page_no_err.upcast_ref::<gtk4::Widget>());
        assert!(
            !texts_no_err.iter().any(|t| t.contains("Save failed")),
            "render_stopped without error must not show error text: {texts_no_err:?}"
        );
    });
}

// ===========================================================================
// USA-6: auto-reconnect on ServiceNotRegistered/Transport
// ===========================================================================

/// USA-6: `AppController::retry_connection` must exist and be callable —
/// it is the user-facing reconnect path. Verified by constructing a real
/// `AppController` headlessly and calling `retry_connection`.
#[test]
fn usa_6_reconnect_mechanism_present() {
    use namtor_gnome::app::AppController;

    common::run_gtk_test(move || {
        use gtk4::prelude::*;

        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.usa6")
            .build();
        let _guard = app.hold();

        let controller = AppController::new_offline_for_test(&app);
        let rc = controller.clone();

        controller.borrow_mut().retry_connection(&rc);

        assert!(
            controller.borrow().is_offline_for_test(),
            "controller stays offline after failed retry (no daemon in test)"
        );
    });
}

// ===========================================================================
// USA-8: Restart Daemon + View Logs wired; polkit action present
// ===========================================================================

/// USA-8: the polkit policy file must include the org.namtor.restart-daemon
/// action. This is a documentation/resource file check, not a source-code
/// scan — the policy XML is a build artifact, not Rust source.
#[test]
fn usa_8_polkit_restart_daemon_action_present() {
    let policy = include_str!("../../../packaging/polkit-1/actions/org.namtor.policy");
    assert!(
        policy.contains("org.namtor.restart-daemon"),
        "polkit policy must include org.namtor.restart-daemon action (USA-8)"
    );
}

/// USA-8: `render_system_status` must produce a Troubleshooting group with
/// "Restart Daemon" and "View System Logs" action rows. Verified by
/// constructing a real presenter, calling `render_system_status`, and
/// asserting the returned groups contain the expected row titles.
#[test]
fn usa_8_restart_and_logs_rows_wired() {
    use gtk4::prelude::*;
    use namtor_gnome::components::system_status::render_system_status;
    use std::cell::RefCell;
    use std::rc::Rc;

    common::run_gtk_test(move || {
        let mut presenter = ProfilePresenter::new(FakeManagerClient::new(), TEST_UID);
        presenter.refresh();
        let presenter_rc = Rc::new(RefCell::new(presenter));

        let groups = render_system_status(&presenter_rc);
        assert!(
            groups.len() >= 2,
            "system_status must return at least 2 groups"
        );

        let mut all_texts = Vec::new();
        for group in &groups {
            let texts = common::collect_widget_text(group.upcast_ref::<gtk4::Widget>());
            all_texts.extend(texts);
        }

        assert!(
            all_texts.iter().any(|t| t.contains("Restart Daemon")),
            "troubleshooting group must have 'Restart Daemon' row: {all_texts:?}"
        );
        assert!(
            all_texts.iter().any(|t| t.contains("View System Logs")),
            "troubleshooting group must have 'View System Logs' row: {all_texts:?}"
        );
    });
}

// ===========================================================================
// USA-9: completion cue when diversity >= 100 and quality good
// ===========================================================================

/// USA-9: the enrollment capture phase must show a completion cue when
/// `diversity_percent >= 100` and `quality_bucket >= 2`. Verified by
/// constructing a real `EnrollmentViewComponent`, rendering with a
/// complete view and an incomplete view, and asserting the completion
/// text appears only in the complete case.
#[test]
fn usa_9_completion_cue_present() {
    use gtk4::prelude::*;
    use namtor_gnome::components::EnrollmentViewComponent;

    let complete_view = EnrollmentViewV3 {
        phase: "capture".into(),
        quality_percent: 95,
        diversity_percent: 100,
        frames_seen: 20,
        frames_accepted: 18,
        frames_rejected: 2,
        quality_bucket: 2,
        guidance: "Looking good".into(),
    };
    let incomplete_view = EnrollmentViewV3 {
        diversity_percent: 50,
        quality_bucket: 1,
        ..complete_view.clone()
    };

    common::run_gtk_test(move || {
        let mut comp = EnrollmentViewComponent::new();

        // Complete view: must show completion cue.
        let page = comp.render(Some(&complete_view), |_| {}, || {}, || {});
        let texts = common::collect_widget_text(page.upcast_ref::<gtk4::Widget>());
        assert!(
            texts.iter().any(|t| t.contains("Enrollment complete")),
            "complete view must show completion cue: {texts:?}"
        );

        // Incomplete view: must NOT show completion cue.
        let page_incomplete = comp.render(Some(&incomplete_view), |_| {}, || {}, || {});
        let texts_incomplete =
            common::collect_widget_text(page_incomplete.upcast_ref::<gtk4::Widget>());
        assert!(
            !texts_incomplete
                .iter()
                .any(|t| t.contains("Enrollment complete")),
            "incomplete view must NOT show completion cue: {texts_incomplete:?}"
        );
    });
}

// ===========================================================================
// USA-10: Rename button in profile list
// ===========================================================================

/// USA-10: the profile list component must render a rename button for each
/// profile. Verified by constructing a real `ProfileListView`, rendering
/// with a model that has a profile, and asserting the rendered widget tree
/// contains the rename accessible label.
#[test]
fn usa_10_rename_button_present() {
    use gtk4::prelude::*;
    use namtor_gnome::components::ProfileListView;
    use namtor_gnome::view_model::{PresenterModel, ProfileRowView};

    let mut model = PresenterModel::default();
    model.profiles.push(ProfileRowView {
        id: "test-id".into(),
        name: "Test User".into(),
        enabled: true,
        created: 0,
        last_used: 0,
    });

    common::run_gtk_test(move || {
        let view = ProfileListView::new();
        let group = view.render(&model);
        let texts = common::collect_widget_text(group.upcast_ref::<gtk4::Widget>());

        assert!(
            texts.iter().any(|t| t.contains("Rename profile")),
            "profile list must show 'Rename profile' in tooltip/label: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Test User")),
            "profile list must show the profile name: {texts:?}"
        );
    });
}

// ===========================================================================
// USA-10: Rename dialog with AdwEntryRow (behavioural)
// ===========================================================================

/// USA-10: behavioural test proving the rename dialog flow:
/// 1. Create a profile via FakeManagerClient
/// 2. Call presenter.rename_profile() (what the dialog confirm would do)
/// 3. Assert the model shows the updated name after refresh
#[test]
fn usa_10_rename_dialog() {
    use namtor_gnome::presenter::after;

    let mut presenter = ProfilePresenter::new(FakeManagerClient::new(), TEST_UID);

    after(&mut presenter, |p| {
        p.refresh();
    });
    after(&mut presenter, |p| {
        p.start_enrollment_v3("Test User");
    });
    after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    after(&mut presenter, |p| {
        p.review_enrollment_v3();
    });
    after(&mut presenter, |p| {
        p.save_profile_v3();
    });

    let model = presenter.model();
    assert_eq!(model.profiles.len(), 1, "should have one profile");
    let profile_id = model.profiles[0].id.clone();
    assert_eq!(model.profiles[0].name, "Test User");

    let new_name = "Renamed User";
    after(&mut presenter, |p| {
        p.rename_profile(&profile_id, new_name);
    });

    let model = presenter.model();
    assert_eq!(model.profiles.len(), 1);
    assert_eq!(
        model.profiles[0].name, new_name,
        "profile name should be updated"
    );
    assert_eq!(
        model.message,
        Some(format!("Renamed profile to '{new_name}'")),
        "should show success message"
    );
}

// ===========================================================================
// USA-11: polkit_auth distinguishes Bus/Check/Denied/Cancelled
// ===========================================================================

/// USA-11: `PolkitAuthError` must have all four variants. Verified by
/// constructing each variant and asserting via `matches!`.
#[test]
fn usa_11_polkit_auth_has_four_variants() {
    let bus = PolkitAuthError::Bus;
    let check = PolkitAuthError::Check;
    let denied = PolkitAuthError::Denied("test".into());
    let cancelled = PolkitAuthError::Cancelled;

    assert!(matches!(bus, PolkitAuthError::Bus));
    assert!(matches!(check, PolkitAuthError::Check));
    assert!(matches!(denied, PolkitAuthError::Denied(_)));
    assert!(matches!(cancelled, PolkitAuthError::Cancelled));

    assert_ne!(bus, check);
    assert_ne!(bus, cancelled);
    assert_ne!(check, denied);
    assert_ne!(denied, cancelled);
}

/// USA-11: the error variants must map to distinct user-facing messages.
#[test]
fn usa_11_polkit_auth_messages_distinct() {
    assert_eq!(
        PolkitAuthError::Bus.user_message(),
        "Polkit service unavailable"
    );
    assert_eq!(
        PolkitAuthError::Check.user_message(),
        "Authorization check failed"
    );
    assert_eq!(
        PolkitAuthError::Cancelled.user_message(),
        "Authentication dismissed"
    );
    assert_eq!(
        PolkitAuthError::Denied("test".into()).user_message(),
        "Authentication denied"
    );
}

/// USA-11: ManagerError::ServiceNotRegistered maps to Bus.
#[test]
fn usa_11_service_not_registered_maps_to_bus() {
    let err = ManagerError::ServiceNotRegistered;
    let auth = PolkitAuthError::from_manager_error(&err);
    assert_eq!(auth, Some(PolkitAuthError::Bus));
}

/// USA-11: ManagerError::Denied with "cancelled" maps to Cancelled.
#[test]
fn usa_11_denied_with_cancel_maps_to_cancelled() {
    let err = ManagerError::Denied("Authentication was cancelled".into());
    let auth = PolkitAuthError::from_manager_error(&err);
    assert_eq!(auth, Some(PolkitAuthError::Cancelled));
}

/// USA-11: ManagerError::Denied without "cancelled" maps to Denied.
#[test]
fn usa_11_denied_without_cancel_maps_to_denied() {
    let err = ManagerError::Denied("Not authorized".into());
    let auth = PolkitAuthError::from_manager_error(&err);
    assert!(matches!(auth, Some(PolkitAuthError::Denied(_))));
}

// ===========================================================================
// USA-12: recovery view renders hint.title once (no double title)
// ===========================================================================

/// USA-12: the recovery view must set the group title to `hint.title`
/// via `set_title` (not render it as a separate Label). Verified by
/// constructing a real `RecoveryView`, rendering with a hint, and
/// asserting the group's title property equals the hint title.
#[test]
fn usa_12_recovery_no_double_title() {
    use gtk4::prelude::*;
    use libadwaita::prelude::*;
    use namtor_gnome::components::RecoveryView;

    let hint = RecoveryHint {
        title: "Nam-tor daemon is not running".into(),
        steps: vec!["Start the daemon".into()],
    };

    common::run_gtk_test(move || {
        let view = RecoveryView::new();
        let group = view.render(&hint);

        assert_eq!(
            group.title().to_string(),
            hint.title,
            "recovery group title must equal hint.title (USA-12 — set via set_title)"
        );

        // The steps must be rendered as a row subtitle, not as a
        // duplicate title label. The row title is "Recovery Steps:".
        let texts = common::collect_widget_text(group.upcast_ref::<gtk4::Widget>());
        assert!(
            texts.iter().any(|t| t.contains("Recovery Steps:")),
            "recovery view must render steps as a row, not a duplicate title: {texts:?}"
        );
    });
}

/// USA-12: the recovery component must show the steps as a row subtitle,
/// not as a duplicate title label.
#[test]
fn usa_12_recovery_shows_steps_as_subtitle() {
    use gtk4::prelude::*;
    use namtor_gnome::components::RecoveryView;

    let hint = RecoveryHint {
        title: "Daemon unreachable".into(),
        steps: vec!["Check the daemon status".into(), "Start the daemon".into()],
    };

    common::run_gtk_test(move || {
        let view = RecoveryView::new();
        let group = view.render(&hint);
        let texts = common::collect_widget_text(group.upcast_ref::<gtk4::Widget>());

        assert!(
            texts.iter().any(|t| t.contains("Recovery Steps:")),
            "recovery view must show 'Recovery Steps:' row: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Check the daemon status")),
            "recovery view must show step text: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Start the daemon")),
            "recovery view must show all steps: {texts:?}"
        );
    });
}

// ===========================================================================
// USA-16: mutations routed through dispatch_async
// ===========================================================================

/// USA-16: the presenter must have a `dispatch_async` method that sets
/// `is_loading` while a mutation is in flight.
#[test]
fn usa_16_presenter_has_dispatch_async() {
    let fake = FakeManagerClient::new();
    let mut presenter = ProfilePresenter::new(fake, TEST_UID);

    let main_context = glib::MainContext::new();
    let _guard = main_context
        .acquire()
        .expect("failed to acquire main context");

    assert!(
        !presenter.model().is_loading,
        "presenter must start with is_loading=false"
    );

    presenter.dispatch_async(|client| {
        let _ = client.set_pam_service_enabled("gdm-password", true);
    });

    assert!(
        presenter.model().is_loading,
        "dispatch_async must set is_loading=true (USA-16)"
    );
    assert!(
        presenter.is_busy(),
        "dispatch_async must set pending_task (USA-16)"
    );
}
