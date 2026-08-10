// SPDX-License-Identifier: GPL-3.0-only
//! Nam-tor GNOME Settings App — GTK4 + libadwaita front-end.
//!
//! The binary constructs an adaptive settings shell with three destinations:
//! Profiles, Authentication, and System.
//!
//! Usage: namtor-gnome

use gio::ApplicationFlags;
use gtk4::prelude::*;
use gtk4::{Align, Label};
use libadwaita as adw;

use namtor_gnome::shell::Shell;

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
    // Create the adaptive shell
    let shell = Shell::new(app);

    // Set up Profiles page with placeholder content
    let profiles_label = Label::builder()
        .label("Profiles\n\nEnrolled profiles will appear here.")
        .halign(Align::Center)
        .valign(Align::Center)
        .css_classes(["dim-label"])
        .build();
    shell.set_profiles_content(&profiles_label);

    // Set up Authentication page with placeholder content
    let auth_label = Label::builder()
        .label("Authentication\n\nPAM service configuration will appear here.")
        .halign(Align::Center)
        .valign(Align::Center)
        .css_classes(["dim-label"])
        .build();
    shell.set_auth_content(&auth_label);

    // Set up System page with placeholder content
    let system_label = Label::builder()
        .label("System\n\nDaemon status and settings will appear here.")
        .halign(Align::Center)
        .valign(Align::Center)
        .css_classes(["dim-label"])
        .build();
    shell.set_system_content(&system_label);

    shell.present();
}
