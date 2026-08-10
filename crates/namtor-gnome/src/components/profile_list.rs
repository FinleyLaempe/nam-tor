// SPDX-License-Identifier: GPL-3.0-only
//! Profile list view component.

use crate::tr;
use crate::view_model::PresenterModel;
use gtk4::prelude::*;
use gtk4::{AccessibleRole, Box as GtkBox, CheckButton, Label, Orientation};

pub struct ProfileListView {
    container: GtkBox,
}

impl ProfileListView {
    pub fn new() -> Self {
        let container = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();
        Self { container }
    }

    pub fn render(&self, model: &PresenterModel) -> &GtkBox {
        while let Some(child) = self.container.first_child() {
            self.container.remove(&child);
        }

        if model.emitter_armed_warning {
            let warn = Label::builder()
                .label(tr("Reduced protection: Always-on emitter detected"))
                .css_classes(["warning-banner"])
                .build();
            warn.set_accessible_role(AccessibleRole::Alert);
            warn.update_property(&[gtk4::accessible::Property::Label(&tr(
                "Reduced emitter protection warning",
            ))]);
            self.container.append(&warn);
        }

        if model.profiles.is_empty() {
            let l = Label::builder()
                .label(tr("No profiles enrolled yet."))
                .halign(gtk4::Align::Start)
                .build();
            l.update_property(&[gtk4::accessible::Property::Label(&tr("Empty profile list"))]);
            self.container.append(&l);
        } else {
            for row in &model.profiles {
                let row_box = GtkBox::builder()
                    .orientation(Orientation::Horizontal)
                    .spacing(8)
                    .build();
                row_box.set_accessible_role(AccessibleRole::ListItem);
                row_box.update_property(&[gtk4::accessible::Property::Label(&tr(
                    "Enrolled profile row",
                ))]);

                let name = Label::builder()
                    .label(tr(&row.name))
                    .halign(gtk4::Align::Start)
                    .hexpand(true)
                    .build();
                name.set_accessible_role(AccessibleRole::ListItem);
                name.update_property(&[gtk4::accessible::Property::Label(&tr(&row.name))]);

                let toggle = CheckButton::builder()
                    .label(tr("Enabled"))
                    .active(row.enabled)
                    .build();
                toggle.set_accessible_role(AccessibleRole::Checkbox);
                toggle.update_property(&[gtk4::accessible::Property::Label(&tr(&format!(
                    "Enable profile {}",
                    row.name
                )))]);
                toggle
                    .set_tooltip_text(Some(&tr(&format!("Toggle profile {} on or off", row.name))));

                let last = Label::builder()
                    .label(tr(&format!(
                        "last used: {}",
                        if row.last_used == 0 {
                            tr("never")
                        } else {
                            format!("epoch {}", row.last_used)
                        }
                    )))
                    .css_classes(["dim-label", "caption"])
                    .build();
                last.update_property(&[gtk4::accessible::Property::Description(&tr(
                    "Timestamp of last successful authentication",
                ))]);

                row_box.append(&name);
                row_box.append(&toggle);
                row_box.append(&last);
                self.container.append(&row_box);
            }
        }
        &self.container
    }
}

impl Default for ProfileListView {
    fn default() -> Self {
        Self::new()
    }
}
