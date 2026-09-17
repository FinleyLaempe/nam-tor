// SPDX-License-Identifier: GPL-3.0-only
//! Desktop-integration tests: the app must follow the *desktop's* colour
//! scheme on GNOME, Plasma, Sway and anywhere else.
//!
//! KDE sets `GtkSettings:gtk-application-prefer-dark-theme`, which libadwaita
//! ignores (and warns about). The portable mechanism is
//! `AdwStyleManager:color-scheme`, which reads the freedesktop
//! `org.freedesktop.appearance color-scheme` portal.

mod common;

const WIZARD_SRC: &str = include_str!("../src/bin/wizard.rs");

/// The wizard must drive its appearance from `AdwStyleManager`, never from
/// the legacy `GtkSettings` prefer-dark-theme flag.
///
/// Note the deviation from the plan, which asserted the *string*
/// `gtk-application-prefer-dark-theme` never appears in the source: the
/// wizard has to name that property in order to *clear* it. libadwaita emits
/// its `Adwaita-WARNING` from `adw_init()`, before any application callback
/// runs, whenever KDE's `~/.config/gtk-4.0/settings.ini` has the flag set —
/// so setting a colour scheme in `build_ui` alone cannot silence it.
#[test]
fn startup_does_not_use_gtk_prefer_dark_theme() {
    assert!(
        !WIZARD_SRC.contains("set_gtk_application_prefer_dark_theme(true)"),
        "use AdwStyleManager::color-scheme, not the deprecated GtkSettings property"
    );
    assert!(
        WIZARD_SRC.contains("set_gtk_application_prefer_dark_theme(false)"),
        "wizard must clear the legacy flag before libadwaita's style manager reads it"
    );
    assert!(
        WIZARD_SRC.contains("StyleManager"),
        "wizard must set an explicit AdwStyleManager colour scheme"
    );
}

/// The colour scheme must be `Default` — following the desktop — rather than
/// `ForceDark` / `ForceLight`, which would pin the app to one appearance and
/// ignore the session.
#[test]
fn colour_scheme_follows_the_desktop_rather_than_being_forced() {
    assert!(
        WIZARD_SRC.contains("ColorScheme::Default"),
        "wizard must follow the desktop colour scheme"
    );
    for forced in ["ColorScheme::ForceDark", "ColorScheme::ForceLight"] {
        assert!(
            !WIZARD_SRC.contains(forced),
            "wizard must not pin the appearance with {forced}"
        );
    }
}

/// Behavioural check: `AdwStyleManager` accepts the `Default` colour scheme
/// and reports it back, so the portal-driven path is actually live.
#[test]
fn style_manager_accepts_the_default_colour_scheme() {
    common::run_gtk_test(|| {
        let manager = libadwaita::StyleManager::default();
        manager.set_color_scheme(libadwaita::ColorScheme::Default);
        assert_eq!(
            manager.color_scheme(),
            libadwaita::ColorScheme::Default,
            "style manager must follow the desktop colour scheme"
        );
    });
}

// ---------------------------------------------------------------------------
// Application id <-> desktop entry
//
// The app id is ABI for installed systems: it is the basename of the
// installed `.desktop` file, the `StartupWMClass` the compositor matches
// against, and the D-Bus name GTK claims for window activation. It does not
// track the crate name, which is why renaming the crate to `namtor-ui` left
// it at `org.namtor.Wizard`.
// ---------------------------------------------------------------------------

const APP_ID: &str = namtor_ui::APP_ID;
const DESKTOP_ENTRY: &str = "../../packaging/applications/org.namtor.Wizard.desktop";

#[test]
fn app_id_matches_desktop_entry_basename() {
    let entry =
        std::path::Path::new("../../packaging/applications").join(format!("{APP_ID}.desktop"));
    assert!(
        entry.exists(),
        "no desktop entry for app id {APP_ID}: {entry:?}"
    );
}

#[test]
fn app_id_matches_startup_wm_class() {
    let txt = std::fs::read_to_string(DESKTOP_ENTRY).expect("read desktop entry");
    assert!(
        txt.lines()
            .any(|l| l.trim() == format!("StartupWMClass={APP_ID}")),
        "StartupWMClass does not match app id {APP_ID}"
    );
}

#[test]
fn wizard_builds_its_application_with_the_shared_app_id() {
    assert!(
        WIZARD_SRC.contains(".application_id(namtor_ui::APP_ID)"),
        "the wizard must construct its application from namtor_ui::APP_ID, \
         not a literal that can drift from the desktop entry"
    );
}

/// The UI is desktop-neutral: GTK4/libadwaita with no GNOME-only runtime
/// dependency. Pinning `Categories` to GNOME hides the app from every other
/// desktop's settings search, and `X-GNOME-*` keys are dead weight there.
#[test]
fn desktop_entry_is_not_desktop_specific() {
    let txt = std::fs::read_to_string(DESKTOP_ENTRY).expect("read desktop entry");
    for line in txt.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(cats) = line.strip_prefix("Categories=") {
            assert!(
                !cats.contains("GNOME"),
                "Categories pins the app to GNOME: {cats}"
            );
        }
        assert!(!line.starts_with("X-GNOME-"), "GNOME-specific key: {line}");
    }
}

