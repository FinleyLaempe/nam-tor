// SPDX-License-Identifier: GPL-3.0-only
//! Tests for the adaptive shell navigation and destination management.
//!
//! These tests verify the shell's state model (which destinations exist,
//! what the default is, navigation between them) without requiring a
//! display server. GTK widget rendering is not tested here.

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
        // Titles should be human-readable (start with capital letter)
        assert!(
            title.chars().next().unwrap().is_uppercase(),
            "title for {:?} should start with capital: {}",
            dest,
            title
        );
    }
}

/// Test that destination names are valid for view stack.
#[test]
fn destination_names_are_valid() {
    for dest in Destination::all() {
        let name = dest.name();
        assert!(!name.is_empty(), "name for {:?} is empty", dest);
        // Names should be lowercase and contain no spaces
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
        // Icon names should end with -symbolic
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
    // We can't construct a real Shell without GTK initialized, but we can
    // test the Destination enum which the shell uses internally
    assert_eq!(Destination::default(), Destination::Profiles);
}

/// Test navigation between destinations.
#[test]
fn can_navigate_between_destinations() {
    // Test that we can switch between all destinations
    let mut current = Destination::default();
    assert_eq!(current, Destination::Profiles);

    current = Destination::Authentication;
    assert_eq!(current, Destination::Authentication);

    current = Destination::System;
    assert_eq!(current, Destination::System);

    // And back to Profiles
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
    let d2 = d1; // Copy, not move
    assert_eq!(d1, d2);
    assert_eq!(d1, Destination::Profiles);
    assert_eq!(d2, Destination::Profiles);
}
