// SPDX-License-Identifier: GPL-3.0-only
//! System status component for the Nam-tor management UI.
//!
//! Renders daemon, camera, model, and protection status as two
//! [`adw::PreferencesGroup`]s: a "System status" group with one
//! [`adw::ActionRow`] per subsystem, and a "Troubleshooting" group with
//! restart-daemon and view-logs action rows.

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::{ActionRowExt, PreferencesGroupExt};

use crate::tr;

/// Render the system status as two `PreferencesGroup`s and return them.
/// The caller adds each group to the System `PreferencesPage`.
pub fn render_system_status<C: crate::client::ManagerClient + 'static>(
    presenter_rc: &std::rc::Rc<std::cell::RefCell<crate::presenter::ProfilePresenter<C>>>,
) -> Vec<adw::PreferencesGroup> {
    let model = presenter_rc.borrow().model().clone();
    let Some(system) = &model.system else {
        let group = adw::PreferencesGroup::builder()
            .title(tr("System status"))
            .build();
        let l = adw::ActionRow::builder()
            .title(tr("System status unavailable."))
            .build();
        l.update_property(&[gtk4::accessible::Property::Label(&tr(
            "System status unavailable",
        ))]);
        group.add(&l);
        return vec![group];
    };

    let mut groups = Vec::new();

    let status_group = adw::PreferencesGroup::builder()
        .title(tr("System status"))
        .build();
    status_group.update_property(&[gtk4::accessible::Property::Label(&tr(
        "System status group",
    ))]);

    let daemon_row = adw::ActionRow::builder()
        .title(tr("Nam-tor Daemon"))
        .subtitle(tr(if system.daemon_running {
            "Running"
        } else {
            "Stopped"
        }))
        .build();
    daemon_row.update_property(&[gtk4::accessible::Property::Description(&tr(
        "Whether the Nam-tor daemon process is running",
    ))]);
    status_group.add(&daemon_row);

    let camera_row = adw::ActionRow::builder()
        .title(tr("IR Camera"))
        .subtitle(tr(if system.camera_detected {
            "Detected"
        } else {
            "Not detected"
        }))
        .build();
    camera_row.update_property(&[gtk4::accessible::Property::Description(&tr(
        "Whether an infrared camera was detected",
    ))]);
    status_group.add(&camera_row);

    let models_row = adw::ActionRow::builder()
        .title(tr("Inference Models"))
        .subtitle(tr(if system.models_loaded {
            "Loaded"
        } else {
            "Missing"
        }))
        .build();
    models_row.update_property(&[gtk4::accessible::Property::Description(&tr(
        "Whether the inference models are loaded",
    ))]);
    status_group.add(&models_row);

    let prot_label = if system.protection.is_software_fallback {
        tr("Software (fallback)")
    } else {
        tr("TPM2 Hardware-bound")
    };
    let prot_row = adw::ActionRow::builder()
        .title(tr("Storage Protection"))
        .subtitle(&prot_label)
        .build();
    prot_row.update_property(&[gtk4::accessible::Property::Description(&tr(
        "How the enrolled face profiles are protected at rest",
    ))]);
    if system.protection.is_software_fallback {
        prot_row.add_css_class("warning");
    }
    status_group.add(&prot_row);

    if !system.protection.warning.is_empty() {
        let warn_row = adw::ActionRow::builder()
            .title(tr(&system.protection.warning))
            .css_classes(["warning"])
            .build();
        warn_row.set_accessible_role(gtk4::AccessibleRole::Alert);
        warn_row.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Storage protection warning",
        ))]);
        status_group.add(&warn_row);
    }

    groups.push(status_group);

    let trouble_group = adw::PreferencesGroup::builder()
        .title(tr("Troubleshooting"))
        .build();
    trouble_group.update_property(&[gtk4::accessible::Property::Label(&tr(
        "Troubleshooting group",
    ))]);

    trouble_group.add(&command_row(
        "Restart Daemon",
        "sudo systemctl restart namtor-daemon",
        "Copy the command that restarts the Nam-tor daemon",
    ));
    trouble_group.add(&command_row(
        "View System Logs",
        "journalctl -u namtor-daemon -e",
        "Copy the command that shows the Nam-tor daemon's log",
    ));

    groups.push(trouble_group);

    groups
}

/// A troubleshooting row that shows a command and copies it on click.
///
/// These two rows were `activatable(true)` with **empty** `connect_activated`
/// bodies — they highlighted under the pointer, responded to Enter, and did
/// nothing at all, under comments describing work that was never done. (The
/// test named `usa_8_restart_and_logs_rows_wired` only asserted the titles
/// appeared, never that a handler existed.)
///
/// Restarting the daemon needs a privileged action the D-Bus interface does
/// not expose, and opening a journal viewer means picking a terminal
/// emulator, which is exactly the per-desktop assumption this UI exists to
/// avoid. Handing over the exact command works identically on GNOME, KDE,
/// Xfce and sway, needs no helper, and is honest about what the app can do.
fn command_row(title: &str, command: &str, accessible: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(tr(title))
        .subtitle(command)
        .subtitle_lines(0)
        .build();
    row.add_css_class("property");

    let button = gtk4::Button::builder()
        .icon_name("edit-copy-symbolic")
        .valign(gtk4::Align::Center)
        .tooltip_text(tr("Copy command"))
        .css_classes(["flat"])
        .build();
    button.update_property(&[gtk4::accessible::Property::Label(&tr(accessible))]);

    let to_copy = command.to_string();
    button.connect_clicked(move |b| {
        b.clipboard().set_text(&to_copy);
        // Confirm in place: a copy with no feedback reads as another dead
        // button, which is what these rows already were.
        b.set_icon_name("object-select-symbolic");
        let b2 = b.clone();
        glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
            b2.set_icon_name("edit-copy-symbolic");
        });
    });

    row.add_suffix(&button);
    row.set_activatable_widget(Some(&button));
    row.update_property(&[
        gtk4::accessible::Property::Label(&tr(accessible)),
        gtk4::accessible::Property::Description(command),
    ]);
    row
}
