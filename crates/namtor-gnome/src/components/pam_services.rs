// SPDX-License-Identifier: GPL-3.0-only
//! PAM services inventory component for the Nam-tor GNOME management UI.
//!
//! Renders a list of PAM services with `gtk::Switch` toggles, descriptions,
//! and compatibility warnings. High-impact (risky) services require an
//! explicit confirmation dialog before enabling.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{AccessibleRole, Align, Box as GtkBox, Label, Orientation, Switch};
use libadwaita as adw;
use libadwaita::prelude::MessageDialogExt;

use crate::client::PamServiceCompatibility;
use crate::presenter::ProfilePresenter;
use crate::tr;
use crate::view_model::{compatibility_label, PamServiceRowView};

/// The exact warning text that MUST appear in the high-impact confirmation
/// dialog. The user must acknowledge this before a risky service can be
/// enabled.
pub const HIGH_IMPACT_WARNING: &str =
    "FAR/FRR and real-attack PAD gates remain unmeasured. Enabling face \
     authentication is experimental and not recommended for \
     security-sensitive use.";

type PresenterRc<C> = Rc<RefCell<ProfilePresenter<C>>>;

/// Render the PAM services inventory into `container`.
///
/// Reads the model from the presenter, builds one row per service with a
/// `gtk::Switch`, and wires toggle callbacks that route through
/// `request_pam_toggle` → confirmation dialog → `confirm_pam_toggle` for
/// risky services, or direct apply for safe services.
pub fn render_pam_services<C: crate::client::ManagerClient + 'static>(
    container: &GtkBox,
    presenter_rc: &PresenterRc<C>,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }

    let model = presenter_rc.borrow().model().clone();

    let title = Label::builder()
        .label(tr("Authentication uses"))
        .halign(Align::Start)
        .css_classes(["heading"])
        .accessible_role(AccessibleRole::Heading)
        .build();
    title.update_property(&[gtk4::accessible::Property::Level(1)]);
    container.append(&title);

    if model.pam_services.is_empty() {
        let l = Label::builder()
            .label(tr("No PAM services found."))
            .halign(Align::Start)
            .css_classes(["dim-label"])
            .build();
        l.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Empty PAM service list",
        ))]);
        container.append(&l);
        return;
    }

    for svc in &model.pam_services {
        container.append(&create_pam_service_row(svc, presenter_rc, container));
    }
}

#[allow(clippy::too_many_arguments)]
fn create_pam_service_row<C: crate::client::ManagerClient + 'static>(
    service: &PamServiceRowView,
    presenter_rc: &PresenterRc<C>,
    container: &GtkBox,
) -> GtkBox {
    let row_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(12)
        .margin_bottom(8)
        .build();

    let info_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .halign(Align::Start)
        .build();

    let name_label = Label::builder()
        .label(tr(&service.display_name))
        .halign(Align::Start)
        .accessible_role(AccessibleRole::ListItem)
        .build();
    name_label.update_property(&[gtk4::accessible::Property::Label(
        &tr(&service.display_name),
    )]);
    info_box.append(&name_label);

    let desc_label = Label::builder()
        .label(tr(&service.description))
        .halign(Align::Start)
        .css_classes(["dim-label", "caption"])
        .build();
    desc_label.update_property(&[gtk4::accessible::Property::Description(&tr(
        &service.description
    ))]);
    info_box.append(&desc_label);

    let compat_text = compatibility_label(service.compatibility);
    let compat_label = Label::builder()
        .label(tr(&format!("Compatibility: {compat_text}")))
        .halign(Align::Start)
        .css_classes(["caption"])
        .build();
    compat_label.update_property(&[gtk4::accessible::Property::Label(&tr(&format!(
        "Service compatibility: {compat_text}"
    )))]);
    if service.compatibility == PamServiceCompatibility::Risky {
        compat_label.add_css_class("warning");
    } else if service.compatibility == PamServiceCompatibility::Unsupported {
        compat_label.add_css_class("error");
    }
    info_box.append(&compat_label);

    row_box.append(&info_box);

    let toggle = Switch::builder()
        .active(service.enabled)
        .valign(Align::Center)
        .accessible_role(AccessibleRole::Switch)
        .tooltip_text(tr(&format!(
            "Toggle {} ({})",
            service.display_name, service.id
        )))
        .build();
    toggle.update_property(&[
        gtk4::accessible::Property::Label(&tr(&format!(
            "Enable {} authentication",
            service.display_name
        ))),
        gtk4::accessible::Property::Description(&tr(&format!(
            "Toggle whether {} uses face authentication",
            service.display_name
        ))),
    ]);

    let is_unsupported = service.compatibility == PamServiceCompatibility::Unsupported;
    if is_unsupported {
        toggle.set_sensitive(false);
        toggle.set_tooltip_text(Some(&tr(&format!(
            "{} is incompatible and cannot be enabled.",
            service.display_name
        ))));
    }

    if !is_unsupported {
        let rc = Rc::clone(presenter_rc);
        let container_clone = container.clone();
        let service_id = service.id.clone();
        let service_name = service.display_name.clone();
        toggle.connect_state_set(move |switch, new_state| {
            handle_toggle::<C>(
                &rc,
                &container_clone,
                &service_id,
                &service_name,
                switch,
                new_state,
            )
        });
    }

    row_box.append(&toggle);
    row_box
}

fn handle_toggle<C: crate::client::ManagerClient + 'static>(
    rc: &PresenterRc<C>,
    container: &GtkBox,
    service_id: &str,
    service_name: &str,
    switch: &Switch,
    new_state: bool,
) -> glib::Propagation {
    {
        let mut p = rc.borrow_mut();
        p.request_pam_toggle(service_id, new_state);
    }

    let needs_confirmation = rc.borrow().model().pending_pam_toggle.is_some();
    let has_error = rc.borrow().model().error.is_some();

    if has_error {
        render_pam_services(container, rc);
        return glib::Propagation::Stop;
    }

    if needs_confirmation {
        show_high_impact_dialog(rc, container, switch, service_id, service_name);
        return glib::Propagation::Stop;
    }

    render_pam_services(container, rc);
    glib::Propagation::Proceed
}

fn show_high_impact_dialog<C: crate::client::ManagerClient + 'static>(
    rc: &PresenterRc<C>,
    container: &GtkBox,
    switch: &Switch,
    _service_id: &str,
    service_name: &str,
) {
    let root = switch.root();
    let parent = root.and_downcast_ref::<adw::ApplicationWindow>();

    let dialog = adw::MessageDialog::builder()
        .heading(format!("Enable {} — high-impact service", service_name))
        .body(HIGH_IMPACT_WARNING)
        .build();

    if let Some(win) = parent {
        dialog.set_transient_for(Some(win));
    }

    dialog.add_response("cancel", "Cancel");
    dialog.add_response("confirm", "I understand, enable anyway");
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);

    let rc_dialog = Rc::clone(rc);
    let container_dialog = container.clone();
    let switch_dialog = switch.clone();
    dialog.connect_response(None, move |dlg, response| {
        let mut p = rc_dialog.borrow_mut();
        if response == "confirm" {
            p.confirm_pam_toggle();
            switch_dialog.set_active(true);
        } else {
            p.cancel_pam_toggle();
            switch_dialog.set_active(false);
        }
        drop(p);
        render_pam_services(&container_dialog, &rc_dialog);
        dlg.close();
    });

    dialog.present();
}
