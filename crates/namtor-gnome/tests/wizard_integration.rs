// SPDX-License-Identifier: GPL-3.0-only
//! Regression test: the GNOME enrollment wizard must wire the real
//! application UI end-to-end, not hard-code placeholder labels.
//!
//! This test was introduced failing-first: the old `wizard.rs` constructed
//! three `Label` widgets with "will appear here" text and ignored the
//! existing presenter, client, and components. After the fix the wizard
//! delegates to an app controller that instantiates `DbusManagerClient`,
//! `ProfilePresenter`, and the real `ProfileListView` / PAM / system
//! components.
//!
//! The assertions are source-level (no display server needed) so they run
//! in CI. They guard against a regression to the placeholder-only wizard.

/// The wizard source MUST NOT contain any placeholder "will appear here"
/// text. The old wizard hard-coded three such labels.
#[test]
fn wizard_source_has_no_placeholder_labels() {
    let wizard = include_str!("../src/bin/wizard.rs");
    assert!(
        !wizard.contains("will appear here"),
        "wizard.rs still contains placeholder 'will appear here' text"
    );
}

/// The wizard MUST reference the real `DbusManagerClient` (or the app
/// controller that constructs it), not just construct bare labels.
#[test]
fn wizard_uses_real_client_or_app_controller() {
    let wizard = include_str!("../src/bin/wizard.rs");
    let app = include_str!("../src/app.rs");
    assert!(
        wizard.contains("app::") || app.contains("DbusManagerClient"),
        "wizard must delegate to an app controller that uses DbusManagerClient, \
         or reference DbusManagerClient directly"
    );
}

/// The app controller (or wizard) MUST instantiate `ProfilePresenter` —
/// the headless state machine that drives every destination.
#[test]
fn app_controller_uses_profile_presenter() {
    let app = include_str!("../src/app.rs");
    assert!(
        app.contains("ProfilePresenter"),
        "app controller must instantiate ProfilePresenter"
    );
}

/// The app controller MUST render the real `ProfileListView` into the
/// Profiles destination, not a placeholder label.
#[test]
fn app_controller_renders_profile_list_view() {
    let app = include_str!("../src/app.rs");
    assert!(
        app.contains("ProfileListView"),
        "app controller must use ProfileListView for the Profiles destination"
    );
}

/// The app controller MUST render the real PAM services component into the
/// Authentication destination.
#[test]
fn app_controller_renders_pam_services() {
    let app = include_str!("../src/app.rs");
    assert!(
        app.contains("render_pam_services"),
        "app controller must call render_pam_services for the Authentication destination"
    );
}

/// The app controller MUST render the real system status component into the
/// System destination.
#[test]
fn app_controller_renders_system_status() {
    let app = include_str!("../src/app.rs");
    assert!(
        app.contains("render_system_status"),
        "app controller must call render_system_status for the System destination"
    );
}

/// The app controller MUST use the `EnrollmentViewComponent` and
/// `ReviewViewComponent` for the enrollment flow, connecting callbacks to
/// the presenter v3 lifecycle methods (Start/Stop/Review/Resume/Save/Discard).
#[test]
fn app_controller_uses_enrollment_and_review_components() {
    let app = include_str!("../src/app.rs");
    assert!(
        app.contains("EnrollmentViewComponent"),
        "app controller must use EnrollmentViewComponent"
    );
    assert!(
        app.contains("ReviewViewComponent"),
        "app controller must use ReviewViewComponent"
    );
}

/// The app controller MUST call presenter v3 lifecycle methods so the
/// enrollment buttons actually drive the daemon.
#[test]
fn app_controller_calls_v3_lifecycle_methods() {
    let app = include_str!("../src/app.rs");
    for method in [
        "start_enrollment_v3",
        "stop_enrollment_v3",
        "review_enrollment_v3",
        "resume_enrollment_v3",
        "save_profile_v3",
        "discard_enrollment_v3",
    ] {
        assert!(
            app.contains(method),
            "app controller must call presenter::{method} to wire enrollment callbacks"
        );
    }
}

/// The enrollment component MUST actually connect its callbacks to the
/// buttons — the old code accepted `on_start`/`on_stop`/`on_cancel`
/// parameters but prefixed them with `_` and never wired them.
#[test]
fn enrollment_component_connects_callbacks() {
    let enrollment = include_str!("../src/components/enrollment.rs");
    // The old code had `_on_start`, `_on_stop`, `_on_cancel` — underscore
    // prefix means "unused". After the fix the callbacks must be connected
    // to button click signals.
    assert!(
        !enrollment.contains("_on_start") && !enrollment.contains("_on_stop"),
        "enrollment component must not discard on_start/on_stop callbacks (no underscore prefix)"
    );
    assert!(
        enrollment.contains("connect_clicked"),
        "enrollment component must connect button click signals to callbacks"
    );
}

/// The review component MUST actually connect its callbacks to the
/// buttons — the old code accepted `on_save`/`on_resume`/`on_discard`
/// parameters but prefixed them with `_` and never wired them.
#[test]
fn review_component_connects_callbacks() {
    let review = include_str!("../src/components/review.rs");
    assert!(
        !review.contains("_on_save") && !review.contains("_on_resume") && !review.contains("_on_discard"),
        "review component must not discard on_save/on_resume/on_discard callbacks (no underscore prefix)"
    );
    assert!(
        review.contains("connect_clicked"),
        "review component must connect button click signals to callbacks"
    );
}

/// The profile list component MUST connect the enabled toggle to a
/// callback so profile enable/disable is wired through the presenter.
/// The redesigned component uses `adw::SwitchRow` with
/// `connect_active_notify` (not the old `gtk4::CheckButton` with
/// `connect_toggled`).
#[test]
fn profile_list_component_connects_toggle() {
    let profile_list = include_str!("../src/components/profile_list.rs");
    assert!(
        profile_list.contains("connect_active_notify"),
        "profile list component must connect the SwitchRow active-notify signal to a callback"
    );
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
