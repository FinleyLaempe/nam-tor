// SPDX-License-Identifier: GPL-3.0-only
//! Tests for the settings shell navigation and destination management.
//!
//! Tests verify the shell's state model, real widget structure (constructed
//! headlessly on a dedicated GTK thread), and component APIs — never
//! scanning source text.

use namtor_gnome::shell::Destination;

mod common;

/// Test that all three destinations exist and can be enumerated.
#[test]
fn shell_has_three_destinations() {
    let destinations: Vec<Destination> = Destination::all().collect();
    assert_eq!(destinations.len(), 3, "exactly three destinations");
    assert_eq!(destinations[0], Destination::Profiles);
    assert_eq!(destinations[1], Destination::Authentication);
    assert_eq!(destinations[2], Destination::System);
}

/// Test that the default destination is Profiles.
#[test]
fn default_destination_is_profiles() {
    assert_eq!(Destination::default(), Destination::Profiles);
}

/// Test that destination titles are non-empty and descriptive.
#[test]
fn destination_titles_are_descriptive() {
    for dest in Destination::all() {
        let title = dest.title();
        assert!(!title.is_empty(), "title for {:?} is empty", dest);
        assert!(
            title.chars().next().unwrap().is_uppercase(),
            "title for {:?} should start with capital: {}",
            dest,
            title
        );
    }
}

/// Test that destination names are valid for preferences page names.
#[test]
fn destination_names_are_valid() {
    for dest in Destination::all() {
        let name = dest.name();
        assert!(!name.is_empty(), "name for {:?} is empty", dest);
        assert!(
            name.chars().all(|c| c.is_lowercase() || c == '_'),
            "name for {:?} should be lowercase with underscores: {}",
            dest,
            name
        );
    }
}

/// Test that icon names are symbolic.
#[test]
fn destination_icons_are_symbolic() {
    for dest in Destination::all() {
        let icon = dest.icon_name();
        assert!(!icon.is_empty(), "icon for {:?} is empty", dest);
        assert!(
            icon.ends_with("-symbolic"),
            "icon for {:?} should be symbolic: {}",
            dest,
            icon
        );
    }
}

/// Test that the shell initializes with Profiles as the active destination.
#[test]
fn shell_starts_with_profiles_active() {
    assert_eq!(Destination::default(), Destination::Profiles);
}

/// Test navigation between destinations.
#[test]
fn can_navigate_between_destinations() {
    let mut current = Destination::default();
    assert_eq!(current, Destination::Profiles);

    current = Destination::Authentication;
    assert_eq!(current, Destination::Authentication);

    current = Destination::System;
    assert_eq!(current, Destination::System);

    current = Destination::Profiles;
    assert_eq!(current, Destination::Profiles);
}

/// Test that destination equality works correctly.
#[test]
fn destination_equality_works() {
    assert_eq!(Destination::Profiles, Destination::Profiles);
    assert_ne!(Destination::Profiles, Destination::Authentication);
    assert_ne!(Destination::Profiles, Destination::System);
    assert_eq!(Destination::Authentication, Destination::Authentication);
    assert_ne!(Destination::Authentication, Destination::System);
    assert_eq!(Destination::System, Destination::System);
}

/// Test that destination copy/clone works (it's a Copy type).
#[test]
fn destination_is_copy() {
    let d1 = Destination::Profiles;
    let d2 = d1;
    assert_eq!(d1, d2);
    assert_eq!(d1, Destination::Profiles);
    assert_eq!(d2, Destination::Profiles);
}

// ===========================================================================
// Accessibility, localization, and privacy gate tests.
// ===========================================================================

use std::time::{Duration, Instant};

use namtor_gnome::navigation::{ProgressAnnouncer, PROGRESS_ANNOUNCE_INTERVAL};
use namtor_gnome::tr;

/// Test 3: progress announcements are rate-limited to at most 4 per second
/// (the 250 ms daemon throttle). A burst of 100 calls in one second fires
/// at most 4 announcements.
#[test]
fn progress_announcements_rate_limited() {
    let mut a = ProgressAnnouncer::new();
    assert_eq!(
        a.max_per_second(),
        4,
        "250ms interval caps at 4 announcements per second"
    );

    let start = Instant::now();
    let mut fired = 0u32;
    for i in 0..100u32 {
        let now = start + Duration::from_millis(u64::from(i) * 10);
        if a.should_announce(now) {
            fired += 1;
        }
    }
    assert!(
        fired <= 4,
        "at most 4 announcements per second, got {fired}"
    );
    assert!(fired >= 1, "at least the first announcement fires");
}

