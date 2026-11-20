// SPDX-License-Identifier: GPL-3.0-only
//! Widget hierarchy regressions for the profile and PAM toggle suffixes (#32).
//!
//! An `adw::SwitchRow` is a `GtkListBoxRow`: it must be a child of a
//! `GtkListBox` (that is what `AdwPreferencesGroup` builds). Adding it as an
//! `ActionRow` suffix parents it into the row's internal `GtkBox`, an
//! invalid hierarchy — and because `gtk_widget_activate` only works on
//! widgets that set an activate signal (`gtk4::Switch` does, `AdwSwitchRow`
//! does not), the row's `activatable-widget` never toggled the switch at
//! all: clicking the row or pressing Space on it did nothing.
//!
//! The correct libadwaita pattern is a plain `gtk4::Switch` as the suffix,
//! vertically centered, set as the row's activatable widget. These tests
//! pin that pattern and the toggle/rollback behavior around it.
//!
//! All GTK behavioral tests run headlessly through the shared GTK test
//! thread (`common::run_gtk_test`). GtkSwitch's row activation toggles
//! through a 100 ms animation tick when animations are enabled, which never
//! completes for unmapped widgets, so the tests disable animations first.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::{ActionRowExt, MessageDialogExt, PreferencesRowExt};

use namtor_ui::components::pam_services::render_pam_services;
use namtor_ui::components::ProfileListView;
use namtor_ui::view_model::{PamServiceRowView, PresenterModel, ProfileRowView};
use namtor_ui::{FakeManagerClient, FakeMode, ProfilePresenter};

const TEST_UID: u32 = 1000;

/// Turn off animations so `GtkSwitch`'s activate toggle is synchronous.
/// With animations on, row activation starts a 100 ms tick-callback
/// animation that never advances for unmapped widgets, so the active state
/// would never change under a headless display.
fn animations_off() {
    if let Some(settings) = gtk4::Settings::default() {
        settings.set_gtk_enable_animations(false);
    }
}

/// Depth-first walk handing every `gtk4::Switch` that is not nested inside
/// an `AdwSwitchRow` to `f`. The switches inside an `AdwSwitchRow` belong to
/// that row; only a bare `GtkSwitch` can be a valid ActionRow suffix.
fn for_each_suffix_switch(widget: &gtk4::Widget, f: &mut dyn FnMut(&gtk4::Switch)) {
    for_each_suffix_switch_inner(widget, false, f);
}

fn for_each_suffix_switch_inner(
    widget: &gtk4::Widget,
    inside_switch_row: bool,
    f: &mut dyn FnMut(&gtk4::Switch),
) {
    let is_switch_row = widget.downcast_ref::<adw::SwitchRow>().is_some();
    if !inside_switch_row {
        if let Some(sw) = widget.downcast_ref::<gtk4::Switch>() {
            f(sw);
        }
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        for_each_suffix_switch_inner(&c, inside_switch_row || is_switch_row, f);
        child = c.next_sibling();
    }
}

fn collect_suffix_switches(widget: &gtk4::Widget) -> Vec<gtk4::Switch> {
    let mut out = Vec::new();
    for_each_suffix_switch(widget, &mut |sw| out.push(sw.clone()));
    out
}

fn count_switch_rows(widget: &gtk4::Widget) -> usize {
    let mut n = 0;
    fn walk(widget: &gtk4::Widget, n: &mut usize) {
        if widget.downcast_ref::<adw::SwitchRow>().is_some() {
            *n += 1;
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            walk(&c, n);
            child = c.next_sibling();
        }
    }
    walk(widget, &mut n);
    n
}

/// The first `AdwActionRow` ancestor of `widget`, if any.
fn ancestor_action_row(widget: &gtk4::Widget) -> Option<adw::ActionRow> {
    let mut cur = widget.parent();
    while let Some(w) = cur {
        if let Some(row) = w.downcast_ref::<adw::ActionRow>() {
            return Some(row.clone());
        }
        cur = w.parent();
    }
    None
}

