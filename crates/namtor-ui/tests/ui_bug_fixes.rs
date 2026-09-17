// SPDX-License-Identifier: GPL-3.0-only
//! Tests for the UI bug fixes (USA-0/2/3/5/6/8/9/10/11/12/16).
//!
//! All tests are behavioural: they construct real presenter/model state,
//! real GTK widgets (headlessly), or real enum values — never scanning
//! source text.

mod common;

use common::after;
use namtor_ui::view_model::{EnrollmentViewV3, ErrorView, RecoveryHint};
use namtor_ui::{
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
    use namtor_ui::app::AppController;

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

        let fail_result: namtor_client::types::ManagerResult<namtor_ui::DbusManagerClient> = Err(
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
        let success_result: namtor_client::types::ManagerResult<namtor_ui::DbusManagerClient> =
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
    use namtor_ui::components::EnrollmentViewComponent;

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
    use namtor_ui::app::AppController;

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
    use namtor_ui::components::system_status::render_system_status;
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

        // The rows must actually offer the command. The test was named
        // "..._rows_wired" but only asserted the two titles appeared, so it
        // passed for years while both `connect_activated` bodies were empty
        // and the rows did nothing at all.
        assert!(
            all_texts
                .iter()
                .any(|t| t.contains("systemctl restart namtor-daemon")),
            "the restart row must show the command to run: {all_texts:?}"
        );
        assert!(
            all_texts
                .iter()
                .any(|t| t.contains("journalctl -u namtor-daemon")),
            "the logs row must show the command to run: {all_texts:?}"
        );

        // And a button to copy it, rather than an activatable row that does
        // nothing when clicked.
        let mut buttons = 0;
        for group in &groups {
            buttons +=
                common::count_widgets_of_type::<gtk4::Button>(group.upcast_ref::<gtk4::Widget>());
        }
        assert!(
            buttons >= 2,
            "each troubleshooting row needs a copy button, found {buttons}"
        );

        // And the buttons must do something when pressed. Counting them is
        // what the previous version of this test did about the rows, while
        // both handlers were empty; a button with an empty handler counts
        // exactly the same. Clicking is the only thing that tells them apart.
        let mut clicked = 0;
        for group in &groups {
            common::for_each_button(group.upcast_ref::<gtk4::Widget>(), &mut |b| {
                if b.icon_name().as_deref() != Some("edit-copy-symbolic") {
                    return;
                }
                b.emit_clicked();
                assert_eq!(
                    b.icon_name().as_deref(),
                    Some("object-select-symbolic"),
                    "a copy with no visible confirmation reads as another dead button"
                );
                clicked += 1;
            });
        }
        assert!(
            clicked >= 2,
            "expected to press both copy buttons, pressed {clicked}"
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
    use namtor_ui::components::EnrollmentViewComponent;

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
        let page = comp.render(Some(&complete_view), true, |_| {}, || {}, || {});
        let texts = common::collect_widget_text(page.upcast_ref::<gtk4::Widget>());
        assert!(
            texts.iter().any(|t| t.contains("Enrollment complete")),
            "complete view must show completion cue: {texts:?}"
        );

        // Incomplete view: must NOT show completion cue.
        let page_incomplete = comp.render(Some(&incomplete_view), true, |_| {}, || {}, || {});
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
    use namtor_ui::components::ProfileListView;
    use namtor_ui::view_model::{PresenterModel, ProfileRowView};

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
    use namtor_ui::presenter::after;

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
    use namtor_ui::components::RecoveryView;

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
    use namtor_ui::components::RecoveryView;

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

// ===========================================================================
// The profile list showed a raw Unix timestamp
// ===========================================================================

/// `format!("epoch {}", row.last_used)` rendered "last used: epoch
/// 1726550400", while `ProfileRowView`'s doc promised "a localized 'last
/// used' line".
#[test]
fn last_used_is_never_a_raw_epoch() {
    use namtor_ui::view_model::last_used_label;
    const NOW: u64 = 1_726_550_400;

    let cases: [(u64, &str); 8] = [
        (0, "never"),
        (NOW - 5, "just now"),
        (NOW - 90, "a minute ago"),
        (NOW - 3 * 3600, "3 hours ago"),
        (NOW - 24 * 3600, "yesterday"),
        (NOW - 3 * 24 * 3600, "3 days ago"),
        (NOW - 10 * 24 * 3600, "last week"),
        (NOW - 400 * 24 * 3600, "last year"),
    ];
    for (ts, expected) in cases {
        let got = last_used_label(ts, NOW);
        assert_eq!(got, expected, "last_used_label({ts}, {NOW})");
        assert!(
            !got.contains("epoch") && !got.contains(&ts.to_string()),
            "a raw timestamp leaked into the label: {got:?}"
        );
    }
}

/// A timestamp in the future means the clock moved, not that the profile will
/// be used later. "in -3 days" would be worse than saying nothing useful.
#[test]
fn a_future_last_used_does_not_render_negative_time() {
    use namtor_ui::view_model::last_used_label;
    let got = last_used_label(2_000_000_000, 1_726_550_400);
    assert_eq!(got, "just now");
    assert!(!got.contains('-'), "negative duration leaked: {got:?}");
}

// ===========================================================================
// The Saving state renders something real
// ===========================================================================

/// `render_saving` must show a spinner and a status line, and no buttons: a
/// commit in flight cannot be cancelled, and offering a control that does
/// nothing is what the Error state used to do.
#[test]
fn the_saving_state_shows_progress_and_no_dead_controls() {
    use gtk4::prelude::*;
    use namtor_ui::components::enrollment::EnrollmentViewComponent;
    common::run_gtk_test(|| {
        let mut c = EnrollmentViewComponent::new();
        let page = c.render_saving();
        let w = page.upcast_ref::<gtk4::Widget>();
        assert_eq!(
            common::count_widgets_of_type::<gtk4::Spinner>(w),
            1,
            "saving must show a spinner"
        );
        assert_eq!(
            common::count_widgets_of_type::<gtk4::Button>(w),
            0,
            "saving must not offer controls that cannot act"
        );
        let texts = common::collect_widget_text(w);
        assert!(
            texts.iter().any(|t| t.contains("Saving")),
            "saving must say what is happening: {texts:?}"
        );
    });
}

/// `render_error` must show the message and two working buttons. The Error
/// arm used to build a Dismiss button and never add it to any container.
#[test]
fn the_error_state_shows_the_message_and_two_buttons() {
    use gtk4::prelude::*;
    use namtor_ui::components::enrollment::EnrollmentViewComponent;
    use std::cell::Cell;
    use std::rc::Rc;
    common::run_gtk_test(|| {
        let dismissed = Rc::new(Cell::new(false));
        let d = Rc::clone(&dismissed);
        let mut c = EnrollmentViewComponent::new();
        let page = c.render_error("no IR camera found", None, move || d.set(true), || {});
        let w = page.upcast_ref::<gtk4::Widget>().clone();
        let texts = common::collect_widget_text(&w);
        assert!(
            texts.iter().any(|t| t.contains("no IR camera found")),
            "the failure detail must be shown: {texts:?}"
        );
        assert_eq!(
            common::count_widgets_of_type::<gtk4::Button>(&w),
            2,
            "Try Again and Cancel must both be present"
        );
        // And they must actually be wired: click the first button.
        let btn = common::first_widget_of_type::<gtk4::Button>(&w).expect("a button");
        btn.emit_clicked();
        assert!(dismissed.get(), "Try Again must invoke its callback");
    });
}

// ===========================================================================
// A programmatic pop must not re-enter the controller's RefCell
// ===========================================================================

/// `AdwNavigationView::pop()` emits `popped` SYNCHRONOUSLY, so the handler
/// runs inside `pop_subpage()` — which runs inside `render_enrollment`, which
/// the caller invoked as `rc.borrow_mut().render_enrollment(&rc)`. Without a
/// guard the handler takes a second mutable borrow of a live `RefCell`, and
/// the release profile is `panic = "abort"`, so the app dies. Pressing Save
/// reached exactly that: the `Saving` arm pops the review page.
///
/// This had no coverage because every other test builds the controller with
/// `new_offline_for_test`, which never installs the handler.
#[test]
fn a_programmatic_pop_while_borrowed_does_not_re_enter() {
    use gtk4::prelude::*;
    use namtor_ui::app::AppController;

    common::run_gtk_test(move || {
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.popguard")
            .build();
        let _guard = app.hold();

        let rc = AppController::new_offline_for_test(&app);
        AppController::install_pop_handler(&rc);

        // Push a sub-page, as the enrollment arms do.
        {
            let c = rc.borrow();
            let page = libadwaita::NavigationPage::builder()
                .title("probe")
                .child(&gtk4::Box::new(gtk4::Orientation::Vertical, 0))
                .build();
            c.shell_for_test().push_subpage(&page);
        }

        // Pop it while the controller is mutably borrowed — the shape of
        // `rc.borrow_mut().render_enrollment(&rc)` reaching the Saving arm.
        // A BorrowMutError here aborts the process, so reaching the assert at
        // all is the result.
        rc.borrow_mut().pop_subpage_for_test();
    });
}

// ===========================================================================
// The profile rows a user actually touches
// ===========================================================================

/// Give the fake bus `n` enrolled profiles. It starts empty, and an empty
/// list renders a single "No profiles enrolled yet." row — no switches, no
/// rename buttons, and a test that would pass by finding nothing.
fn seed_profiles(bus: &namtor_client::fake_bus::FakeBus, n: usize) {
    use namtor_ui::ManagerClient;
    for i in 0..n {
        let session = bus
            .client
            .start_enrollment(TEST_UID, &format!("profile-{i}"))
            .expect("start enrollment on the fake bus");
        bus.client
            .commit_profile(&session)
            .expect("commit profile on the fake bus");
    }
}

/// Flipping a profile's enable switch runs a callback that takes a mutable
/// borrow of the presenter, drops it, then takes a mutable borrow of the
/// controller and re-renders the whole list from inside a GTK signal handler.
/// The release profile is `panic = "abort"`, so a second live borrow anywhere
/// on that path kills the Settings app outright — the same class of bug as
/// the programmatic pop above, on the path with the most user traffic.
///
/// Nothing drove it. `AppController` is typed to
/// `ProfilePresenter<DbusManagerClient>`, so a trait mock cannot be injected,
/// and every test used `new_offline_for_test`, where `presenter` is `None`
/// and `render_all` returns before rendering a single row. `FakeBus` hands
/// out a real `DbusManagerClient`, which is what this uses.
#[test]
fn flipping_a_profile_switch_does_not_re_enter_the_controller() {
    use gtk4::prelude::*;
    use namtor_client::fake_bus::FakeBus;
    use namtor_ui::app::AppController;

    common::run_gtk_test(move || {
        let bus = FakeBus::spawn().expect("spawn fake bus");
        seed_profiles(&bus, 2);
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.profilerows")
            .build();
        let _guard = app.hold();

        // A second client on the same bus, so the test can read back what
        // the UI's client wrote.
        let observer = bus.observer();

        let rc = AppController::new_online_for_test(&app, bus.client);
        let window = rc.borrow().shell_for_test().window().clone();
        let root: gtk4::Widget = window.clone().upcast();

        // The fake serves profiles, so rows must have rendered. If they did
        // not, the rest of this test would pass by finding nothing.
        let switches = common::count_widgets_of_type::<libadwaita::SwitchRow>(&root);
        assert!(
            switches > 0,
            "no profile switch rendered — the test would prove nothing"
        );

        let sw =
            common::first_widget_of_type::<libadwaita::SwitchRow>(&root).expect("a profile switch");
        let before = sw.is_active();

        // The user flips it. Everything downstream — presenter borrow,
        // controller borrow, full re-render of the list this very widget
        // belongs to — runs synchronously inside this call.
        sw.set_active(!before);

        // Surviving is most of the point, but a live controller is the rest:
        // an aborted borrow would have taken the process with it, and a
        // poisoned one would show up here.
        assert!(
            rc.try_borrow().is_ok(),
            "the controller is still mutably borrowed after the toggle handler returned"
        );
        assert!(rc.borrow().has_presenter_for_test());
        assert!(!rc.borrow().is_offline_for_test());

        // The re-render replaces the rows, so the widget flipped above is
        // detached; the current tree is what the user sees.
        let after = common::count_widgets_of_type::<libadwaita::SwitchRow>(&root);
        assert_eq!(
            after, switches,
            "the list must still hold one switch per profile after re-render"
        );

        // Surviving is not the same as working. The flip must have reached
        // the daemon — a handler that swallowed the change would pass every
        // assertion above.
        use namtor_ui::ManagerClient;
        let profiles = observer.list_profiles().expect("list profiles");
        assert!(
            profiles.iter().any(|p| p.enabled == !before),
            "the toggle did not reach the daemon: {profiles:?}"
        );
    });
}

/// The rename button on each profile row opens a dialog. It is wired from the
/// same place and carries the same re-entrancy shape, and clicking it was
/// equally undriven.
#[test]
fn clicking_rename_on_a_profile_row_does_not_re_enter_the_controller() {
    use gtk4::prelude::*;
    use namtor_client::fake_bus::FakeBus;
    use namtor_ui::app::AppController;

    common::run_gtk_test(move || {
        let bus = FakeBus::spawn().expect("spawn fake bus");
        seed_profiles(&bus, 1);
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.profilerename")
            .build();
        let _guard = app.hold();

        let rc = AppController::new_online_for_test(&app, bus.client);
        let window = rc.borrow().shell_for_test().window().clone();
        let root: gtk4::Widget = window.clone().upcast();

        // The rename button is the one with the edit icon; the Enroll button
        // has a label instead.
        let mut rename: Option<gtk4::Button> = None;
        common::for_each_button(&root, &mut |b| {
            if b.icon_name().as_deref() == Some("document-edit-symbolic") && rename.is_none() {
                rename = Some(b.clone());
            }
        });
        let rename = rename.expect("a profile row must carry a rename button");

        rename.emit_clicked();

        assert!(
            rc.try_borrow().is_ok(),
            "the controller is still mutably borrowed after the rename handler returned"
        );
        assert!(rc.borrow().has_presenter_for_test());
    });
}

/// The rename dialog's empty-name guard: pressing Rename with a blank field
/// must mark the entry and leave the dialog open, not close it having done
/// nothing. `AdwMessageDialog` does not close itself when a response is
/// activated — the handler's early `return` is what keeps it open — and that
/// dependency had never been exercised, so the guard could have been
/// cosmetic.
#[test]
fn renaming_to_a_blank_name_keeps_the_dialog_open_and_changes_nothing() {
    use gtk4::prelude::*;
    use libadwaita::prelude::*;
    use namtor_client::fake_bus::FakeBus;
    use namtor_ui::app::AppController;
    use namtor_ui::ManagerClient;

    common::run_gtk_test(move || {
        let bus = FakeBus::spawn().expect("spawn fake bus");
        seed_profiles(&bus, 1);
        let observer = bus.observer();
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.renameguard")
            .build();
        let _guard = app.hold();

        let rc = AppController::new_online_for_test(&app, bus.client);
        let window = rc.borrow().shell_for_test().window().clone();
        let root: gtk4::Widget = window.clone().upcast();

        let original = observer.list_profiles().expect("list")[0].name.clone();

        let mut rename: Option<gtk4::Button> = None;
        common::for_each_button(&root, &mut |b| {
            if b.icon_name().as_deref() == Some("document-edit-symbolic") && rename.is_none() {
                rename = Some(b.clone());
            }
        });
        rename.expect("a rename button").emit_clicked();

        let dialog =
            message_dialog_headed("Rename Profile").expect("the rename dialog must be presented");
        let entry = common::first_widget_of_type::<libadwaita::EntryRow>(
            &dialog.clone().upcast::<gtk4::Widget>(),
        )
        .expect("the dialog carries a name entry");
        assert_eq!(
            entry.text().as_str(),
            original,
            "prefilled with the old name"
        );

        // Whitespace is blank.
        entry.set_text("   ");
        dialog.response("rename");

        assert!(
            dialog.is_visible(),
            "a blank name must leave the dialog open so the user can correct it"
        );
        assert!(
            entry.has_css_class("error"),
            "the entry must be marked so the user can see what is wrong"
        );
        assert_eq!(
            observer.list_profiles().expect("list")[0].name,
            original,
            "a blank name must not have renamed anything"
        );

        // A real name goes through, and the dialog closes.
        entry.remove_css_class("error");
        entry.set_text("Bright Name");
        dialog.response("rename");
        assert!(!dialog.is_visible(), "a valid rename closes the dialog");
        assert_eq!(
            observer.list_profiles().expect("list")[0].name,
            "Bright Name"
        );

        assert!(
            rc.try_borrow().is_ok(),
            "the controller is still mutably borrowed after the rename handler returned"
        );
    });
}

/// The `AdwMessageDialog` whose heading contains `needle`, among the current
/// toplevels. Dialogs are separate toplevels, so they are not reachable by
/// walking the window, and a dialog presented without a transient parent is
/// not necessarily `is_visible()` — matching on the heading is what makes
/// this reliable in both cases.
fn message_dialog_headed(needle: &str) -> Option<libadwaita::MessageDialog> {
    use gtk4::prelude::*;
    use libadwaita::prelude::*;
    gtk4::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<libadwaita::MessageDialog>().ok())
        .find(|d| d.heading().is_some_and(|h| h.contains(needle)))
}

/// Flipping the "Authentication uses" switch for a service is the one place
/// in the app that triggers a polkit prompt. The handler ends by putting the
/// switch back on whatever the daemon reports, and `connect_active_notify`
/// fires for programmatic changes too — so without the `APPLYING` guard that
/// re-render re-enters the handler and issues a second toggle, with a second
/// prompt, for a state the user only asked to have displayed.
///
/// The guard existed and was never exercised: no test in the suite ever set
/// a switch's active state or emitted `active-notify`. The presenter-level
/// tests drive `request_pam_toggle` directly, which is the layer underneath
/// it.
#[test]
fn enabling_a_safe_service_issues_exactly_one_call_and_asks_nothing() {
    use gtk4::prelude::*;
    use namtor_ui::components::pam_services::render_pam_services;
    use std::sync::Arc;

    common::run_gtk_test(move || {
        let fake = Arc::new(FakeManagerClient::new());
        let (rc, target) = pam_fixture(&fake, namtor_ui::PamServiceCompatibility::Safe);
        let group = render_pam_services(&rc);
        let root: gtk4::Widget = group.clone().upcast();

        let switch =
            switch_for_service(&root, &target.id).expect("the target service must have a switch");
        assert!(!switch.is_active(), "the target service starts disabled");

        let before = fake.pam_toggle_calls();
        switch.set_active(true);
        let after = fake.pam_toggle_calls();

        assert_eq!(
            after - before,
            1,
            "toggling {} issued {} calls; each one is a separate polkit prompt",
            target.display_name,
            after - before
        );
        assert!(
            message_dialog_headed("high-impact").is_none(),
            "a Safe service must apply without asking"
        );
        assert!(
            rc.try_borrow().is_ok(),
            "the presenter is still borrowed after the toggle handler returned"
        );
        assert!(switch.is_active(), "the switch must show the applied state");
    });
}

/// Enabling a high-impact service asks first and applies nothing until the
/// warning is accepted. Nothing had ever answered that dialog either way.
#[test]
fn confirming_the_high_impact_warning_applies_it_once() {
    use gtk4::prelude::*;
    use libadwaita::prelude::*;
    use namtor_ui::components::pam_services::render_pam_services;
    use std::sync::Arc;

    common::run_gtk_test(move || {
        let fake = Arc::new(FakeManagerClient::new());
        let (rc, target) = pam_fixture(&fake, namtor_ui::PamServiceCompatibility::Risky);
        let group = render_pam_services(&rc);
        let root: gtk4::Widget = group.clone().upcast();

        let switch =
            switch_for_service(&root, &target.id).expect("the target service must have a switch");

        let before = fake.pam_toggle_calls();
        switch.set_active(true);
        assert_eq!(
            fake.pam_toggle_calls(),
            before,
            "a high-impact service must not apply before the warning is accepted"
        );

        let dialog = message_dialog_headed("high-impact")
            .expect("enabling a high-impact service must ask first");
        dialog.response("confirm");

        assert_eq!(
            fake.pam_toggle_calls() - before,
            1,
            "accepting the warning must apply it exactly once"
        );
        assert!(
            rc.borrow().model().pending_pam_toggle.is_none(),
            "the staged toggle must be cleared once it is applied"
        );
        assert!(
            rc.try_borrow().is_ok(),
            "the presenter is still borrowed after the confirm handler returned"
        );
    });
}

/// Declining the high-impact warning must apply nothing and put the switch
/// back.
#[test]
fn declining_the_high_impact_warning_applies_nothing() {
    use gtk4::prelude::*;
    use libadwaita::prelude::*;
    use namtor_ui::components::pam_services::render_pam_services;
    use std::sync::Arc;

    common::run_gtk_test(move || {
        let fake = Arc::new(FakeManagerClient::new());
        let (rc, target) = pam_fixture(&fake, namtor_ui::PamServiceCompatibility::Risky);
        let group = render_pam_services(&rc);
        let root: gtk4::Widget = group.clone().upcast();

        let switch =
            switch_for_service(&root, &target.id).expect("the target service must have a switch");

        let before = fake.pam_toggle_calls();
        switch.set_active(true);
        let dialog = message_dialog_headed("high-impact")
            .expect("enabling a high-impact service must ask first");
        dialog.response("cancel");

        assert_eq!(
            fake.pam_toggle_calls(),
            before,
            "declining the warning must not have applied anything"
        );
        assert!(
            !switch.is_active(),
            "the switch must go back to off after the warning is declined"
        );
        assert!(
            rc.borrow().model().pending_pam_toggle.is_none(),
            "the staged toggle must be cleared when it is declined"
        );
    });
}

/// A presenter wired to `fake`, refreshed, plus a disabled service of the
/// given compatibility to act on. Naming the service matters: the first
/// switch that happens to be off may belong to an `Unsupported` service,
/// which has no handler at all, and a test that flipped one of those would
/// observe nothing happening and call it a pass.
fn pam_fixture(
    fake: &std::sync::Arc<FakeManagerClient>,
    want: namtor_ui::PamServiceCompatibility,
) -> (
    std::rc::Rc<std::cell::RefCell<ProfilePresenter<FakeManagerClient>>>,
    namtor_ui::view_model::PamServiceRowView,
) {
    let mut presenter = ProfilePresenter::from_arc(std::sync::Arc::clone(fake), TEST_UID);
    presenter.refresh();
    let target = presenter
        .model()
        .pam_services
        .iter()
        .find(|s| s.compatibility == want && !s.enabled)
        .unwrap_or_else(|| panic!("the fake must serve a disabled {want:?} PAM service"))
        .clone();
    (std::rc::Rc::new(std::cell::RefCell::new(presenter)), target)
}

/// The switch belonging to one PAM service.
///
/// Picking "the first switch that is off" picks whichever service happens to
/// come first, and an `Unsupported` service gets no handler at all — a test
/// that flipped one of those would observe nothing happening and call it a
/// pass. The switch carries the service id in its tooltip, so it can be
/// named.
fn switch_for_service(root: &gtk4::Widget, service_id: &str) -> Option<libadwaita::SwitchRow> {
    use gtk4::prelude::*;
    let mut found = None;
    collect_switch_rows(root, &mut |sw| {
        if found.is_none()
            && sw
                .tooltip_text()
                .is_some_and(|t| t.contains(&format!("({service_id})")))
        {
            found = Some(sw.clone());
        }
    });
    found
}

/// Walk a widget tree and hand every `AdwSwitchRow` to `f`.
fn collect_switch_rows(widget: &gtk4::Widget, f: &mut dyn FnMut(&libadwaita::SwitchRow)) {
    use gtk4::prelude::*;
    if let Some(sw) = widget.downcast_ref::<libadwaita::SwitchRow>() {
        f(sw);
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        collect_switch_rows(&c, f);
        child = c.next_sibling();
    }
}

/// `build()` calls `show_offline` twice: once with a no-op callback while the
/// controller does not exist yet, then again with the real `retry_connection`
/// closure once it does. If the second call did not replace the first, the
/// Retry button on the offline page would be dead on first launch and the
/// user would sit in front of "cannot reach the daemon" with nothing to
/// press. `build()` itself is never run by a test — it needs a real system
/// bus — so the replace semantics it depends on are pinned here.
#[test]
fn showing_the_offline_page_twice_leaves_the_second_retry_wired() {
    use gtk4::prelude::*;
    use namtor_ui::app::AppController;
    use std::cell::Cell;
    use std::rc::Rc;

    common::run_gtk_test(move || {
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.offlineretry")
            .build();
        let _guard = app.hold();

        let rc = AppController::new_offline_for_test(&app);

        let first = Rc::new(Cell::new(0u32));
        let second = Rc::new(Cell::new(0u32));

        {
            let f = Rc::clone(&first);
            rc.borrow_mut()
                .shell_for_test_mut()
                .show_offline("daemon stopped", move || f.set(f.get() + 1));
        }
        {
            let s = Rc::clone(&second);
            rc.borrow_mut()
                .shell_for_test_mut()
                .show_offline("daemon stopped", move || s.set(s.get() + 1));
        }

        let window = rc.borrow().shell_for_test().window().clone();
        let root: gtk4::Widget = window.clone().upcast();

        let mut pressed = 0;
        common::for_each_button(&root, &mut |b| {
            b.emit_clicked();
            pressed += 1;
        });
        assert!(pressed > 0, "the offline page must offer a button to press");

        assert_eq!(
            first.get(),
            0,
            "the first callback is still wired: the Retry button would be \
             dead on first launch"
        );
        assert!(
            second.get() > 0,
            "the second callback never ran: Retry does nothing"
        );
    });
}

/// An error on the Profiles page is the one a user meets first, and it used
/// to be a single red line: title and detail glued together, and the recovery
/// steps the presenter had already worked out discarded. Rendering the page
/// and looking at it showed "Connection error: dbus transport error:
/// org.freedesktop.DBus.Error.UnknownInterface: Unknown interface
/// 'org.namtor.Manager2'" as one line of red text with nothing to do about
/// it.
#[test]
fn a_profiles_page_error_shows_the_condition_the_detail_and_the_steps() {
    use gtk4::prelude::*;
    use namtor_ui::app::profiles_error_groups;
    use namtor_ui::view_model::{ErrorView, RecoveryHint};

    common::run_gtk_test(move || {
        let err = ErrorView {
            title: "Camera is busy".into(),
            detail: "Another program has the camera open.".into(),
            recovery: Some(RecoveryHint {
                title: "Another program is using the camera".into(),
                steps: vec![
                    "Close any video call, camera app, or recorder".into(),
                    "Find what is holding it: fuser -v /dev/video*".into(),
                ],
            }),
        };
        let groups = profiles_error_groups(&err);
        assert_eq!(groups.len(), 2, "condition and steps are separate groups");

        let mut texts = Vec::new();
        for g in &groups {
            texts.extend(common::collect_widget_text(g.upcast_ref::<gtk4::Widget>()));
        }
        // The condition is its own line, not prefixed onto the detail.
        assert!(
            texts.iter().any(|t| t == "Camera is busy"),
            "the condition must stand alone: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t == "Another program has the camera open."),
            "the detail must be its own line: {texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|t| t.starts_with("Camera is busy: Another program")),
            "the two must not be glued back together: {texts:?}"
        );
        // And the steps are numbered and present.
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("1. Close any video call")),
            "the first step must be shown: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("fuser")),
            "the user needs the command that names the holder: {texts:?}"
        );
    });
}

/// An error with no recovery hint renders the condition and the detail, and
/// no empty steps group.
#[test]
fn a_profiles_page_error_without_steps_renders_one_group() {
    use namtor_ui::app::profiles_error_groups;
    use namtor_ui::view_model::ErrorView;

    common::run_gtk_test(move || {
        let err = ErrorView {
            title: "Operation failed".into(),
            detail: "state: something went wrong".into(),
            recovery: None,
        };
        assert_eq!(profiles_error_groups(&err).len(), 1);
    });
}
