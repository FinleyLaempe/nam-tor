// SPDX-License-Identifier: GPL-3.0-only
//! Profile list view component.

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

        if model.reduced_protection_warning {
            let warn = Label::builder()
                .label("Reduced protection: Always-on emitter detected")
                .css_classes(["warning-banner"])
                .build();
            warn.set_accessible_role(AccessibleRole::Alert);
            self.container.append(&warn);
        }

        if model.profiles.is_empty() {
            let l = Label::builder()
                .label("No profiles enrolled yet.")
                .halign(gtk4::Align::Start)
                .build();
            self.container.append(&l);
        } else {
            for row in &model.profiles {
                let row_box = GtkBox::builder()
                    .orientation(Orientation::Horizontal)
                    .spacing(8)
                    .build();
                let name = Label::builder()
                    .label(&row.name)
                    .halign(gtk4::Align::Start)
                    .hexpand(true)
                    .build();
                name.set_accessible_role(AccessibleRole::ListItem);
                let toggle = CheckButton::builder()
                    .label("Enabled")
                    .active(row.enabled)
                    .build();
                toggle.set_accessible_role(AccessibleRole::Checkbox);
                let last = Label::builder()
                    .label(format!(
                        "last used: {}",
                        row.last_used
                            .map(|t| format!("epoch {t}"))
                            .unwrap_or_else(|| "never".into())
                    ))
                    .css_classes(["dim-label", "caption"])
                    .build();
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