/// The toggle suffix of the row titled `row_title`.
///
/// Selecting by row title works for every service, including `Unsupported`
/// ones — their switches get a different tooltip ("… is incompatible …"),
/// so a tooltip-based lookup would silently miss them.
fn suffix_switch_in_row(root: &gtk4::Widget, row_title: &str) -> Option<gtk4::Switch> {
    fn find_row(widget: &gtk4::Widget, row_title: &str) -> Option<adw::ActionRow> {
        if let Some(row) = widget.downcast_ref::<adw::ActionRow>() {
            if row.title().as_str() == row_title {
                return Some(row.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            if let Some(row) = find_row(&c, row_title) {
                return Some(row);
            }
            child = c.next_sibling();
        }
        None
    }

    let row = find_row(root, row_title)?;
    collect_suffix_switches(row.upcast_ref()).into_iter().next()
}

/// Coerce a concrete widget reference to `&gtk4::Widget` for the helpers.
fn as_widget<T: glib::object::IsA<gtk4::Widget>>(w: &T) -> &gtk4::Widget {
    w.upcast_ref()
}

fn profile_model() -> PresenterModel {
    let mut model = PresenterModel::default();
    model.profiles.push(ProfileRowView {
        id: "profile-on".into(),
        name: "Enabled User".into(),
        enabled: true,
        created: 0,
        last_used: 0,
    });
    model.profiles.push(ProfileRowView {
        id: "profile-off".into(),
        name: "Disabled User".into(),
        enabled: false,
        created: 0,
        last_used: 0,
    });
    model
}

/// A presenter wired to `fake`, refreshed, plus a disabled service of the
/// given compatibility to act on. Naming the service matters: flipping a
/// switch that belongs to a different service would observe nothing.
fn pam_fixture(
    fake: &std::sync::Arc<FakeManagerClient>,
    want: namtor_ui::PamServiceCompatibility,
) -> (
    Rc<RefCell<ProfilePresenter<FakeManagerClient>>>,
    PamServiceRowView,
) {
    let mut presenter = ProfilePresenter::from_arc(std::sync::Arc::clone(fake), TEST_UID);
    presenter.refresh();
    let target = presenter
        .model()
        .pam_services
        .iter()
        .find(|s| s.compatibility == want && !s.enabled)
        .unwrap_or_else(|| panic!("the fake must serve a disabled {want:?} PAM service"))
        .clone();
    (Rc::new(RefCell::new(presenter)), target)
}

// ===========================================================================
// Profile list: the toggle suffix must be a valid, working gtk4::Switch
// ===========================================================================

/// The profile toggle must be a bare `gtk4::Switch` suffix — vertically
/// centered in its ActionRow — never a nested `AdwSwitchRow`. An
/// `AdwSwitchRow` is a `GtkListBoxRow`; parenting it into the ActionRow's
/// suffix `GtkBox` is an invalid hierarchy (#32).
#[test]
fn profile_toggle_is_a_vertically_centered_gtk_switch_suffix() {
    common::run_gtk_test(move || {
        animations_off();
        let view = ProfileListView::new();
        let group = view.render(&profile_model());
        let root: gtk4::Widget = group.clone().upcast();

        assert_eq!(
            count_switch_rows(&root),
            0,
            "an AdwSwitchRow must never be nested in an ActionRow suffix"
        );

        let switches = collect_suffix_switches(&root);
        assert_eq!(
            switches.len(),
            2,
            "each profile row must carry exactly one gtk4::Switch suffix"
        );

        for sw in &switches {
            assert_eq!(
                sw.valign(),
                gtk4::Align::Center,
                "the toggle suffix must be vertically centered in its row"
            );
        }

        let states: Vec<bool> = switches.iter().map(|sw| sw.is_active()).collect();
        assert!(
            states.contains(&true) && states.contains(&false),
            "the switches must reflect the profiles' enabled states: {states:?}"
        );
    });
}

/// Activating a profile row must toggle the profile's switch and fire the
/// toggle callback exactly once. With the old `AdwSwitchRow` suffix the
/// row's activatable widget never activated: `gtk_widget_activate` only
/// works on widgets that set an activate signal, which `AdwSwitchRow` does
/// not — clicking the row or pressing Space on it did nothing.
#[test]
fn activating_a_profile_row_toggles_the_profile_and_fires_the_callback_once() {
    common::run_gtk_test(move || {
        animations_off();

        let toggles: Rc<RefCell<Vec<(String, bool)>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&toggles);

        let model = profile_model();
        let view = ProfileListView::new();
        let group = view.render_with_callback(
            &model,
            Some(Rc::new(move |id, on| {
                recorded.borrow_mut().push((id.to_string(), on));
            })),
            None,
        );
        let root: gtk4::Widget = group.clone().upcast();

        let switch = collect_suffix_switches(&root)
            .into_iter()
            .find(|sw| !sw.is_active())
            .expect("the disabled profile must have an off switch");
        assert!(
            !switch.is_active(),
            "the disabled profile's switch starts off"
        );

        let row = ancestor_action_row(as_widget(&switch))
            .expect("the switch must live inside an ActionRow");
        ActionRowExt::activate(&row);

        assert!(
            switch.is_active(),
            "activating the row must toggle the profile switch on"
        );
        assert_eq!(
            toggles.borrow().as_slice(),
            [("profile-off".to_string(), true)],
            "activating the row must fire the toggle callback exactly once"
        );
    });
}

/// Flipping a profile switch fires the toggle callback once with the
/// profile id and the new state — the callback contract the presenter
/// relies on. The flip must not fire twice (once for the user action, once
/// for a programmatic re-render).
#[test]
fn flipping_a_profile_switch_fires_the_callback_once_with_the_profile_id() {
    common::run_gtk_test(move || {
        animations_off();

        let toggles: Rc<RefCell<Vec<(String, bool)>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&toggles);

        let model = profile_model();
        let view = ProfileListView::new();
        let group = view.render_with_callback(
            &model,
            Some(Rc::new(move |id, on| {
                recorded.borrow_mut().push((id.to_string(), on));
            })),
            None,
        );
        let root: gtk4::Widget = group.clone().upcast();

        let switch = collect_suffix_switches(&root)
            .into_iter()
            .find(|sw| !sw.is_active())
            .expect("the disabled profile must have an off switch");

        switch.set_active(true);

        assert_eq!(
            toggles.borrow().as_slice(),
            [("profile-off".to_string(), true)],
            "one user flip must notify exactly once, with the profile id"
        );
    });
}