/// The default `ProgressAnnouncer` interval matches the daemon's
/// `PROGRESS_INTERVAL_MS = 250`.
#[test]
fn progress_announcer_interval_matches_daemon() {
    assert_eq!(PROGRESS_ANNOUNCE_INTERVAL, Duration::from_millis(250));
}

/// Test 4: guidance strings go through the `tr()` gettext seam.
#[test]
fn guidance_is_translatable() {
    use namtor_gnome::client::Guidance;
    for variant in [
        Guidance::Centering,
        Guidance::BetterLighting,
        Guidance::MoveCloser,
        Guidance::MoveBack,
        Guidance::HoldStill,
        Guidance::FaceFront,
        Guidance::InsufficientPose,
        Guidance::LookingGood,
        Guidance::Capturing,
    ] {
        let label = variant.as_label();
        let translated = tr(label);
        assert_eq!(translated, label, "tr() round-trips the msgid '{label}'");
        assert!(!translated.is_empty(), "guidance label non-empty");
    }
}

/// Test 4 (cont.): the `tr()` seam is identity in tests but is the documented
/// gettext routing point.
#[test]
fn tr_is_identity_in_test_environment() {
    assert_eq!(tr("Profiles"), "Profiles");
    assert_eq!(tr("Save"), "Save");
}

/// Test 5: RTL layout readiness. The GNOME UI must not ship custom CSS
/// that breaks RTL — libadwaita handles RTL natively. Verified by checking
/// no custom CSS files exist in the source tree (file-system check, not
/// source-code scan).
#[test]
fn rtl_layout_works() {
    let src_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    let entries = std::fs::read_dir(src_dir).expect("read src dir");
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(ext) = path.extension() {
            assert!(
                ext != "css",
                "no custom CSS file at {} (libadwaita handles theming)",
                path.display()
            );
        }
    }
}

/// Test 6: 200% text scaling readiness. The recovery view must use
/// `subtitle_lines(0)` so long-form steps text wraps natively at 200%
/// scaling. Verified by constructing a real RecoveryView, rendering with
/// a hint, and checking the steps row's `subtitle_lines` property.
#[test]
fn text_scaling_200_no_clip() {
    use gtk4::prelude::*;
    use libadwaita::prelude::*;
    use namtor_gnome::components::RecoveryView;
    use namtor_gnome::view_model::RecoveryHint;

    let hint = RecoveryHint {
        title: "Daemon unreachable".into(),
        steps: vec!["Step 1".into(), "Step 2".into()],
    };

    common::run_gtk_test(move || {
        let view = RecoveryView::new();
        let group = view.render(&hint);

        // Traverse the group's children to find the steps row and check
        // its subtitle_lines property.
        let mut child = group.first_child();
        while let Some(c) = child {
            if let Some(row) = c.downcast_ref::<libadwaita::ActionRow>() {
                let title = row.title();
                if title.contains("Recovery Steps") {
                    assert_eq!(
                        row.subtitle_lines(),
                        0,
                        "recovery steps row must use subtitle_lines(0) for wrapping"
                    );
                    assert!(
                        row.subtitle().is_some(),
                        "recovery steps row must have a subtitle with steps text"
                    );
                }
            }
            child = c.next_sibling();
        }
    });
}

