// SPDX-License-Identifier: GPL-3.0-only
//! Recovery view component for unreachable daemon.

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Label, Orientation};
use crate::view_model::RecoveryHint;

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
            .label(&hint.title)
            .halign(gtk4::Align::Start)
            .css_classes(["title"])
            .build();
        self.container.append(&title);

        let detail = Label::builder()
            .label(&hint.title) // Note: detail logic remains as-is from previous draft
            .halign(gtk4::Align::Start)
            .wrap(true)
            .build();
        self.container.append(&detail);

        let steps = Label::builder()
            .label(format!("Recovery:\n{}", hint.steps.join("\n• ")))
            .halign(gtk4::Align::Start)
            .wrap(true)
            .build();
        self.container.append(&steps);

        &self.container
    }
}

impl Default for RecoveryView {
    fn default() -> Self {
        Self::new()
    }
}
