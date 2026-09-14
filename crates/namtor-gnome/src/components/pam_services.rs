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
pub fn render_pam_services<C: crate::client::ManagerClient + Send + Sync + 'static>(
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

fn create_pam_service_row<C: crate::client::ManagerClient + Send + Sync + 'static>(
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

fn handle_toggle<C: crate::client::ManagerClient + Send + Sync + 'static>(
    rc: &PresenterRc<C>,
    switch: &adw::SwitchRow,
    service_id: &str,
    service_name: &str,
) {
    let new_state = switch.is_active();

    // USA-0/USA-16: Route the PAM toggle through dispatch_async so the
    // GTK main thread never blocks on the D-Bus round-trip (which may
    // trigger a polkit CheckAuthorization dialog). The staging logic
    // (checking for confirmation, error handling) runs synchronously
    // for immediate UI feedback, but the actual D-Bus call is async.
    let service_id = service_id.to_string();
    let service_name = service_name.to_string();

    // Stage the toggle synchronously (for confirmation dialog).
    {
        let mut p = rc.borrow_mut();
        p.request_pam_toggle(&service_id, new_state);
    }

    // Check for errors or confirmation needs.
    let needs_confirmation = rc.borrow().model().pending_pam_toggle.is_some();
    let has_error = rc.borrow().model().error.is_some();

    if has_error {
        switch.set_active(!new_state);
        return;
    }

    if needs_confirmation {
        // Confirmation dialog will call confirm_pam_toggle asynchronously.
        show_high_impact_dialog(rc, switch, &service_id, &service_name);
        return;
    }

    // For non-risky toggles (and disables), dispatch the D-Bus call asynchronously.
    let rc_dispatch = Rc::clone(rc);
    let mut p = rc.borrow_mut();
    p.dispatch_async(move |client| {
        let _ = client.set_pam_service_enabled(&service_id, new_state);
    });
    drop(p);
    // After dispatching, schedule a refresh on the main thread.
    glib::MainContext::default().spawn_local(async move {
        // Give the D-Bus call time to complete.
        glib::timeout_future(std::time::Duration::from_millis(50)).await;
        let mut p = rc_dispatch.borrow_mut();
        p.refresh();
        p.mark_async_complete();
    });
}

fn show_high_impact_dialog<C: crate::client::ManagerClient + Send + Sync + 'static>(
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
        if response == "confirm" {
            // Dispatch the confirmation asynchronously (USA-0).
            // Get the staged toggle info before dispatching.
            let (staged_id, staged_enabled) = {
                let p = rc_dialog.borrow();
                p.model()
                    .pending_pam_toggle
                    .as_ref()
                    .map(|t| (t.id.clone(), t.enabled))
                    .unwrap_or_else(|| (String::new(), false))
            };
            if !staged_id.is_empty() {
                let rc_confirm = Rc::clone(&rc_dialog);
                let mut p = rc_dialog.borrow_mut();
                p.dispatch_async(move |client| {
                    let _ = client.set_pam_service_enabled(&staged_id, staged_enabled);
                });
                drop(p);
                // Schedule refresh on the main thread after the D-Bus call completes.
                glib::MainContext::default().spawn_local(async move {
                    glib::timeout_future(std::time::Duration::from_millis(50)).await;
                    let mut p = rc_confirm.borrow_mut();
                    p.refresh();
                    p.mark_async_complete();
                });
            }
            switch_dialog.set_active(true);
        } else {
            let mut p = rc_dialog.borrow_mut();
            p.cancel_pam_toggle();
            drop(p);
            switch_dialog.set_active(false);
        }
        dlg.close();
    });

    dialog.present();
}
