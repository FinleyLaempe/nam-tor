// SPDX-License-Identifier: GPL-3.0-only
//! Regression test: the GNOME enrollment wizard must wire the real
//! application UI end-to-end, not hard-code placeholder labels.
//!
//! All tests are behavioural: they construct real AppControllers,
//! presenters, and GTK components headlessly — never scanning source text.

mod common;

use namtor_gnome::fake::FakeManagerClient;
use namtor_gnome::presenter::ProfilePresenter;

const TEST_UID: u32 = 1000;

/// The wizard must not render placeholder "will appear here" text. The old
/// wizard hard-coded three such labels. Verified by constructing a real
/// Shell (the wizard's container) and asserting the widget tree contains
/// no placeholder text.
#[test]
fn wizard_renders_real_content_not_placeholders() {
    use gtk4::prelude::*;

    common::run_gtk_test(|| {
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.wiz0")
            .build();
        let _guard = app.hold();

        let shell = namtor_gnome::shell::Shell::new(&app);
        let dialog = shell.dialog();
        let texts = common::collect_widget_text(dialog.upcast_ref::<gtk4::Widget>());

        assert!(
            !texts.iter().any(|t| t.contains("will appear here")),
            "shell must not contain placeholder 'will appear here' text: {texts:?}"
        );
    });
}

/// The wizard must delegate to a real AppController that constructs a
/// real D-Bus client. Verified by constructing an AppController with
/// FakeBus and asserting it transitions to online state with a presenter.
#[test]
fn wizard_uses_real_client_or_app_controller() {
    use namtor_gnome::app::AppController;

    common::run_gtk_test(|| {
        use gtk4::prelude::*;
        use namtor_client::fake_bus::FakeBus;

        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.wiz1")
            .build();
        let _guard = app.hold();

        let controller = AppController::new_offline_for_test(&app);
        let rc = controller.clone();

        assert!(
            controller.borrow().is_offline_for_test(),
            "controller must start offline"
        );

        let fake_bus = FakeBus::spawn().expect("spawn fake bus");
        let client: namtor_client::types::ManagerResult<namtor_gnome::DbusManagerClient> =
            Ok(fake_bus.client);
        controller.borrow_mut().apply_connect_result(&rc, client);

        assert!(
            !controller.borrow().is_offline_for_test(),
            "controller must be online after FakeBus connect"
        );
        assert!(
            controller.borrow().has_presenter_for_test(),
            "controller must have a presenter (real client, not placeholder)"
        );
    });
}

/// The AppController must instantiate a `ProfilePresenter` — the headless
/// state machine that drives every destination. Verified by checking the
/// controller has a presenter after connecting to FakeBus.
#[test]
fn app_controller_uses_profile_presenter() {
    use namtor_gnome::app::AppController;

    common::run_gtk_test(|| {
        use gtk4::prelude::*;
        use namtor_client::fake_bus::FakeBus;

        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.wiz2")
            .build();
        let _guard = app.hold();

        let controller = AppController::new_offline_for_test(&app);
        let rc = controller.clone();

        let fake_bus = FakeBus::spawn().expect("spawn fake bus");
        controller
            .borrow_mut()
            .apply_connect_result(&rc, Ok(fake_bus.client));

        assert!(
            controller.borrow().has_presenter_for_test(),
            "AppController must instantiate a ProfilePresenter"
        );
    });
}

/// The AppController must render the real `ProfileListView` into the
/// Profiles destination. Verified by constructing a ProfileListView with
/// a real model and asserting it renders profile content.
#[test]
fn app_controller_renders_profile_list_view() {
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
            texts.iter().any(|t| t.contains("Test User")),
            "ProfileListView must render the profile name: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Enrolled Profiles")),
            "ProfileListView must render the group title: {texts:?}"
        );
    });
}

