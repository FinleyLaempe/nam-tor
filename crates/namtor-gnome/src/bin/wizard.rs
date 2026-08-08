// SPDX-License-Identifier: GPL-3.0-only
//! Nam-tor management UI — GTK4 + libadwaita front-end for the
//! `org.namtor.Manager` (v1) and `org.namtor.Manager2` (v2) D-Bus services
//! on the **system bus** (spec §9.1, §9.2, task 14).
//!
//! The binary constructs a [`ProfilePresenter`] over a live
//! [`DbusManagerClient`] and renders the resulting [`PresenterModel`] as a
//! GTK4 window. All biometric data stays in the daemon; the UI only ever
//! sees the coarse types in [`namtor_gnome::client`].
//!
//! Usage: namtor-enroll-wizard

use std::cell::RefCell;
use std::process::ExitCode;
use std::rc::Rc;

use gio::ApplicationFlags;
use gtk4::prelude::*;
use gtk4::{AccessibleRole, Align, Box as GtkBox, Label, Orientation, ScrolledWindow};
use libadwaita as adw;
use libadwaita::prelude::AdwApplicationWindowExt;

use namtor_gnome::components::pam_services::render_pam_services;
use namtor_gnome::presenter::ProfilePresenter;
use namtor_gnome::DbusManagerClient;

/// Get the current user's uid. SAFETY: `getuid()` is always safe to call.
fn current_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid() is a POSIX function that never fails and has no
    // preconditions.
    unsafe { getuid() }
}

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

    let status_label = Label::builder()
        .halign(Align::Start)
        .css_classes(["dim-label", "caption"])
        .build();
    content.append(&status_label);

    let title = Label::builder()
        .label("Enrolled Profiles")
        .halign(Align::Start)
        .css_classes(["title"])
        .build();
    content.append(&title);

    let profiles_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .build();
    let scrolled = ScrolledWindow::builder()
        .child(&profiles_box)
        .vexpand(true)
        .build();
    content.append(&scrolled);

    let enrollment_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .build();
    content.append(&enrollment_box);

    let pam_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .build();
    content.append(&pam_box);

    let protection_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .build();
    content.append(&protection_box);

    let error_label = Label::builder()
        .halign(Align::Start)
        .css_classes(["error"])
        .wrap(true)
        .build();
    content.append(&error_label);

    let message_label = Label::builder()
        .halign(Align::Start)
        .css_classes(["success"])
        .wrap(true)
        .build();
    content.append(&message_label);

    let toolbar_view = adw::ToolbarView::builder().build();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Nam-tor — Face Profiles")
        .default_width(560)
        .default_height(560)
        .content(&toolbar_view)
        .build();

    let presenter = match DbusManagerClient::connect() {
        Ok(client) => ProfilePresenter::new(client, current_uid()),
        Err(err) => {
            render_unreachable(&window, &err);
            window.present();
            return;
        }
    };
    let presenter_rc = Rc::new(RefCell::new(presenter));
    {
        let mut p = presenter_rc.borrow_mut();
        p.refresh();
    }

    render(
        &profiles_box,
        &enrollment_box,
        &protection_box,
        &status_label,
        &error_label,
        &message_label,
        &presenter_rc.borrow().model().clone(),
    );
    render_pam_services(&pam_box, &presenter_rc);

    let enroll_box = enrollment_box.clone();
    let profiles_box_clone = profiles_box.clone();
    let protection_clone = protection_box.clone();
    let status_clone = status_label.clone();
    let error_clone = error_label.clone();
    let message_clone = message_label.clone();
    let rc_for_timer = Rc::clone(&presenter_rc);
    glib::source::timeout_add_local(std::time::Duration::from_millis(500), move || {
        let mut p = rc_for_timer.borrow_mut();
        if p.model().enrollment.is_some() {
            p.poll_progress();
            let m = p.model().clone();
            drop(p);
            render(
                &profiles_box_clone,
                &enroll_box,
                &protection_clone,
                &status_clone,
                &error_clone,
                &message_clone,
                &m,
            );
        }
        glib::ControlFlow::Continue
    });

    window.present();
}