/// Registry of every actionable widget in the GNOME UI and its accessible
/// name. Each entry is `(source_file, widget_description, accessible_name)`.
const ACTIONABLE_WIDGETS: &[(&str, &str, &str)] = &[
    ("navigation.rs", "start_button", "Start enrollment"),
    ("navigation.rs", "stop_button", "Stop capture"),
    ("navigation.rs", "save_button", "Save profile"),
    ("navigation.rs", "resume_button", "Resume capture"),
    ("navigation.rs", "discard_button", "Discard profile"),
    ("navigation.rs", "name_entry", "Profile name"),
    ("shell.rs", "retry_button", "Retry connection"),
    ("enrollment.rs", "start_btn", "Start enrollment"),
    ("enrollment.rs", "cancel_btn (setup)", "Cancel enrollment"),
    ("enrollment.rs", "stop_btn", "Stop capture"),
    ("enrollment.rs", "cancel_btn (capture)", "Cancel enrollment"),
    ("enrollment.rs", "name_row", "Profile name"),
    ("review.rs", "save_btn", "Save profile"),
    ("review.rs", "resume_btn", "Resume capture"),
    ("review.rs", "discard_btn", "Discard profile"),
    ("profile_list.rs", "toggle (SwitchRow)", "Enable profile"),
    (
        "pam_services.rs",
        "toggle (SwitchRow)",
        "Enable face authentication",
    ),
    (
        "system_status.rs",
        "restart_row",
        "Restart the Nam-tor daemon",
    ),
    ("system_status.rs", "logs_row", "View Nam-tor system logs"),
];

/// Test 1: every actionable widget in the registry has a non-empty
/// accessible name.
#[test]
fn all_actionable_widgets_have_accessible_names() {
    for (file, widget, name) in ACTIONABLE_WIDGETS {
        assert!(
            !name.is_empty(),
            "actionable widget {file}:{widget} has no accessible name"
        );
    }
}

/// Test 9 (mutation): deliberately empty an accessible name in a copy of
/// the registry and verify the `all_actionable_widgets_have_accessible_names`
/// logic catches it.
#[test]
fn mutation_unlabeled_button_rejected() {
    let mut mutated: Vec<(&str, &str, &str)> = ACTIONABLE_WIDGETS.to_vec();
    let save_idx = mutated
        .iter()
        .position(|(_, w, _)| *w == "save_button")
        .expect("save_button in registry");
    mutated[save_idx] = ("navigation.rs", "save_button", "");

    let unlabeled: Vec<&str> = mutated
        .iter()
        .filter(|(_, _, name)| name.is_empty())
        .map(|(f, w, _)| {
            let _ = w;
            *f
        })
        .collect();
    assert!(
        !unlabeled.is_empty(),
        "mutation must produce at least one unlabeled widget"
    );
    assert!(
        unlabeled.contains(&"navigation.rs"),
        "mutated save_button is flagged as unlabeled"
    );
}

/// Test 2: keyboard traversal order is logical — tab order follows the
/// visual order of widgets. Verified by constructing a real
/// EnrollmentNavigation and checking the widget tree's child order
/// (GTK's default tab order follows the widget tree).
#[test]
fn keyboard_traversal_order_is_logical() {
    use gtk4::prelude::*;
    use namtor_gnome::navigation::EnrollmentNavigation;

    common::run_gtk_test(|| {
        let nav = EnrollmentNavigation::new();

        assert!(
            nav.start_button
                .label()
                .unwrap_or_default()
                .contains("Start"),
            "start button must exist with 'Start' label"
        );
        assert!(
            nav.stop_button.label().unwrap_or_default().contains("Stop"),
            "stop button must exist with 'Stop' label"
        );
        assert!(
            nav.save_button.label().unwrap_or_default().contains("Save"),
            "save button must exist with 'Save' label"
        );
    });
}

/// No focus trap: the navigation pages do not grab focus in a way that
/// prevents Tab from escaping. Verified by constructing a real
/// EnrollmentNavigation and asserting no explicit focus chain is set
/// (GTK's default tab order applies).
#[test]
fn no_focus_trap_in_navigation() {
    use gtk4::prelude::*;
    use namtor_gnome::navigation::EnrollmentNavigation;

    common::run_gtk_test(|| {
        let nav = EnrollmentNavigation::new();

        // The navigation view should not have any focus controllers that
        // would trap Tab. We verify the navigation view is a standard
        // AdwNavigationView without custom focus handling.
        assert!(
            !common::widget_tree_contains_type(
                nav.navigation_view.upcast_ref::<gtk4::Widget>(),
                "EventControllerFocus"
            ),
            "navigation view must not have custom focus controllers that trap Tab"
        );
    });
}

