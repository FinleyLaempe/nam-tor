// SPDX-License-Identifier: GPL-3.0-only
//! Recovery view component for unreachable daemon.

use crate::tr;
use crate::view_model::RecoveryHint;
use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Label, Orientation};

pub struct RecoveryView {
    container: GtkBox,
}

impl RecoveryView {
    pub fn new() -> Self {
        let container = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .margin_start(24)
            .margin_end(24)
            .margin_top(24)
            .margin_bottom(24)
            .spacing(12)
            .build();
        Self { container }
    }

    pub fn render(&self, hint: &RecoveryHint) -> &GtkBox {
        while let Some(child) = self.container.first_child() {
            self.container.remove(&child);
        }

        let title = Label::builder()
            .label(tr(&hint.title))
            .halign(gtk4::Align::Start)
            .css_classes(["title-1"])
            .build();
        title.set_accessible_role(gtk4::AccessibleRole::Heading);
        title.update_property(&[gtk4::accessible::Property::Level(1)]);
        self.container.append(&title);

        let detail = Label::builder()
            .label(tr(&hint.title))
            .halign(gtk4::Align::Start)
            .wrap(true)
            .build();
        detail.update_property(&[gtk4::accessible::Property::Description(&tr(
            "Detailed explanation of the recovery state",
        ))]);
        self.container.append(&detail);

        let steps_text = format!("{}\n\n{}", tr("Recovery Steps:"), hint.steps.join("\n\n"));
        let steps = Label::builder()
            .label(tr(&steps_text))
            .halign(gtk4::Align::Start)
            .wrap(true)
            .css_classes(["caption"])
            .build();
        steps.set_accessible_role(gtk4::AccessibleRole::List);
        steps.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Recovery guidance steps",
        ))]);
        self.container.append(&steps);

        &self.container
    }
}

impl Default for RecoveryView {
    fn default() -> Self {
        Self::new()
    }
}
