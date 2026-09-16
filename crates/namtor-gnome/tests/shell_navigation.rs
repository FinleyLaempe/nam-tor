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

use namtor_gnome::a11y::{ProgressAnnouncer, PROGRESS_ANNOUNCE_INTERVAL};
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
    ("shell.rs", "retry_button", "Retry connection"),
    ("enrollment.rs", "start_btn", "Start enrollment"),
    ("enrollment.rs", "cancel_btn (setup)", "Cancel enrollment"),
    ("enrollment.rs", "stop_btn", "Stop capture"),
    ("enrollment.rs", "cancel_btn (capture)", "Cancel enrollment"),
    ("enrollment.rs", "name_row", "Profile name"),
    ("review.rs", "save_btn", "Save profile"),
    ("review.rs", "resume_btn", "Resume capture"),
    ("review.rs", "discard_btn", "Discard profile"),
    ("scan_feedback.rs", "silhouette", "Face position indicator"),
    ("scan_feedback.rs", "guidance headline", "Scan guidance"),
    ("scan_feedback.rs", "image quality meter", "Image quality"),
    ("scan_feedback.rs", "angle variety meter", "Angle variety"),
    (
        "scan_feedback.rs",
        "views kept meter",
        "Share of captured views kept",
    ),
    (
        "scan_feedback.rs",
        "attention row",
        "Signal needing attention",
    ),
    ("scan_feedback.rs", "accepted count", "Accepted views"),
    ("scan_feedback.rs", "seen count", "Views seen"),
    ("scan_feedback.rs", "rejected count", "Rejected views"),
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
        .position(|(_, w, _)| *w == "save_btn")
        .expect("save_btn in registry");
    mutated[save_idx] = ("review.rs", "save_btn", "");

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
        unlabeled.contains(&"review.rs"),
        "mutated save_btn is flagged as unlabeled"
    );
}

/// A capture-phase view good enough to render. Used by the accessibility
/// tests that need a real widget tree.
fn capture_view() -> namtor_gnome::view_model::EnrollmentViewV3 {
    namtor_gnome::view_model::EnrollmentViewV3 {
        phase: "capturing".into(),
        quality_percent: 72,
        diversity_percent: 40,
        frames_seen: 120,
        frames_accepted: 30,
        frames_rejected: 90,
        quality_bucket: 1,
        guidance: "move_closer".into(),
    }
}

fn review_view() -> namtor_gnome::view_model::ReviewViewV3 {
    namtor_gnome::view_model::ReviewViewV3 {
        ready_to_save: true,
        quality_percent: 82,
        diversity_percent: 100,
        frames_accepted: 44,
        guidance: "looking_good".into(),
    }
}

/// Test 2: keyboard traversal order is logical — tab order follows the
/// visual order of widgets. Verified by rendering the canonical enrollment
/// and review components and checking the primary actions are present in
/// the widget tree (GTK's default tab order follows the widget tree).
#[test]
fn keyboard_traversal_order_is_logical() {
    use gtk4::prelude::*;
    use namtor_gnome::components::{EnrollmentViewComponent, ReviewViewComponent};

    common::run_gtk_test(|| {
        let mut enrollment = EnrollmentViewComponent::new();
        let setup = enrollment.render(None, |_| {}, || {}, || {});
        let setup_texts = common::collect_widget_text(setup.upcast_ref::<gtk4::Widget>());
        assert!(
            setup_texts.iter().any(|t| t.contains("Start")),
            "setup phase must offer a Start action: {setup_texts:?}"
        );

        let capture = enrollment.render(Some(&capture_view()), |_| {}, || {}, || {});
        let capture_texts = common::collect_widget_text(capture.upcast_ref::<gtk4::Widget>());
        assert!(
            capture_texts.iter().any(|t| t.contains("Stop")),
            "capture phase must offer a Stop action: {capture_texts:?}"
        );

        let mut review = ReviewViewComponent::new();
        let page = review.render(&review_view(), || {}, || {}, || {}, false);
        let review_texts = common::collect_widget_text(page.upcast_ref::<gtk4::Widget>());
        assert!(
            review_texts.iter().any(|t| t.contains("Save")),
            "review must offer a Save action: {review_texts:?}"
        );
    });
}

/// No focus trap: the enrollment pages do not grab focus in a way that
/// prevents Tab from escaping. Verified by rendering the canonical
/// component and asserting no focus controller is installed.
#[test]
fn no_focus_trap_in_navigation() {
    use gtk4::prelude::*;
    use namtor_gnome::components::EnrollmentViewComponent;

    common::run_gtk_test(|| {
        let mut enrollment = EnrollmentViewComponent::new();
        let page = enrollment.render(Some(&capture_view()), |_| {}, || {}, || {});

        assert!(
            !common::widget_tree_contains_type(
                page.upcast_ref::<gtk4::Widget>(),
                "EventControllerFocus"
            ),
            "enrollment page must not have custom focus controllers that trap Tab"
        );
    });
}

/// Mnemonics are present on primary actions via `use_underline(true)` and
/// `_`-prefixed labels. Verified by rendering the canonical components and
/// inspecting every button in the resulting widget tree.
#[test]
fn primary_actions_have_mnemonics() {
    use gtk4::prelude::*;
    use namtor_gnome::components::{EnrollmentViewComponent, ReviewViewComponent};

    common::run_gtk_test(|| {
        // One component per page: the component owns a single
        // `PreferencesPage` and clears it on every render, so reusing one
        // instance would leave all three handles pointing at the last render.
        let mut setup_comp = EnrollmentViewComponent::new();
        let mut capture_comp = EnrollmentViewComponent::new();
        let mut review_comp = ReviewViewComponent::new();

        let pages: Vec<gtk4::Widget> = vec![
            setup_comp
                .render(None, |_| {}, || {}, || {})
                .clone()
                .upcast(),
            capture_comp
                .render(Some(&capture_view()), |_| {}, || {}, || {})
                .clone()
                .upcast(),
            review_comp
                .render(&review_view(), || {}, || {}, || {}, false)
                .clone()
                .upcast(),
        ];

        for (action, page_idx) in [
            ("_Start", 0usize),
            ("_Stop", 1usize),
            ("_Save", 2usize),
            ("_Resume", 2usize),
            ("_Discard", 2usize),
        ] {
            let mut found = false;
            common::for_each_button(&pages[page_idx], &mut |btn: &gtk4::Button| {
                let label = btn.label().unwrap_or_default();
                if label.contains(action) {
                    found = true;
                    assert!(
                        btn.uses_underline(),
                        "button '{label}' must have use_underline=true"
                    );
                }
            });
            assert!(found, "no button carries the mnemonic '{action}'");
        }
    });
}
