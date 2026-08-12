// SPDX-License-Identifier: GPL-3.0-only
//! PAM services inventory component for the Nam-tor GNOME management UI.
//!
//! Renders a list of PAM services as an [`adw::PreferencesGroup`] containing
//! one [`adw::ActionRow`] per service with an [`adw::SwitchRow`] suffix
//! toggle, descriptions as subtitles, and compatibility warnings. High-impact
//! (risky) services require an explicit confirmation dialog before enabling.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::{ActionRowExt, MessageDialogExt, PreferencesGroupExt};

use crate::client::PamServiceCompatibility;
use crate::presenter::ProfilePresenter;
use crate::tr;
use crate::view_model::compatibility_label;

/// The exact warning text that MUST appear in the high-impact confirmation
/// dialog. The user must acknowledge this before a risky service can be
/// enabled.
pub const HIGH_IMPACT_WARNING: &str =
    "FAR/FRR and real-attack PAD gates remain unmeasured. Enabling face \
     authentication is experimental and not recommended for \
     security-sensitive use.";

type PresenterRc<C> = Rc<RefCell<ProfilePresenter<C>>>;

/// Render the PAM services inventory as a `PreferencesGroup` and return it.
/// The caller adds the group to the Authentication `PreferencesPage`.
pub fn render_pam_services<C: crate::client::ManagerClient + 'static>(
    presenter_rc: &PresenterRc<C>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(tr("Authentication uses"))
        .build();
    group.update_property(&[gtk4::accessible::Property::Label(&tr(
        "PAM services inventory",
    ))]);

    let model = presenter_rc.borrow().model().clone();

    if model.pam_services.is_empty() {
        let l = adw::ActionRow::builder()
            .title(tr("No PAM services found."))
            .build();
        l.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Empty PAM service list",
        ))]);
        group.add(&l);
        return group;
    }

    for svc in &model.pam_services {
        group.add(&create_pam_service_row(svc, presenter_rc));
    }
    group
}

fn create_pam_service_row<C: crate::client::ManagerClient + 'static>(
    service: &crate::view_model::PamServiceRowView,
    presenter_rc: &PresenterRc<C>,
) -> adw::ActionRow {
    let compat_text = compatibility_label(service.compatibility);
    let subtitle = format!(
        "{}\n{}: {}",
        service.description,
        tr("Compatibility"),
        compat_text
    );

    let row = adw::ActionRow::builder()
        .title(tr(&service.display_name))
        .subtitle(tr(&subtitle))
        .build();
    row.update_property(&[
        gtk4::accessible::Property::Label(&tr(&service.display_name)),
        gtk4::accessible::Property::Description(&tr(&service.description)),
    ]);

    if service.compatibility == PamServiceCompatibility::Risky {
        row.add_css_class("warning");
    } else if service.compatibility == PamServiceCompatibility::Unsupported {
        row.add_css_class("error");
    }

    let toggle = adw::SwitchRow::builder()
        .active(service.enabled)
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
        let service_id = service.id.clone();
        let service_name = service.display_name.clone();
        toggle.connect_active_notify(move |sw| {
            handle_toggle::<C>(&rc, sw, &service_id, &service_name);
        });
    }

    row.add_suffix(&toggle);
    row.set_activatable_widget(Some(&toggle));
    row
}

fn handle_toggle<C: crate::client::ManagerClient + 'static>(
    rc: &PresenterRc<C>,
    switch: &adw::SwitchRow,
    service_id: &str,
    service_name: &str,
) {
    let new_state = switch.is_active();
    // USA-0/USA-16: in a full deployment this synchronous D-Bus call
    // (which may trigger a polkit CheckAuthorization) should be routed
    // through `presenter.dispatch_async()` + `glib::MainContext` idle
    // callback so the GTK main thread never blocks on the round-trip.
    // The scroll position of the PAM services page is managed by
    // `PreferencesDialog` internally (the dialog preserves scroll across
    // re-renders of the same page).
    {
        let mut p = rc.borrow_mut();
        p.request_pam_toggle(service_id, new_state);
    }

    let needs_confirmation = rc.borrow().model().pending_pam_toggle.is_some();
    let has_error = rc.borrow().model().error.is_some();

    if has_error {
        switch.set_active(!new_state);
        return;
    }

    if needs_confirmation {
        show_high_impact_dialog(rc, switch, service_id, service_name);
    }
}

fn show_high_impact_dialog<C: crate::client::ManagerClient + 'static>(
    rc: &PresenterRc<C>,
    switch: &adw::SwitchRow,
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
        dlg.close();
    });

    dialog.present();
}
