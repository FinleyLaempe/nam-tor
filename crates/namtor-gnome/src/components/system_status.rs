// SPDX-License-Identifier: GPL-3.0-only
//! System status component for the Nam-tor GNOME management UI.
//!
//! Renders daemon, camera, model, and protection status using `AdwPreferencesGroup`.

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Label};

use libadwaita as adw;
use libadwaita::prelude::{ActionRowExt, PreferencesGroupExt};

use crate::tr;

/// Render the system status view into `container`.
pub fn render_system_status<C: crate::client::ManagerClient + 'static>(
    container: &GtkBox,
    presenter_rc: &std::rc::Rc<std::cell::RefCell<crate::presenter::ProfilePresenter<C>>>,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }

    let model = presenter_rc.borrow().model().clone();
    let Some(system) = &model.system else {
        let l = Label::builder()
            .label(tr("System status unavailable."))
            .halign(gtk4::Align::Start)
            .css_classes(["dim-label"])
            .build();
        l.update_property(&[gtk4::accessible::Property::Label(&tr(
            "System status unavailable",
        ))]);
        container.append(&l);
        return;
    };

    let group = adw::PreferencesGroup::builder()
        .title(tr("System status"))
        .build();
    container.append(&group);

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
    group.add(&daemon_row);

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
    group.add(&camera_row);

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
    group.add(&models_row);

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
    group.add(&prot_row);

    if !system.protection.warning.is_empty() {
        let warn_label = Label::builder()
            .label(tr(&system.protection.warning))
            .halign(gtk4::Align::Start)
            .css_classes(["caption", "warning"])
            .build();
        warn_label.set_accessible_role(gtk4::AccessibleRole::Alert);
        warn_label.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Storage protection warning",
        ))]);
        container.append(&warn_label);
    }

    let trouble_group = adw::PreferencesGroup::builder()
        .title(tr("Troubleshooting"))
        .build();
    container.append(&trouble_group);

    let restart_row = adw::ActionRow::builder()
        .title(tr("Restart Daemon"))
        .build();
    restart_row.set_activatable(true);
    restart_row.update_property(&[
        gtk4::accessible::Property::Label(&tr("Restart the Nam-tor daemon")),
        gtk4::accessible::Property::Description(&tr("Restart the system daemon service")),
    ]);
    restart_row.connect_activated(move |_| {
        // Wired to a presenter/systemd call in the full shell.
    });
    trouble_group.add(&restart_row);

    let logs_row = adw::ActionRow::builder()
        .title(tr("View System Logs"))
        .build();
    logs_row.set_activatable(true);
    logs_row.update_property(&[
        gtk4::accessible::Property::Label(&tr("View Nam-tor system logs")),
        gtk4::accessible::Property::Description(&tr(
            "Open the system journal for the Nam-tor daemon",
        )),
    ]);
    logs_row.connect_activated(move |_| {
        // Wired to journalctl in the full shell.
    });
    trouble_group.add(&logs_row);
}