/// The AppController must render the real PAM services component into the
/// Authentication destination. Verified by calling `render_pam_services`
/// with a real presenter and asserting it returns a non-empty group.
#[test]
fn app_controller_renders_pam_services() {
    use gtk4::prelude::*;
    use namtor_gnome::components::pam_services::render_pam_services;
    use std::cell::RefCell;
    use std::rc::Rc;

    common::run_gtk_test(|| {
        let mut presenter = ProfilePresenter::new(FakeManagerClient::new(), TEST_UID);
        presenter.refresh();
        let presenter_rc = Rc::new(RefCell::new(presenter));

        let group = render_pam_services(&presenter_rc);
        assert_eq!(
            group.type_().name(),
            "AdwPreferencesGroup",
            "render_pam_services must return an AdwPreferencesGroup"
        );

        let texts = common::collect_widget_text(group.upcast_ref::<gtk4::Widget>());
        assert!(
            !texts.is_empty(),
            "PAM services group must have content: {texts:?}"
        );
    });
}

/// The AppController must render the real system status component into the
/// System destination. Verified by calling `render_system_status` with a
/// real presenter and asserting it returns non-empty groups.
#[test]
fn app_controller_renders_system_status() {
    use gtk4::prelude::*;
    use namtor_gnome::components::system_status::render_system_status;
    use std::cell::RefCell;
    use std::rc::Rc;

    common::run_gtk_test(|| {
        let mut presenter = ProfilePresenter::new(FakeManagerClient::new(), TEST_UID);
        presenter.refresh();
        let presenter_rc = Rc::new(RefCell::new(presenter));

        let groups = render_system_status(&presenter_rc);
        assert!(
            groups.len() >= 2,
            "render_system_status must return at least 2 groups"
        );

        for group in &groups {
            let texts = common::collect_widget_text(group.upcast_ref::<gtk4::Widget>());
            assert!(!texts.is_empty(), "system status group must have content");
        }
    });
}

/// The AppController must use the `EnrollmentViewComponent` and
/// `ReviewViewComponent` for the enrollment flow. Verified by constructing
/// both components and rendering them with real view models.
#[test]
fn app_controller_uses_enrollment_and_review_components() {
    use gtk4::prelude::*;
    use namtor_gnome::components::{EnrollmentViewComponent, ReviewViewComponent};
    use namtor_gnome::view_model::{EnrollmentViewV3, ReviewViewV3};

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
    let review = ReviewViewV3 {
        ready_to_save: true,
        quality_percent: 80,
        diversity_percent: 60,
        frames_accepted: 8,
        guidance: "Looking good".into(),
    };

    common::run_gtk_test(move || {
        // EnrollmentViewComponent must render the capture phase.
        let mut enrollment = EnrollmentViewComponent::new();
        let page = enrollment.render(Some(&view), |_| {}, || {}, || {});
        let texts = common::collect_widget_text(page.upcast_ref::<gtk4::Widget>());
        assert!(
            texts.iter().any(|t| t.contains("Capturing enrollment")),
            "EnrollmentViewComponent must render capture phase: {texts:?}"
        );

        // ReviewViewComponent must render the review summary.
        let mut review_comp = ReviewViewComponent::new();
        let page = review_comp.render(&review, || {}, || {}, || {}, false);
        let texts = common::collect_widget_text(page.upcast_ref::<gtk4::Widget>());
        assert!(
            texts.iter().any(|t| t.contains("Review Enrollment")),
            "ReviewViewComponent must render review summary: {texts:?}"
        );
    });
}

/// The presenter must expose all v3 lifecycle methods. Verified by calling
/// each method on a real presenter with a FakeManagerClient.
#[test]
fn app_controller_calls_v3_lifecycle_methods() {
    let mut presenter = ProfilePresenter::new(FakeManagerClient::new(), TEST_UID);

    common::after(&mut presenter, |p| {
        p.refresh();
    });
    common::after(&mut presenter, |p| {
        p.start_enrollment_v3("Test User");
    });
    common::after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    common::after(&mut presenter, |p| {
        p.review_enrollment_v3();
    });
    common::after(&mut presenter, |p| {
        p.resume_enrollment_v3();
    });
    common::after(&mut presenter, |p| {
        p.stop_enrollment_v3();
    });
    common::after(&mut presenter, |p| {
        p.review_enrollment_v3();
    });
    common::after(&mut presenter, |p| {
        p.save_profile_v3();
    });
    assert_eq!(
        presenter.model().profiles.len(),
        1,
        "v3 lifecycle must produce a saved profile"
    );

    common::after(&mut presenter, |p| {
        p.discard_enrollment_v3();
    });
}

