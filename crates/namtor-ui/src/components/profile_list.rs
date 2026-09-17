// SPDX-License-Identifier: GPL-3.0-only
//! Profile list view component.
//!
//! Renders enrolled profiles as an [`adw::PreferencesGroup`] containing one
//! [`adw::ActionRow`] per profile, with an [`adw::SwitchRow`] suffix to
//! toggle the profile on or off, and a rename button suffix (USA-10).

use std::cell::RefCell;

use crate::tr;
use crate::view_model::PresenterModel;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::{ActionRowExt, PreferencesGroupExt};

pub type ToggleCallback = std::rc::Rc<dyn Fn(&str, bool)>;
pub type RenameCallback = std::rc::Rc<dyn Fn(&str, &str)>;

pub struct ProfileListView {
    group: adw::PreferencesGroup,
    added_rows: RefCell<Vec<gtk4::Widget>>,
}

impl ProfileListView {
    pub fn new() -> Self {
        let group = adw::PreferencesGroup::builder()
            .title(tr("Enrolled Profiles"))
            .build();
        Self {
            group,
            added_rows: RefCell::new(Vec::new()),
        }
    }

    pub fn render(&self, model: &PresenterModel) -> &adw::PreferencesGroup {
        self.render_with_callback(model, None, None)
    }

    /// Render with optional toggle and rename callbacks.
    /// `on_toggle(id, enabled)` fires when the SwitchRow changes.
    /// `on_rename(id, current_name)` fires when the rename button is clicked (USA-10).
    pub fn render_with_callback(
        &self,
        model: &PresenterModel,
        on_toggle: Option<ToggleCallback>,
        on_rename: Option<RenameCallback>,
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
            self.added_rows.borrow_mut().push(warn.clone().upcast());
        }

        if model.profiles.is_empty() {
            let l = adw::ActionRow::builder()
                .title(tr("No profiles enrolled yet."))
                .build();
            l.update_property(&[gtk4::accessible::Property::Label(&tr("Empty profile list"))]);
            self.group.add(&l);
            self.added_rows.borrow_mut().push(l.clone().upcast());
        } else {
            for row in &model.profiles {
                let action_row = adw::ActionRow::builder()
                    .title(tr(&row.name))
                    .subtitle(format!(
                        "{} {}",
                        tr("last used:"),
                        crate::view_model::last_used_label_now(row.last_used)
                    ))
                    .build();
                action_row.set_accessible_role(gtk4::AccessibleRole::ListItem);
                action_row.update_property(&[gtk4::accessible::Property::Label(&tr(&format!(
                    "Enrolled profile {}",
                    row.name
                )))]);

                let toggle = adw::SwitchRow::builder()
                    .active(row.enabled)
                    .tooltip_text(crate::tr_args("Toggle profile {} on or off", &[&row.name]))
                    .build();
                toggle.update_property(&[
                    gtk4::accessible::Property::Label(&crate::tr_args(
                        "Enable profile {}",
                        &[&row.name],
                    )),
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

                // USA-10: rename button on each profile row.
                let rename_btn = gtk4::Button::builder()
                    .icon_name("document-edit-symbolic")
                    .tooltip_text(crate::tr_args("Rename profile {}", &[&row.name]))
                    .valign(gtk4::Align::Center)
                    .build();
                rename_btn.update_property(&[
                    gtk4::accessible::Property::Label(&crate::tr_args(
                        "Rename profile {}",
                        &[&row.name],
                    )),
                    gtk4::accessible::Property::Description(&tr("Rename this profile")),
                ]);

                if let Some(cb) = &on_rename {
                    let id = row.id.clone();
                    let name = row.name.clone();
                    let cb = std::rc::Rc::clone(cb);
                    rename_btn.connect_clicked(move |_| {
                        cb(&id, &name);
                    });
                }

                action_row.add_suffix(&rename_btn);
                action_row.set_activatable_widget(Some(&toggle));
                self.group.add(&action_row);
                self.added_rows
                    .borrow_mut()
                    .push(action_row.clone().upcast());
            }
        }
        &self.group
    }

    fn clear(&self) {
        let rows = std::mem::take(&mut *self.added_rows.borrow_mut());
        for row in rows {
            self.group.remove(&row);
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
