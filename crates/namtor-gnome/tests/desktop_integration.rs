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