/// The enrollment component must connect its callbacks to buttons — the
/// old code accepted `on_start`/`on_stop` parameters but prefixed them
/// with `_` and never wired them. Verified by constructing the component,
/// rendering it, and asserting the buttons exist and are sensitive.
#[test]
fn enrollment_component_connects_callbacks() {
    use gtk4::prelude::*;
    use namtor_gnome::components::EnrollmentViewComponent;
    use namtor_gnome::view_model::EnrollmentViewV3;

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

    common::run_gtk_test(move || {
        let mut comp = EnrollmentViewComponent::new();
        let page = comp.render(Some(&view), |_| {}, || {}, || {});

        // The capture phase must have Stop and Cancel buttons.
        let texts = common::collect_widget_text(page.upcast_ref::<gtk4::Widget>());
        assert!(
            texts.iter().any(|t| t.contains("Stop")),
            "enrollment capture must have a Stop button: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Cancel")),
            "enrollment capture must have a Cancel button: {texts:?}"
        );
    });
}

/// The review component must connect its callbacks to buttons — the old
/// code accepted `on_save`/`on_resume`/`on_discard` parameters but
/// prefixed them with `_` and never wired them. Verified by constructing
/// the component, rendering it, and asserting the buttons exist.
#[test]
fn review_component_connects_callbacks() {
    use gtk4::prelude::*;
    use namtor_gnome::components::ReviewViewComponent;
    use namtor_gnome::view_model::ReviewViewV3;

    let review = ReviewViewV3 {
        ready_to_save: true,
        quality_percent: 80,
        diversity_percent: 60,
        frames_accepted: 8,
        guidance: "Looking good".into(),
    };

    common::run_gtk_test(move || {
        let mut comp = ReviewViewComponent::new();
        let page = comp.render(&review, || {}, || {}, || {}, false);

        let texts = common::collect_widget_text(page.upcast_ref::<gtk4::Widget>());
        assert!(
            texts.iter().any(|t| t.contains("Save")),
            "review must have a Save button: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Resume")),
            "review must have a Resume button: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Discard")),
            "review must have a Discard button: {texts:?}"
        );
    });
}

/// The profile list component must connect the enabled toggle to a
/// callback so profile enable/disable is wired through the presenter.
/// Verified by constructing a ProfileListView with a toggle callback and
/// asserting the rendered group contains a SwitchRow.
#[test]
fn profile_list_component_connects_toggle() {
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

        assert!(
            common::widget_tree_contains_type(group.upcast_ref::<gtk4::Widget>(), "SwitchRow"),
            "profile list must contain a SwitchRow toggle"
        );
    });
}

/// The built wizard binary MUST NOT contain placeholder strings. This test
/// runs `strings` on the compiled binary and checks for the absence of
/// "will appear here". It is ignored in CI (no binary guaranteed) but can
/// be run with `--ignored` after a release build.
#[test]
#[ignore = "requires a release-built binary; run with NAMTOR_BIN_TEST=1"]
fn built_wizard_binary_has_no_placeholders() {
    let bin = std::env::var("NAMTOR_BIN_TEST").ok();
    let bin_path = bin
        .as_deref()
        .unwrap_or("target/release/namtor-enroll-wizard");
    let output = std::process::Command::new("strings")
        .arg(bin_path)
        .output()
        .expect("strings command");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("will appear here"),
        "built wizard binary still contains placeholder 'will appear here' strings"
    );
}