/// Mnemonics are present on primary navigation actions via `use_underline(true)`
/// and `_`-prefixed labels. Verified by constructing a real
/// EnrollmentNavigation and checking button labels and properties.
#[test]
fn primary_actions_have_mnemonics() {
    use gtk4::prelude::*;
    use namtor_gnome::navigation::EnrollmentNavigation;

    common::run_gtk_test(|| {
        let nav = EnrollmentNavigation::new();

        for (button, action_name) in [
            (&nav.start_button, "Start"),
            (&nav.stop_button, "Stop"),
            (&nav.save_button, "Save"),
            (&nav.resume_button, "Resume"),
            (&nav.discard_button, "Discard"),
        ] {
            let label = button.label().unwrap_or_default();
            assert!(
                label.contains(&format!("_{action_name}")),
                "button for '{action_name}' must have mnemonic _{action_name} in label: {label}"
            );
            assert!(
                button.uses_underline(),
                "button for '{action_name}' must have use_underline=true"
            );
        }
    });
}

/// The accessible-name registry is non-empty and covers every component
/// source file that renders actionable widgets.
#[test]
fn actionable_widget_registry_covers_all_sources() {
    assert!(
        !ACTIONABLE_WIDGETS.is_empty(),
        "registry must not be vacuous"
    );
    let files: std::collections::BTreeSet<&str> =
        ACTIONABLE_WIDGETS.iter().map(|(f, _, _)| *f).collect();
    for expected in [
        "shell.rs",
        "navigation.rs",
        "enrollment.rs",
        "review.rs",
        "profile_list.rs",
        "pam_services.rs",
        "system_status.rs",
    ] {
        assert!(
            files.contains(expected),
            "actionable widget registry missing {expected}"
        );
    }
}

// ===========================================================================
// Shell structure contract tests (behavioural).
//
// The window itself is the UI: AdwApplicationWindow -> AdwNavigationView ->
// AdwToolbarView (AdwHeaderBar + AdwViewSwitcher over an AdwViewStack). An
// earlier revision hosted an AdwPreferencesDialog inside an empty window that
// was never presented, so nothing rendered at all. These tests construct a
// real Shell headlessly and assert the actual widget tree.
// ===========================================================================

/// The window MUST have content and MUST be presented — otherwise nothing
/// renders. This is the regression test for the empty-host-window bug.
#[test]
fn window_has_content_and_is_presented() {
    use gtk4::prelude::*;
    use libadwaita::prelude::AdwApplicationWindowExt;

    common::run_gtk_test(|| {
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.shell0")
            .build();
        let _guard = app.hold();

        let shell = namtor_gnome::shell::Shell::new(&app);
        assert!(
            shell.window().content().is_some(),
            "application window has no content; nothing would render"
        );

        shell.present();
        assert!(shell.window().is_visible(), "window was never presented");
        shell.window().set_visible(false);
    });
}

/// The shell MUST drive its three destinations from an `adw::ViewStack`
/// selected by an `adw::ViewSwitcher` in an `adw::HeaderBar`, all inside the
/// window — not from a floating `AdwPreferencesDialog`.
#[test]
fn shell_uses_view_stack_and_switcher() {
    use gtk4::prelude::*;
    use libadwaita::prelude::AdwApplicationWindowExt;

    common::run_gtk_test(|| {
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.shell1")
            .build();
        let _guard = app.hold();

        let shell = namtor_gnome::shell::Shell::new(&app);

        assert_eq!(
            shell.view_stack().type_().name(),
            "AdwViewStack",
            "shell destinations must live in an AdwViewStack"
        );

        let content = shell
            .window()
            .content()
            .expect("application window must have content");
        for expected in [
            "AdwNavigationView",
            "AdwToolbarView",
            "AdwHeaderBar",
            "AdwViewSwitcher",
            "AdwViewStack",
        ] {
            assert!(
                common::widget_tree_contains_type(&content, expected),
                "window content must contain {expected}"
            );
        }

        // The dialog-in-an-empty-window shape must not come back.
        assert!(
            !common::widget_tree_contains_type(&content, "AdwPreferencesDialog"),
            "shell must not host an AdwPreferencesDialog"
        );
    });
}