// --- the camera row reports a probe, not an inference -----------------------

/// An empty sysfs must report no camera, whatever the daemon is doing.
///
/// `camera_detected` was `daemon_reachable`, so the System page answered
/// "IR Camera — Detected" on a machine with no camera at all and gave no clue
/// why enrollment then failed.
#[test]
fn no_usb_devices_means_no_camera() {
    let sysfs = tempfile::tempdir().unwrap();
    let quirks = tempfile::tempdir().unwrap();
    assert!(!namtor_ui::supported_camera_present_in(
        sysfs.path(),
        quirks.path()
    ));
}

/// A camera whose vid/pid is not in the quirks database is not supported, so
/// it must not be reported as detected.
#[test]
fn an_unsupported_camera_is_not_reported_as_detected() {
    let sysfs = tempfile::tempdir().unwrap();
    let dev = sysfs.path().join("bus/usb/devices/1-1");
    std::fs::create_dir_all(&dev).unwrap();
    std::fs::write(dev.join("idVendor"), "dead\n").unwrap();
    std::fs::write(dev.join("idProduct"), "beef\n").unwrap();
    std::fs::write(dev.join("bcdDevice"), "0100\n").unwrap();
    std::fs::write(dev.join("descriptors"), b"").unwrap();

    let quirks = tempfile::tempdir().unwrap();
    // A quirks entry for a DIFFERENT device.
    std::fs::write(quirks.path().join("3277-0018.toml"), quirks_fixture()).unwrap();

    assert!(
        !namtor_ui::supported_camera_present_in(sysfs.path(), quirks.path()),
        "a device with no quirks entry must not count as a supported camera"
    );
}

/// An unreadable quirks directory reports "not detected" rather than taking
/// the page down: this drives one status row.
#[test]
fn an_unreadable_quirks_dir_is_not_fatal() {
    let sysfs = tempfile::tempdir().unwrap();
    assert!(!namtor_ui::supported_camera_present_in(
        sysfs.path(),
        std::path::Path::new("/nonexistent/quirks")
    ));
}

/// A supported camera whose quirks entry has no bcdDevice qualifier must be
/// detected.
///
/// `QuirksDb::match_device` documents that a bcd-qualified lookup never falls
/// back to the plain `vid:pid` key, and most entries — including the
/// reference camera's — are keyed without a bcd. Querying with `Some(bcd)`
/// alone reported "no camera" on a machine that has one attached, which is
/// worse than the inference it replaced.
#[test]
fn a_supported_camera_keyed_without_bcd_is_detected() {
    let sysfs = tempfile::tempdir().unwrap();
    let dev = sysfs.path().join("bus/usb/devices/3-8");
    std::fs::create_dir_all(&dev).unwrap();
    std::fs::write(dev.join("idVendor"), "3277\n").unwrap();
    std::fs::write(dev.join("idProduct"), "0018\n").unwrap();
    std::fs::write(dev.join("bcdDevice"), "0100\n").unwrap();
    std::fs::write(dev.join("descriptors"), b"").unwrap();

    // A streaming node on interface 2, as a sibling interface directory.
    let iface = sysfs
        .path()
        .join("bus/usb/devices/3-8:1.2/video4linux/video2");
    std::fs::create_dir_all(&iface).unwrap();
    std::fs::write(iface.join("name"), "USB2.0 IR Camera\n").unwrap();

    let quirks = tempfile::tempdir().unwrap();
    std::fs::write(quirks.path().join("3277-0018.toml"), quirks_fixture()).unwrap();

    assert!(
        namtor_ui::supported_camera_present_in(sysfs.path(), quirks.path()),
        "the reference camera's entry is keyed 3277:0018 with no bcd qualifier"
    );
}

/// The real reference quirks entry, read from the repo.
///
/// Hand-rolling one drifts: `QuirksEntry` requires `device.name` and a
/// `nodes` section, and `load_quirks_dir` fails the entire database if any
/// single file fails to parse — so a fixture missing a field reads as "no
/// camera" and these tests would pass or fail for the wrong reason.
fn quirks_fixture() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../quirks/devices/3277-0018.toml");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// --- the translation seam is real -------------------------------------------

/// `tr()` must return the msgid unchanged when no catalog is present, which
/// is every current install — the repo ships no `.po`/`.mo` files.
#[test]
fn tr_falls_through_to_the_msgid_without_a_catalog() {
    assert_eq!(namtor_ui::tr("Enroll Face"), "Enroll Face");
    assert_eq!(namtor_ui::tr(""), "");
}

/// A locale directory with no catalog for this app must not error, and must
/// not leave the UI showing anything other than English.
#[test]
fn a_missing_catalog_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("NAMTOR_LOCALE_DIR", dir.path());
    std::env::set_var("LC_ALL", "de_DE.UTF-8");
    // init_translations is a OnceLock, so this may be a no-op if another test
    // in this binary got there first; either way it must not panic and tr()
    // must keep working.
    let _ = namtor_ui::init_translations();
    assert_eq!(namtor_ui::tr("Enroll Face"), "Enroll Face");
    std::env::remove_var("NAMTOR_LOCALE_DIR");
    std::env::remove_var("LC_ALL");
}
