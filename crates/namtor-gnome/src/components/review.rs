// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment review view component.
//!
//! Displays a summary of the capture quality and coverage before the user
//! commits the profile to the store. No biometric data or raw scores are
//! exposed.

use crate::view_model::ReviewView;
use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Label, Orientation};

pub struct ReviewViewComponent {
    container: GtkBox,
}

impl ReviewViewComponent {
    pub fn new() -> Self {
        let container = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(12)
            .build();
        Self { container }
    }

    /// Renders the review summary.
    pub fn render(
        &self,
        view: &ReviewView,
        _on_commit: impl Fn() + 'static,
        _on_retry: impl Fn() + 'static,
        _on_cancel: impl Fn() + 'static,
    ) -> &GtkBox {
        while let Some(child) = self.container.first_child() {
            self.container.remove(&child);
        }

        let group = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(16)
            .build();

        let title = Label::builder()
            .label("Review Enrollment")
            .css_classes(["title-1"])
            .halign(gtk4::Align::Start)
            .build();
        group.append(&title);

        let summary_box = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(8)
            .build();

        let coverage_label = Label::builder()
            .label(format!("Coverage: {}%", view.coverage_percent))
            .halign(gtk4::Align::Start)
            .build();
        summary_box.append(&coverage_label);

        let quality_label = Label::builder()
            .label(format!("Quality: {}", view.quality_label))
            .halign(gtk4::Align::Start)
            .build();
        summary_box.append(&quality_label);

        let template_label = Label::builder()
            .label(format!("Templates: {} captures", view.template_count))
            .halign(gtk4::Align::Start)
            .build();
        summary_box.append(&template_label);

        group.append(&summary_box);

        let btn_box = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(12)
            .halign(gtk4::Align::End)
            .build();

        let cancel_btn = gtk4::Button::builder().label("Cancel").build();
        // In a real implementation, we would connect on_cancel here.

        let retry_btn = gtk4::Button::builder().label("Retry").build();
        // In a real implementation, we would connect on_retry here.

        let commit_btn = gtk4::Button::builder()
            .label("Finish")
            .css_classes(["suggested-action"])
            .build();
        // In a real implementation, we would connect on_commit here.

        btn_box.append(&cancel_btn);
        btn_box.append(&retry_btn);
        btn_box.append(&commit_btn);
        group.append(&btn_box);

        self.container.append(&group);

        &self.container
    }
}

impl Default for ReviewViewComponent {
    fn default() -> Self {
        Self::new()
    }
}
