// SPDX-License-Identifier: GPL-3.0-only
//! Profile list view component.
//!
//! Renders enrolled profiles as an [`adw::PreferencesGroup`] containing one
//! [`adw::ActionRow`] per profile, with an [`adw::SwitchRow`] suffix to
//! toggle the profile on or off. The group title is "Enrolled Profiles".

use crate::tr;
use crate::view_model::PresenterModel;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::{ActionRowExt, PreferencesGroupExt};

pub type ToggleCallback = std::rc::Rc<dyn Fn(&str, bool)>;

pub struct ProfileListView {
    group: adw::PreferencesGroup,
}

impl ProfileListView {
    pub fn new() -> Self {
        let group = adw::PreferencesGroup::builder()
            .title(tr("Enrolled Profiles"))
            .build();
        Self { group }
    }

    pub fn render(&self, model: &PresenterModel) -> &adw::PreferencesGroup {
        self.render_with_callback(model, None)
    }

    /// Render with an optional toggle callback invoked as `on_toggle(id, enabled)`.
    /// When `on_toggle` is `None` the switches are rendered but not wired (used
    /// by callers that refresh separately after mutations).
    pub fn render_with_callback(
        &self,
        model: &PresenterModel,
        on_toggle: Option<ToggleCallback>,
    ) -> &adw::PreferencesGroup {
        self.clear();

        if model.emitter_armed_warning {
            let warn = adw::ActionRow::builder()
                .title(tr("Reduced protection: Always-on emitter detected"))
                .css_classes(["warning"])
                .build();
            warn.set_accessible_role(gtk4::AccessibleRole::Alert);
            warn.update_property(&[gtk4::accessible::Property::Label(&tr(
                "Reduced emitter protection warning",
            ))]);
            self.group.add(&warn);
        }

        if model.profiles.is_empty() {
            let l = adw::ActionRow::builder()
                .title(tr("No profiles enrolled yet."))
                .build();
            l.update_property(&[gtk4::accessible::Property::Label(&tr("Empty profile list"))]);
            self.group.add(&l);
        } else {
            for row in &model.profiles {
                let action_row = adw::ActionRow::builder()
                    .title(tr(&row.name))
                    .subtitle(tr(&format!(
                        "last used: {}",
                        if row.last_used == 0 {
                            tr("never")
                        } else {
                            format!("epoch {}", row.last_used)
                        }
                    )))
                    .build();
                action_row.set_accessible_role(gtk4::AccessibleRole::ListItem);
                action_row.update_property(&[gtk4::accessible::Property::Label(&tr(&format!(
                    "Enrolled profile {}",
                    row.name
                )))]);

                let toggle = adw::SwitchRow::builder()
                    .active(row.enabled)
                    .tooltip_text(tr(&format!("Toggle profile {} on or off", row.name)))
                    .build();
                toggle.update_property(&[
                    gtk4::accessible::Property::Label(&tr(&format!("Enable profile {}", row.name))),
                    gtk4::accessible::Property::Description(&tr("Toggle this profile on or off")),
                ]);

                if let Some(cb) = &on_toggle {
                    let id = row.id.clone();
                    let cb = std::rc::Rc::clone(cb);
                    toggle.connect_active_notify(move |sw| {
                        cb(&id, sw.is_active());
                    });
                }

                action_row.add_suffix(&toggle);
                action_row.set_activatable_widget(Some(&toggle));
                self.group.add(&action_row);
            }
        }
        &self.group
    }

    fn clear(&self) {
        while let Some(child) = self.group.first_child() {
            self.group.remove(&child);
        }
    }

    /// Get a reference to the underlying `PreferencesGroup`.
    pub fn group(&self) -> &adw::PreferencesGroup {
        &self.group
    }
}

impl Default for ProfileListView {
    fn default() -> Self {
        Self::new()
    }
}
