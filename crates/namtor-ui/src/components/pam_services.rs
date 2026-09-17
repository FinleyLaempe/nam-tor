// SPDX-License-Identifier: GPL-3.0-only
//! PAM services inventory component for the Nam-tor management UI.
//!
//! Renders a list of PAM services as an [`adw::PreferencesGroup`] containing
//! one [`adw::ActionRow`] per service with an [`adw::SwitchRow`] suffix
//! toggle, descriptions as subtitles, and compatibility warnings. High-impact
//! (risky) services require an explicit confirmation dialog before enabling.

use std::cell::{Cell, RefCell};
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

thread_local! {
    /// True while we are setting a switch ourselves.
    ///
    /// `connect_active_notify` fires for programmatic changes too, so every
    /// `set_active` used to re-enter `handle_toggle` and start a second
    /// toggle for the state we were only trying to display. The UI is
    /// single-threaded, so a thread-local flag is the whole mechanism.
    static APPLYING: Cell<bool> = const { Cell::new(false) };
}

/// Set a switch without the change being read back as a user action.
fn set_switch_silently(switch: &adw::SwitchRow, active: bool) {
    if switch.is_active() == active {
        return;
    }
    APPLYING.with(|f| f.set(true));
    switch.set_active(active);
    APPLYING.with(|f| f.set(false));
}

/// Show the outcome of a toggle and put the switch back on the truth.
///
/// The authoritative state is whatever the daemon reports after the call, not
/// what the user clicked, so a refusal is visible rather than being silently
/// undone half a second later.
fn report_toggle_outcome<C: crate::client::ManagerClient + Send + Sync + 'static>(
    rc: &PresenterRc<C>,
    switch: &adw::SwitchRow,
    service_id: &str,
) {
    let (error, authoritative) = {
        let p = rc.borrow();
        let m = p.model();
        let actual = m
            .pam_services
            .iter()
            .find(|s| s.id == service_id)
            .map(|s| s.enabled);
        (m.error.clone(), actual)
    };

    if let Some(actual) = authoritative {
        set_switch_silently(switch, actual);
    }

    let Some(err) = error else { return };
    show_error_dialog(switch, &err);
    rc.borrow_mut().dismiss_error();
}

/// A privileged action that was refused deserves a modal, not a silent
/// revert. This is the only surface the services list owns.
fn show_error_dialog(switch: &adw::SwitchRow, err: &crate::view_model::ErrorView) {
    let mut body = err.detail.clone();
    if let Some(hint) = &err.recovery {
        body.push_str("\n\n");
        body.push_str(&tr(&hint.title));
        for (i, step) in hint.steps.iter().enumerate() {
            body.push_str(&format!("\n{}. {}", i + 1, step));
        }
    }
    let dialog = adw::MessageDialog::builder()
        .heading(tr(&err.title))
        .body(&body)
        .build();
    if let Some(win) = switch.root().and_downcast_ref::<adw::ApplicationWindow>() {
        dialog.set_transient_for(Some(win));
    }
    dialog.add_response("ok", &tr("_Close"));
    dialog.set_default_response(Some("ok"));
    dialog.present();
}

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
    // Ignore the notify we caused ourselves while displaying a state.
    if APPLYING.with(|f| f.get()) {
        return;
    }
    let new_state = switch.is_active();

    // The toggle is applied synchronously by `request_pam_toggle` below, so
    // the GTK main loop is blocked for the length of the D-Bus round trip,
    // including any polkit prompt. That was true before this code consumed
    // the outcome and is not made worse by it — the previous comment here
    // claimed `dispatch_async` kept the main thread free, which was never the
    // case for this path because the staging call applies the change itself.
    // Moving it off-thread is worth doing and is tracked separately; it needs
    // the same begin/finish split the profile save uses.
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
        set_switch_silently(switch, !new_state);
        report_toggle_outcome(rc, switch, &service_id);
        return;
    }

    if needs_confirmation {
        // Confirmation dialog will call confirm_pam_toggle asynchronously.
        show_high_impact_dialog(rc, switch, &service_id, &service_name);
        return;
    }

    // `request_pam_toggle` above ALREADY applied this toggle: it documents
    // "Disabling any service applies immediately. Enabling a `Safe` service
    // applies immediately", and only a high-impact enable is staged. So there
    // is nothing left to call here — reaching for the client again fired the
    // D-Bus call a second time, which means a second polkit prompt for the
    // same switch, and let the second call's result overwrite the first, so a
    // success followed by a denial recorded an error for a change that had in
    // fact been applied.
    //
    // What was actually missing was consuming the outcome. The presenter has
    // been mapping every failure to an ErrorView with a recovery hint all
    // along; the view threw it away with `let _ =`, slept 50 ms, and
    // refreshed — so a polkit denial showed the user nothing and the sleep
    // raced a prompt that takes seconds.
    rc.borrow_mut().mark_async_complete();
    report_toggle_outcome(rc, switch, &service_id);
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
            let (staged_id, _staged_enabled) = {
                let p = rc_dialog.borrow();
                p.model()
                    .pending_pam_toggle
                    .as_ref()
                    .map(|t| (t.id.clone(), t.enabled))
                    .unwrap_or_else(|| (String::new(), false))
            };
            if !staged_id.is_empty() {
                // `confirm_pam_toggle` is the staged-toggle's own entry
                // point; it applies the pending toggle and clears it. Calling
                // `set_pam_service_enabled` directly here bypassed that and
                // left `pending_pam_toggle` set.
                {
                    let mut p = rc_dialog.borrow_mut();
                    p.confirm_pam_toggle();
                    p.mark_async_complete();
                }
                // No set_active(true) here. report_toggle_outcome has already
                // put the switch on whatever the daemon reports; forcing it
                // true afterwards showed a denied high-impact enable as
                // enabled.
                report_toggle_outcome(&rc_dialog, &switch_dialog, &staged_id);
            }
        } else {
            let mut p = rc_dialog.borrow_mut();
            p.cancel_pam_toggle();
            drop(p);
            set_switch_silently(&switch_dialog, false);
        }
        dlg.close();
    });

    dialog.present();
}
