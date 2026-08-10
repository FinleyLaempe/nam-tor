// SPDX-License-Identifier: GPL-3.0-only
//! Nam-tor GNOME Settings App — GTK4 + libadwaita front-end.
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

use namtor_gnome::app::AppController;

fn main() -> std::process::ExitCode {
    let app = adw::Application::builder()
        .application_id("org.namtor.Gnome")
        .flags(ApplicationFlags::FLAGS_NONE)
        .build();

    app.connect_activate(build_ui);
    app.run();
    std::process::ExitCode::SUCCESS
}

fn build_ui(app: &adw::Application) {
    let controller = AppController::build(app);
    controller.borrow().present();
}