#[allow(clippy::too_many_arguments)]
fn render(
    profiles_box: &GtkBox,
    enrollment_box: &GtkBox,
    protection_box: &GtkBox,
    status_label: &Label,
    error_label: &Label,
    message_label: &Label,
    model: &namtor_gnome::PresenterModel,
) {
    while let Some(child) = profiles_box.first_child() {
        profiles_box.remove(&child);
    }
    if model.profiles.is_empty() {
        let l = Label::builder()
            .label("No profiles enrolled yet.")
            .halign(Align::Start)
            .build();
        profiles_box.append(&l);
    } else {
        for row in &model.profiles {
            let row_box = GtkBox::builder()
                .orientation(Orientation::Horizontal)
                .spacing(8)
                .build();
            let name = Label::builder()
                .label(&row.name)
                .halign(Align::Start)
                .hexpand(true)
                .accessible_role(AccessibleRole::ListItem)
                .build();
            let toggle = gtk4::CheckButton::builder()
                .label("Enabled")
                .active(row.enabled)
                .accessible_role(AccessibleRole::Checkbox)
                .build();
            let last = Label::builder()
                .label(format!(
                    "last used: {}",
                    if row.last_used == 0 {
                        "never".to_string()
                    } else {
                        format!("epoch {}", row.last_used)
                    }
                ))
                .css_classes(["dim-label", "caption"])
                .build();
            row_box.append(&name);
            row_box.append(&toggle);
            row_box.append(&last);
            profiles_box.append(&row_box);
        }
    }

    while let Some(child) = enrollment_box.first_child() {
        enrollment_box.remove(&child);
    }
    if let Some(enroll) = &model.enrollment {
        let frame = gtk4::Frame::builder().label("Enrollment").build();
        let inner = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        let progress = Label::builder()
            .label(format!(
                "Frames captured: {}  ·  Coverage: {}%  ·  Quality: {}",
                enroll.frames_captured, enroll.coverage_percent, enroll.quality.label
            ))
            .halign(Align::Start)
            .build();
        inner.append(&progress);
        if enroll.quality.show_guidance {
            let guidance = Label::builder()
                .label("Low diversity — move closer, then slowly turn your head left and right.")
                .halign(Align::Start)
                .css_classes(["warning"])
                .wrap(true)
                .accessible_role(AccessibleRole::Alert)
                .build();
            inner.append(&guidance);
        }
        let bar = gtk4::ProgressBar::builder()
            .fraction(enroll.coverage_percent as f64 / 100.0)
            .build();
        inner.append(&bar);
        frame.set_child(Some(&inner));
        enrollment_box.append(&frame);
    }

    while let Some(child) = protection_box.first_child() {
        protection_box.remove(&child);
    }
    if let Some(protection) = &model.protection {
        let prot_title = Label::builder()
            .label("Template Protection")
            .halign(Align::Start)
            .css_classes(["heading"])
            .build();
        protection_box.append(&prot_title);
        let sealing = format!(
            "Sealing: {} (TPM: {})",
            protection.sealing.as_str(),
            if protection.has_tpm { "yes" } else { "no" }
        );
        let row = Label::builder()
            .label(&sealing)
            .halign(Align::Start)
            .css_classes(["body"])
            .build();
        protection_box.append(&row);
        if !protection.warning.is_empty() {
            let warn = Label::builder()
                .label(&protection.warning)
                .halign(Align::Start)
                .css_classes(["warning"])
                .wrap(true)
                .build();
            protection_box.append(&warn);
        }
    }

    if let Some(status) = &model.status {
        status_label.set_label(&format!(
            "API v{}  ·  {} profile(s)  ·  Emitter {}",
            status.api_version,
            status.profiles.len(),
            if status.emitter_armed {
                "armed"
            } else {
                "disarmed"
            },
        ));
    } else if model.is_loading {
        status_label.set_label("Loading...");
    } else {
        status_label.set_label("Daemon status unknown");
    }

    if let Some(err) = &model.error {
        let mut text = format!("{}\n{}", err.title, err.detail);
        if let Some(rec) = &err.recovery {
            text.push_str("\n\nRecovery:\n");
            for step in &rec.steps {
                text.push_str("• ");
                text.push_str(step);
                text.push('\n');
            }
        }
        error_label.set_label(&text);
        error_label.set_visible(true);
    } else {
        error_label.set_label("");
        error_label.set_visible(false);
    }

    if let Some(msg) = &model.message {
        message_label.set_label(msg);
        message_label.set_visible(true);
    } else {
        message_label.set_label("");
        message_label.set_visible(false);
    }
}

fn render_unreachable(window: &adw::ApplicationWindow, err: &namtor_gnome::ManagerError) {
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new("Nam-tor", "Daemon Unavailable"))
        .build();
    let content = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .margin_start(24)
        .margin_end(24)
        .margin_top(24)
        .margin_bottom(24)
        .spacing(12)
        .build();
    let title = Label::builder()
        .label("Cannot reach the Nam-tor daemon")
        .halign(Align::Start)
        .css_classes(["title"])
        .build();
    content.append(&title);
    let detail = Label::builder()
        .label(format!("{err}"))
        .halign(Align::Start)
        .wrap(true)
        .build();
    content.append(&detail);
    let steps = Label::builder()
        .label(
            "Recovery:\n\
             • Start the daemon: sudo systemctl start namtor-daemon\n\
             • Verify the D-Bus service file is installed\n\
             • Check journalctl -u namtor-daemon",
        )
        .halign(Align::Start)
        .wrap(true)
        .build();
    content.append(&steps);
    let toolbar = adw::ToolbarView::builder().build();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    window.set_content(Some(&toolbar));
}