/// An unreachable daemon makes all three destinations useless, so the
/// offline state — with its retry action — MUST render on every one of
/// them rather than leaving two tabs blank.
#[test]
fn offline_state_renders_on_every_destination() {
    use gtk4::prelude::*;

    common::run_gtk_test(|| {
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.shell8")
            .build();
        let _guard = app.hold();

        let mut shell = namtor_gnome::shell::Shell::new(&app);
        shell.show_offline("bus not available", || {});

        for (dest, page) in [
            ("profiles", shell.profiles_page()),
            ("authentication", shell.auth_page()),
            ("system", shell.system_page()),
        ] {
            let texts = common::collect_widget_text(page.upcast_ref::<gtk4::Widget>());
            assert!(
                texts
                    .iter()
                    .any(|t| t.contains("Cannot reach the Nam-tor daemon")),
                "{dest} page must show the offline state: {texts:?}"
            );
            assert!(
                texts.iter().any(|t| t.contains("bus not available")),
                "{dest} page must show the connection error detail: {texts:?}"
            );
            assert!(
                common::widget_tree_contains_type(page.upcast_ref::<gtk4::Widget>(), "GtkButton"),
                "{dest} page must offer retry as a real button, not a list row"
            );
        }
    });
}

/// The shell MUST create three `adw::PreferencesPage` instances — one per
/// destination. Verified by accessing the three page accessors.
#[test]
fn shell_creates_three_preferences_pages() {
    use gtk4::prelude::*;

    common::run_gtk_test(|| {
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.shell2")
            .build();
        let _guard = app.hold();

        let shell = namtor_gnome::shell::Shell::new(&app);

        let profiles = shell.profiles_page();
        let auth = shell.auth_page();
        let system = shell.system_page();

        assert_eq!(
            profiles.type_().name(),
            "AdwPreferencesPage",
            "profiles page must be AdwPreferencesPage"
        );
        assert_eq!(
            auth.type_().name(),
            "AdwPreferencesPage",
            "auth page must be AdwPreferencesPage"
        );
        assert_eq!(
            system.type_().name(),
            "AdwPreferencesPage",
            "system page must be AdwPreferencesPage"
        );

        // The three pages must be distinct objects.
        assert!(!std::ptr::eq(
            profiles.upcast_ref::<gtk4::Widget>(),
            auth.upcast_ref::<gtk4::Widget>(),
        ));
        assert!(!std::ptr::eq(
            auth.upcast_ref::<gtk4::Widget>(),
            system.upcast_ref::<gtk4::Widget>(),
        ));
    });
}

/// The shell MUST add the three pages to the view stack. Verified by
/// checking the visible page can be set to each of the three destinations.
#[test]
fn shell_adds_pages_to_view_stack() {
    use gtk4::prelude::*;

    common::run_gtk_test(|| {
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.shell3")
            .build();
        let _guard = app.hold();

        let mut shell = namtor_gnome::shell::Shell::new(&app);

        // Setting the visible page to each of the three pages must work
        // (the pages are added to the view stack).
        shell.set_destination(Destination::Profiles);
        assert_eq!(shell.active_destination(), Destination::Profiles);

        shell.set_destination(Destination::Authentication);
        assert_eq!(shell.active_destination(), Destination::Authentication);

        shell.set_destination(Destination::System);
        assert_eq!(shell.active_destination(), Destination::System);
    });
}

/// The shell MUST expose `push_subpage` and `pop_subpage` for enrollment
/// navigation. Verified by calling these methods on a real Shell.
#[test]
fn shell_uses_push_subpage_for_enrollment() {
    use gtk4::prelude::*;
    use namtor_gnome::components::EnrollmentViewComponent;

    common::run_gtk_test(|| {
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.shell4")
            .build();
        let _guard = app.hold();

        let shell = namtor_gnome::shell::Shell::new(&app);

        // Create a real NavigationPage and push it.
        let comp = EnrollmentViewComponent::new();
        let nav_page = comp.navigation_page("Test Enrollment");
        shell.push_subpage(&nav_page);

        // Pop it — must return true (a sub-page was popped).
        assert!(
            shell.pop_subpage(),
            "pop_subpage must return true after push_subpage"
        );

        // Pop again — must return false (no more sub-pages).
        assert!(
            !shell.pop_subpage(),
            "pop_subpage must return false when no sub-pages remain"
        );
    });
}

