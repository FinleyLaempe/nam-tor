// SPDX-License-Identifier: GPL-3.0-only
//! Nam-tor Settings App — GTK4 + libadwaita front-end.
//!
//! The binary constructs an [`AppController`] which wires the adaptive
//! settings shell to the live D-Bus daemon via `ProfilePresenter` and the
//! real UI components. If the daemon is unreachable the controller shows
//! the shell's offline status page with a retry action — no placeholder
//! labels.
//!
//! Usage: namtor-enroll-wizard

use gio::ApplicationFlags;
use gtk4::prelude::*;
use libadwaita as adw;

use namtor_ui::app::AppController;

const USAGE: &str = "\
Usage: namtor-enroll-wizard

Nam-tor management UI (GTK4/libadwaita). Takes no arguments.

Options:
  -h, --help       Print this help and exit
  -V, --version    Print the version and exit";

fn main() -> std::process::ExitCode {
    // MUST precede adw::Application::builder() so --version works headlessly.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = namtor_layout::cli_preamble::handle(
        "namtor-enroll-wizard",
        env!("CARGO_PKG_VERSION"),
        USAGE,
        &args,
    ) {
        return std::process::ExitCode::from(code);
    }
    // Load the message catalog before any string is rendered. `tr()` is the
    // gettext seam for every user-facing string; until now nothing ever
    // called into gettext, so the seam could not be used by a distributor
    // shipping a .mo. A missing catalog is the ordinary case today and falls
    // through to the English msgid.
    namtor_ui::init_translations();

    // KDE's GTK-config module writes the legacy prefer-dark-theme flag into
    // ~/.config/gtk-4.0/settings.ini. libadwaita ignores that flag and warns
    // about it the moment its style manager is constructed inside
    // `adw_init()` — before any application callback runs. Initialising GTK
    // here lets the flag be cleared first, so the app is driven purely by
    // `AdwStyleManager:color-scheme` (set in `build_ui`) with no warning and
    // no second, conflicting dark-theme mechanism.
    clear_legacy_dark_theme_flag();

    let app = adw::Application::builder()
        .application_id(namtor_ui::APP_ID)
        .flags(ApplicationFlags::FLAGS_NONE)
        .build();

    app.connect_activate(build_ui);
    app.run();
    std::process::ExitCode::SUCCESS
}

/// Clear `GtkSettings:gtk-application-prefer-dark-theme` before libadwaita's
/// style manager reads it. Best-effort: if GTK cannot be initialised (no
/// display) there is nothing to clear and `app.run()` reports the real error.
fn clear_legacy_dark_theme_flag() {
    if gtk4::init().is_err() {
        return;
    }
    let Some(display) = gtk4::gdk::Display::default() else {
        return;
    };
    gtk4::Settings::for_display(&display).set_gtk_application_prefer_dark_theme(false);
}

fn build_ui(app: &adw::Application) {
    // Follow the desktop's colour scheme via the freedesktop appearance
    // portal (`org.freedesktop.appearance color-scheme`), which works on
    // GNOME, Plasma, Sway and others. KDE instead writes the legacy
    // GtkSettings prefer-dark-theme flag, which libadwaita ignores;
    // ForceDark / ForceLight would pin the app to one scheme and ignore the
    // desktop entirely.
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::Default);
    let controller = AppController::build(app);
    controller.borrow().present();
}