// ===========================================================================
// PAM services: same hierarchy rule, plus rollback and sensitivity
// ===========================================================================

/// Every PAM service toggle must be a bare, vertically centered
/// `gtk4::Switch` suffix set as its row's activatable widget — never a
/// nested `AdwSwitchRow` (#32). Unsupported services must stay insensitive.
#[test]
fn pam_toggle_is_a_centered_gtk_switch_the_row_activates() {
    common::run_gtk_test(move || {
        animations_off();

        let fake = std::sync::Arc::new(FakeManagerClient::new());
        let (rc, _) = pam_fixture(&fake, namtor_ui::PamServiceCompatibility::Safe);
        let group = render_pam_services(&rc);
        let root: gtk4::Widget = group.clone().upcast();

        assert_eq!(
            count_switch_rows(&root),
            0,
            "an AdwSwitchRow must never be nested in an ActionRow suffix"
        );

        let services = rc.borrow().model().pam_services.clone();
        assert!(
            !services.is_empty(),
            "the fake must serve PAM services for this test to mean anything"
        );

        for svc in &services {
            let switch = suffix_switch_in_row(&root, &svc.display_name)
                .unwrap_or_else(|| panic!("service {} must have a gtk4::Switch suffix", svc.id));
            let sw = &switch;

            assert_eq!(
                sw.valign(),
                gtk4::Align::Center,
                "the toggle suffix must be vertically centered in its row"
            );
            assert_eq!(
                sw.is_sensitive(),
                svc.compatibility != namtor_ui::PamServiceCompatibility::Unsupported,
                "service {} toggle sensitivity must follow its compatibility",
                svc.id
            );
            assert_eq!(
                sw.is_active(),
                svc.enabled,
                "service {} switch must reflect the served state",
                svc.id
            );

            let row = ancestor_action_row(as_widget(sw))
                .expect("the switch must live inside an ActionRow");
            let activatable = row
                .activatable_widget()
                .expect("the service row must have an activatable widget");
            assert_eq!(
                activatable.type_().name(),
                "GtkSwitch",
                "the activatable widget must be the bare gtk4::Switch, not a SwitchRow"
            );
            assert_eq!(
                &activatable, sw,
                "activating the row must act on the service's own switch"
            );
        }
    });
}

