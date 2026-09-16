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
