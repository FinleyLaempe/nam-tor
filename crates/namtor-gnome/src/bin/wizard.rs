// SPDX-License-Identifier: GPL-3.0-only
//! Nam-tor enrollment wizard — GTK4 + libadwaita application.
//!
//! Spec §9.2: GTK4 + libadwaita, Rust. Enrollment wizard, device status,
//! enrollment deletion.
//!
//! Usage: namtor-enroll-wizard

use std::process::ExitCode;

use gio::ApplicationFlags;
use gtk4::prelude::*;
use gtk4::{Align, ApplicationWindow, Box as GtkBox, Label, Orientation, ScrolledWindow};
use libadwaita as adw;

use namtor_gnome::{default_key_path, default_store_dir, store::UiStore};

fn main() -> ExitCode {
    let app = adw::Application::builder()
        .application_id("org.namtor.Wizard")
        .flags(ApplicationFlags::FLAGS_NONE)
        .build();

    app.connect_activate(build_ui);
    app.run();
    ExitCode::SUCCESS
}

fn build_ui(app: &adw::Application) {
    let store_dir = default_store_dir();
    let key_path = default_key_path();

    let store = match UiStore::open(&store_dir, &key_path) {
        Ok(s) => s,
        Err(e) => {
            let win = ApplicationWindow::builder()
                .application(app)
                .title("Nam-tor — Error")
                .build();
            let label = Label::builder()
                .label(format!(
                    "Failed to open template store:\n{e}\n\nPath: {}",
                    store_dir.display()
                ))
                .margin_top(24)
                .margin_bottom(24)
                .margin_start(24)
                .margin_end(24)
                .build();
            win.set_child(Some(&label));
            win.present();
            return;
        }
    };

    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new("Nam-tor", "Face Authentication"))
        .build();

    let content = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .margin_start(12)
        .margin_end(12)
        .margin_top(12)
        .margin_bottom(12)
        .spacing(8)
        .build();

    let title = Label::builder()
        .label("Enrolled Users")
        .halign(Align::Start)
        .css_classes(["title"])
        .build();
    content.append(&title);

    let users_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .build();

    match store.list() {
        Ok(users) if users.is_empty() => {
            let l = Label::builder()
                .label("No enrollments yet.")
                .halign(Align::Start)
                .build();
            users_box.append(&l);
        }
        Ok(users) => {
            for u in users {
                let text = format!(
                    "• {} (uid {}) — {} templates, model: {}",
                    u.username, u.uid, u.num_templates, u.model_id
                );
                let l = Label::builder().label(&text).halign(Align::Start).build();
                users_box.append(&l);
            }
        }
        Err(e) => {
            let l = Label::builder()
                .label(format!("Error: {e}"))
                .halign(Align::Start)
                .build();
            users_box.append(&l);
        }
    }

    let scrolled = ScrolledWindow::builder()
        .child(&users_box)
        .vexpand(true)
        .build();
    content.append(&scrolled);

    let note = Label::builder()
        .label("To enroll a new user, run:\nnamtor-cli enroll <username>")
        .halign(Align::Start)
        .css_classes(["dim-label", "caption"])
        .build();
    content.append(&note);

    let toolbar_view = adw::ToolbarView::builder().build();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Nam-tor Enrollment")
        .default_width(480)
        .default_height(360)
        .content(&toolbar_view)
        .build();

    window.present();
}