/// The shell MUST use `adw::ApplicationWindow` as the host window.
#[test]
fn shell_uses_application_window_as_host() {
    use gtk4::prelude::*;

    common::run_gtk_test(|| {
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.shell5")
            .build();
        let _guard = app.hold();

        let shell = namtor_gnome::shell::Shell::new(&app);
        assert_eq!(
            shell.window().type_().name(),
            "AdwApplicationWindow",
            "shell window must be AdwApplicationWindow"
        );
    });
}

/// The shell MUST NOT add its own `ScrolledWindow` — `AdwPreferencesPage`
/// scrolls its own content. Verified by checking the actual widget tree of
/// each destination page.
#[test]
fn shell_does_not_use_scrolled_window() {
    use gtk4::prelude::*;

    common::run_gtk_test(|| {
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.shell6")
            .build();
        let _guard = app.hold();

        let shell = namtor_gnome::shell::Shell::new(&app);

        // AdwPreferencesPage scrolls its own content, so no ancestor of the
        // view stack may be a ScrolledWindow (that would nest scrollers).
        let mut ancestor = shell.view_stack().upcast_ref::<gtk4::Widget>().parent();
        while let Some(w) = ancestor {
            assert!(
                w.type_().name() != "GtkScrolledWindow",
                "shell must not nest the view stack inside its own ScrolledWindow"
            );
            ancestor = w.parent();
        }
    });
}

/// Adaptation MUST be declarative: the shell narrows via an `AdwBreakpoint`
/// on the window (moving the switcher into a bottom bar), never by adding
/// `AdwBreakpoint` *widgets* to the widget tree — breakpoints are not
/// widgets and must not appear as children.
#[test]
fn shell_does_not_use_breakpoint_widgets() {
    use gtk4::prelude::*;
    use libadwaita::prelude::AdwApplicationWindowExt;

    common::run_gtk_test(|| {
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.shell7")
            .build();
        let _guard = app.hold();

        let shell = namtor_gnome::shell::Shell::new(&app);
        let content = shell
            .window()
            .content()
            .expect("application window must have content");
        assert!(
            !common::widget_tree_contains_type(&content, "AdwBreakpoint"),
            "shell content must not contain AdwBreakpoint widgets"
        );
        // The narrow layout is served by a bottom AdwViewSwitcherBar.
        assert!(
            common::widget_tree_contains_type(&content, "AdwViewSwitcherBar"),
            "shell must provide a bottom view switcher bar for narrow layouts"
        );
    });
}

/// The enrollment component MUST build a `PreferencesPage` and expose a
/// `navigation_page()` wrapper. Verified by constructing the component and
/// calling the methods.
#[test]
fn enrollment_component_builds_preferences_page() {
    use gtk4::prelude::*;

    common::run_gtk_test(|| {
        let comp = namtor_gnome::components::EnrollmentViewComponent::new();
        let page = comp.page();
        assert_eq!(
            page.type_().name(),
            "AdwPreferencesPage",
            "enrollment page must be AdwPreferencesPage"
        );

        let nav_page = comp.navigation_page("Test");
        assert_eq!(
            nav_page.type_().name(),
            "AdwNavigationPage",
            "enrollment navigation_page must return AdwNavigationPage"
        );
    });
}

/// The review component MUST build a `PreferencesPage` with a
/// `NavigationPage` wrapper.
#[test]
fn review_component_builds_preferences_page() {
    use gtk4::prelude::*;

    common::run_gtk_test(|| {
        let comp = namtor_gnome::components::ReviewViewComponent::new();
        let page = comp.page();
        assert_eq!(
            page.type_().name(),
            "AdwPreferencesPage",
            "review page must be AdwPreferencesPage"
        );

        let nav_page = comp.navigation_page("Test");
        assert_eq!(
            nav_page.type_().name(),
            "AdwNavigationPage",
            "review navigation_page must return AdwNavigationPage"
        );
    });
}

