// SPDX-License-Identifier: GPL-3.0-only
//! System status component for the Nam-tor GNOME management UI.
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

    let restart_row = adw::ActionRow::builder()
        .title(tr("Restart Daemon"))
        .subtitle(tr("Restart the system daemon service"))
        .activatable(true)
        .build();
    restart_row.update_property(&[
        gtk4::accessible::Property::Label(&tr("Restart the Nam-tor daemon")),
        gtk4::accessible::Property::Description(&tr("Restart the system daemon service")),
    ]);
    restart_row.connect_activated(move |_| {
        // USA-8: wired to systemctl restart namtor-daemon via polkit in the
        // full shell (task 41). For now the row is present and activatable.
    });
    trouble_group.add(&restart_row);

    let logs_row = adw::ActionRow::builder()
        .title(tr("View System Logs"))
        .subtitle(tr("Open the system journal for the Nam-tor daemon"))
        .activatable(true)
        .build();
    logs_row.update_property(&[
        gtk4::accessible::Property::Label(&tr("View Nam-tor system logs")),
        gtk4::accessible::Property::Description(&tr(
            "Open the system journal for the Nam-tor daemon",
        )),
    ]);
    logs_row.connect_activated(move |_| {
        // USA-8: wired to journalctl in the full shell (task 41).
    });
    trouble_group.add(&logs_row);

    groups.push(trouble_group);

    groups
}