/// A refused toggle must roll the switch back to the authoritative state
/// silently — the rollback itself is a programmatic change that must not
/// re-enter the handler and issue a second privileged request. One flip,
/// one request, switch back on the truth.
#[test]
fn a_refused_pam_toggle_rolls_back_silently_and_requests_once() {
    common::run_gtk_test(move || {
        animations_off();

        let fake = std::sync::Arc::new(FakeManagerClient::new());
        fake.set_mode(FakeMode::PolkitDenied("denied by test".into()));
        let (rc, target) = pam_fixture(&fake, namtor_ui::PamServiceCompatibility::Safe);
        let group = render_pam_services(&rc);
        let root: gtk4::Widget = group.clone().upcast();

        let switch = suffix_switch_in_row(&root, &target.display_name)
            .expect("the target service must have a switch");
        assert!(!switch.is_active(), "the target service starts disabled");

        let before = fake.pam_toggle_calls();
        switch.set_active(true);
        let after = fake.pam_toggle_calls();

        assert_eq!(
            after - before,
            1,
            "one refused flip must issue exactly one request; each one is a \
             separate polkit prompt"
        );
        assert!(
            !switch.is_active(),
            "the switch must roll back to the authoritative (off) state"
        );
        // The component consumes the presenter error after surfacing it in a
        // dialog. Assert that visible outcome, not the dismissed model state.
        let dialog = gtk4::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<adw::MessageDialog>().ok())
            .find(|d| d.is_visible())
            .expect("the refusal must be surfaced in an error dialog");
        assert!(dialog.body().contains("denied by test"));
        dialog.response("ok");
    });
}

/// An unsupported service cannot be toggled: its switch is insensitive and
/// no user action may reach a privileged request through it.
#[test]
fn an_unsupported_service_is_insensitive_and_never_requests_a_toggle() {
    common::run_gtk_test(move || {
        animations_off();

        let fake = std::sync::Arc::new(FakeManagerClient::new());
        let (rc, target) = pam_fixture(&fake, namtor_ui::PamServiceCompatibility::Unsupported);
        let group = render_pam_services(&rc);
        let root: gtk4::Widget = group.clone().upcast();

        let switch = suffix_switch_in_row(&root, &target.display_name)
            .expect("the target service must have a switch");

        assert!(
            !switch.is_sensitive(),
            "an incompatible service must not be toggleable"
        );

        // Even a programmatic flip must not reach the daemon: no handler may
        // be listening on an unsupported service's switch.
        let before = fake.pam_toggle_calls();
        switch.set_active(true);
        assert_eq!(
            fake.pam_toggle_calls(),
            before,
            "an unsupported service must never issue a privileged request"
        );
    });
}