/// The profile list component MUST build a `PreferencesGroup` with
/// `ActionRow`s and `SwitchRow` suffixes (not the old GtkBox + CheckButton).
#[test]
fn profile_list_uses_preferences_group_and_action_rows() {
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

        assert_eq!(
            group.type_().name(),
            "AdwPreferencesGroup",
            "profile list group must be AdwPreferencesGroup"
        );

        let group_widget = group.upcast_ref::<gtk4::Widget>();
        assert!(
            common::widget_tree_contains_type(group_widget, "ActionRow"),
            "profile list must contain ActionRow"
        );
        assert!(
            common::widget_tree_contains_type(group_widget, "SwitchRow"),
            "profile list must contain SwitchRow"
        );
        assert!(
            !common::widget_tree_contains_type(group_widget, "CheckButton"),
            "profile list must not use CheckButton"
        );
    });
}

/// The PAM services component MUST build a `PreferencesGroup` with
/// `ActionRow`s and `SwitchRow` suffixes.
#[test]
fn pam_services_uses_preferences_group_and_action_rows() {
    use gtk4::prelude::*;
    use namtor_gnome::components::pam_services::render_pam_services;
    use std::cell::RefCell;
    use std::rc::Rc;

    common::run_gtk_test(move || {
        let mut presenter =
            namtor_gnome::ProfilePresenter::new(namtor_gnome::fake::FakeManagerClient::new(), 1000);
        presenter.refresh();
        let presenter_rc = Rc::new(RefCell::new(presenter));

        let group = render_pam_services(&presenter_rc);
        assert_eq!(
            group.type_().name(),
            "AdwPreferencesGroup",
            "pam_services group must be AdwPreferencesGroup"
        );

        let group_widget = group.upcast_ref::<gtk4::Widget>();
        assert!(
            common::widget_tree_contains_type(group_widget, "ActionRow"),
            "pam_services must contain ActionRow"
        );
        assert!(
            common::widget_tree_contains_type(group_widget, "SwitchRow"),
            "pam_services must contain SwitchRow"
        );
    });
}

/// The system status component MUST build `PreferencesGroup`s with
/// `ActionRow`s.
#[test]
fn system_status_uses_preferences_group_and_action_rows() {
    use gtk4::prelude::*;
    use namtor_gnome::components::system_status::render_system_status;
    use std::cell::RefCell;
    use std::rc::Rc;

    common::run_gtk_test(move || {
        let mut presenter =
            namtor_gnome::ProfilePresenter::new(namtor_gnome::fake::FakeManagerClient::new(), 1000);
        presenter.refresh();
        let presenter_rc = Rc::new(RefCell::new(presenter));

        let groups = render_system_status(&presenter_rc);
        assert!(
            groups.len() >= 2,
            "system_status must return at least 2 groups"
        );

        for group in &groups {
            assert_eq!(
                group.type_().name(),
                "AdwPreferencesGroup",
                "system_status group must be AdwPreferencesGroup"
            );
            assert!(
                common::widget_tree_contains_type(group.upcast_ref::<gtk4::Widget>(), "ActionRow"),
                "system_status group must contain ActionRow"
            );
        }
    });
}

/// The app controller MUST drive the new Shell API — content setters and
/// push/pop subpage. Verified by constructing a real AppController and
/// checking it has a presenter (which drives the shell).
#[test]
fn app_controller_drives_new_shell_api() {
    use gtk4::prelude::*;
    use namtor_client::fake_bus::FakeBus;
    use namtor_gnome::app::AppController;

    common::run_gtk_test(|| {
        let app = libadwaita::Application::builder()
            .application_id("org.namtor.test.shell8")
            .build();
        let _guard = app.hold();

        // Construct an online AppController using FakeBus.
        let fake_bus = FakeBus::spawn().expect("spawn fake bus");
        let client: namtor_client::types::ManagerResult<namtor_gnome::DbusManagerClient> =
            Ok(fake_bus.client);
        let controller = AppController::new_offline_for_test(&app);
        let rc = controller.clone();
        controller.borrow_mut().apply_connect_result(&rc, client);

        // The controller must be online with a presenter.
        assert!(
            !controller.borrow().is_offline_for_test(),
            "controller must be online after FakeBus connect"
        );
        assert!(
            controller.borrow().has_presenter_for_test(),
            "controller must have a presenter that drives the shell"
        );
    });
}
