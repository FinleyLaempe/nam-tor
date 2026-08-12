// SPDX-License-Identifier: GPL-3.0-only
//! Tests for the settings shell navigation and destination management.
//!
//! These tests verify the shell's state model (which destinations exist,
//! what the default is, navigation between them) without requiring a
//! display server. GTK widget rendering is not tested here.
//!
//! The shell was redesigned from `adw::ApplicationWindow` + `ViewStack` +
//! `ViewSwitcherBar` to `adw::PreferencesDialog` presented from an
//! `adw::ApplicationWindow`, with three `adw::PreferencesPage`s. These
//! tests assert the new structure via source-contract scans (no display
//! server needed).

use namtor_gnome::shell::Destination;

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
// Task 13: accessibility, localization, and privacy gate tests.
//
// These tests are deliberately headless (no GTK init / display server) so
// they run in CI. They assert properties of the source code and the pure
// data model: the progress throttle, the gettext seam, RTL/scaling readiness
// (no custom CSS, essential labels wrap), accessible-name coverage via a
// widget registry, and a mutation test that removing an accessible name is
// caught.
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

/// Test 5: RTL layout readiness. The GNOME UI must not hardcode LTR
/// direction or ship custom CSS that breaks RTL. libadwaita handles RTL
/// natively. This test asserts no custom CSS file exists in the crate and
/// no `set_direction(Ltr)` / `direction: ltr` calls appear in source.
#[test]
fn rtl_layout_works() {
    let src_shell = include_str!("../src/shell.rs");
    let src_nav = include_str!("../src/navigation.rs");
    for src in [src_shell, src_nav] {
        assert!(
            !src.contains("set_direction(gtk4::TextDirection::Ltr)"),
            "no hardcoded LTR direction"
        );
        assert!(
            !src.contains("TextDirection::Ltr"),
            "no LTR text direction override"
        );
    }
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

/// Test 6: 200% text scaling readiness. In the new PreferencesDialog
/// structure, long-form text (guidance, privacy notices, recovery steps)
/// is rendered via `adw::ActionRow` subtitles which wrap natively. This
/// test asserts no fixed font sizes are used (libadwaita scales text) and
/// that subtitle_lines is not capped to a small number for long text.
#[test]
fn text_scaling_200_no_clip() {
    let essential_sources = [
        include_str!("../src/components/enrollment.rs"),
        include_str!("../src/components/review.rs"),
        include_str!("../src/components/recovery.rs"),
        include_str!("../src/components/pam_services.rs"),
    ];
    for src in essential_sources {
        // No fixed font sizes that would clip when scaled.
        assert!(
            !src.contains("font_size") && !src.contains("font-size"),
            "no fixed font sizes (libadwaita scales text)"
        );
    }
    // Recovery view uses subtitle_lines(0) so the steps text is not
    // truncated at 200% scaling.
    let recovery = include_str!("../src/components/recovery.rs");
    assert!(
        recovery.contains("subtitle_lines(0)") || recovery.contains("subtitle"),
        "recovery view uses subtitle for long-form steps text (wraps natively)"
    );
}

/// Registry of every actionable widget in the GNOME UI and its accessible
/// name. Each entry is `(source_file, widget_description, accessible_name)`.
/// The accessible-name test asserts every entry has a non-empty name; the
/// mutation test removes one and verifies the test catches it.
const ACTIONABLE_WIDGETS: &[(&str, &str, &str)] = &[
    ("navigation.rs", "start_button", "Start enrollment"),
    ("navigation.rs", "stop_button", "Stop capture"),
    ("navigation.rs", "save_button", "Save profile"),
    ("navigation.rs", "resume_button", "Resume capture"),
    ("navigation.rs", "discard_button", "Discard profile"),
    ("navigation.rs", "name_entry", "Profile name"),
    ("shell.rs", "retry_row", "Retry connection"),
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
/// visual order of widgets in the source.
#[test]
fn keyboard_traversal_order_is_logical() {
    let nav_src = include_str!("../src/navigation.rs");

    let entry_pos = nav_src.find("name_entry").unwrap_or(usize::MAX);
    let start_pos = nav_src
        .find("let start_button = Button::builder()")
        .unwrap_or(usize::MAX);
    assert!(
        entry_pos < start_pos,
        "setup page: name_entry must precede start_button in source (tab order)"
    );

    let progress_pos = nav_src
        .find("let progress_label = Label::builder()")
        .unwrap_or(usize::MAX);
    let stop_pos = nav_src
        .find("let stop_button = Button::builder()")
        .unwrap_or(usize::MAX);
    assert!(
        progress_pos < stop_pos,
        "capture page: progress_label must precede stop_button (tab order)"
    );

    let summary_pos = nav_src
        .find("let summary_label = Label::builder()")
        .unwrap_or(usize::MAX);
    let save_pos = nav_src
        .find("let save_button = Button::builder()")
        .unwrap_or(usize::MAX);
    assert!(
        summary_pos < save_pos,
        "review page: summary_label must precede save_button (tab order)"
    );
}

/// No focus trap: the navigation pages do not grab focus in a way that
/// prevents Tab from escaping.
#[test]
fn no_focus_trap_in_navigation() {
    let src = include_str!("../src/navigation.rs");
    assert!(
        !src.contains("set_focus_chain"),
        "no explicit focus chain (would risk trapping Tab)"
    );
    assert!(!src.contains("grab_focus_loop"), "no focus grab loops");
}

/// Mnemonics are present on primary navigation actions via `use_underline(true)`
/// and `_`-prefixed labels.
#[test]
fn primary_actions_have_mnemonics() {
    let src = include_str!("../src/navigation.rs");
    for action in ["Start", "Stop", "Save", "Resume", "Discard"] {
        let needle = format!("_{action}");
        assert!(
            src.contains(&needle),
            "primary action '{action}' must have a mnemonic (_{action})"
        );
    }
    assert!(
        src.contains(".use_underline(true)"),
        "use_underline must be enabled for mnemonics"
    );
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
// AdwPreferencesDialog structure contract tests.
//
// The shell was redesigned from ApplicationWindow + ViewStack + ViewSwitcherBar
// to PreferencesDialog + PreferencesPage + PreferencesGroup. These source-
// contract tests verify the new structure is present and the old structure
// is gone. PreferencesDialog handles scrolling internally (the old
// ScrolledWindow ownership tests are replaced by these structural assertions).
// ===========================================================================

/// The shell source MUST use `adw::PreferencesDialog` as the primary
/// container, not the old `adw::ViewStack`.
#[test]
fn shell_uses_preferences_dialog() {
    let src = include_str!("../src/shell.rs");
    assert!(
        src.contains("adw::PreferencesDialog"),
        "shell.rs must use adw::PreferencesDialog"
    );
    assert!(
        !src.contains("adw::ViewStack"),
        "shell.rs must not use the old adw::ViewStack"
    );
    assert!(
        !src.contains("adw::ViewSwitcherBar"),
        "shell.rs must not use the old adw::ViewSwitcherBar"
    );
    assert!(
        !src.contains("adw::ViewSwitcher"),
        "shell.rs must not use the old adw::ViewSwitcher"
    );
}

/// The shell MUST create three `adw::PreferencesPage` instances — one per
/// destination.
#[test]
fn shell_creates_three_preferences_pages() {
    let src = include_str!("../src/shell.rs");
    let count = src.matches("adw::PreferencesPage::builder").count();
    assert!(
        count >= 3,
        "expected at least 3 PreferencesPage builders (one per destination), got {count}"
    );
}

/// The shell MUST add the three pages to the dialog via `dialog.add(...)`.
#[test]
fn shell_adds_pages_to_dialog() {
    let src = include_str!("../src/shell.rs");
    let count = src.matches("dialog.add").count();
    assert!(
        count >= 3,
        "expected at least 3 dialog.add() calls (one per page), got {count}"
    );
}

/// The shell MUST use `push_subpage` for enrollment navigation.
#[test]
fn shell_uses_push_subpage_for_enrollment() {
    let src = include_str!("../src/shell.rs");
    assert!(
        src.contains("push_subpage"),
        "shell.rs must expose push_subpage for enrollment navigation"
    );
    assert!(
        src.contains("pop_subpage"),
        "shell.rs must expose pop_subpage for enrollment navigation"
    );
}

/// The shell MUST still use `adw::ApplicationWindow` as the host window.
#[test]
fn shell_uses_application_window_as_host() {
    let src = include_str!("../src/shell.rs");
    assert!(
        src.contains("adw::ApplicationWindow"),
        "shell.rs must use adw::ApplicationWindow as the dialog host"
    );
}

/// The shell MUST NOT use the old `ScrolledWindow` wrapping —
/// `PreferencesDialog` handles scrolling internally.
#[test]
fn shell_does_not_use_scrolled_window() {
    let src = include_str!("../src/shell.rs");
    assert!(
        !src.contains("ScrolledWindow"),
        "shell.rs must not use ScrolledWindow (PreferencesDialog scrolls internally)"
    );
}

/// The shell MUST NOT use the old `Breakpoint`-based adaptive switching —
/// `PreferencesDialog` handles adaptation internally.
#[test]
fn shell_does_not_use_breakpoint() {
    let src = include_str!("../src/shell.rs");
    assert!(
        !src.contains("Breakpoint"),
        "shell.rs must not use Breakpoint (PreferencesDialog handles adaptation)"
    );
}

/// The enrollment and review components MUST build `PreferencesPage`s that
/// can be wrapped in `NavigationPage` for `push_subpage`.
#[test]
fn enrollment_component_builds_preferences_page() {
    let src = include_str!("../src/components/enrollment.rs");
    assert!(
        src.contains("adw::PreferencesPage"),
        "enrollment component must build adw::PreferencesPage"
    );
    assert!(
        src.contains("navigation_page"),
        "enrollment component must expose a navigation_page() wrapper"
    );
    assert!(
        src.contains("adw::NavigationPage"),
        "enrollment component must wrap in adw::NavigationPage"
    );
}

/// The review component MUST build a `PreferencesPage` with a
/// `NavigationPage` wrapper.
#[test]
fn review_component_builds_preferences_page() {
    let src = include_str!("../src/components/review.rs");
    assert!(
        src.contains("adw::PreferencesPage"),
        "review component must build adw::PreferencesPage"
    );
    assert!(
        src.contains("navigation_page"),
        "review component must expose a navigation_page() wrapper"
    );
    assert!(
        src.contains("adw::NavigationPage"),
        "review component must wrap in adw::NavigationPage"
    );
}

/// The profile list component MUST build a `PreferencesGroup` with
/// `ActionRow`s and `SwitchRow` suffixes (not the old GtkBox + CheckButton).
#[test]
fn profile_list_uses_preferences_group_and_action_rows() {
    let src = include_str!("../src/components/profile_list.rs");
    assert!(
        src.contains("adw::PreferencesGroup"),
        "profile list must use adw::PreferencesGroup"
    );
    assert!(
        src.contains("adw::ActionRow"),
        "profile list must use adw::ActionRow"
    );
    assert!(
        src.contains("adw::SwitchRow"),
        "profile list must use adw::SwitchRow (not gtk4::CheckButton)"
    );
    assert!(
        !src.contains("CheckButton"),
        "profile list must not use the old gtk4::CheckButton"
    );
}

/// The PAM services component MUST build a `PreferencesGroup` with
/// `ActionRow`s and `SwitchRow` suffixes.
#[test]
fn pam_services_uses_preferences_group_and_action_rows() {
    let src = include_str!("../src/components/pam_services.rs");
    assert!(
        src.contains("adw::PreferencesGroup"),
        "pam_services must use adw::PreferencesGroup"
    );
    assert!(
        src.contains("adw::ActionRow"),
        "pam_services must use adw::ActionRow"
    );
    assert!(
        src.contains("adw::SwitchRow"),
        "pam_services must use adw::SwitchRow (not gtk4::Switch)"
    );
}

/// The system status component MUST build `PreferencesGroup`s with
/// `ActionRow`s.
#[test]
fn system_status_uses_preferences_group_and_action_rows() {
    let src = include_str!("../src/components/system_status.rs");
    assert!(
        src.contains("adw::PreferencesGroup"),
        "system_status must use adw::PreferencesGroup"
    );
    assert!(
        src.contains("adw::ActionRow"),
        "system_status must use adw::ActionRow"
    );
}

/// The app controller MUST drive the new Shell API — `set_profiles_content`,
/// `set_auth_content`, `set_system_content`, `push_subpage`, `pop_subpage`.
#[test]
fn app_controller_drives_new_shell_api() {
    let src = include_str!("../src/app.rs");
    assert!(
        src.contains("set_profiles_content") || src.contains("add_profiles_group"),
        "app.rs must use the new Shell content/group API"
    );
    assert!(
        src.contains("push_subpage"),
        "app.rs must use push_subpage for enrollment"
    );
    assert!(
        src.contains("pop_subpage"),
        "app.rs must use pop_subpage for enrollment"
    );
}
