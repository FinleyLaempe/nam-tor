// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment review view component.
//!
//! Displays a summary of the capture quality and coverage before the user
//! commits the profile to the store. No biometric data or raw scores are
//! exposed.

use crate::view_model::ReviewViewV3;
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
        view: &ReviewViewV3,
        _on_save: impl Fn() + 'static,
        _on_resume: impl Fn() + 'static,
        _on_discard: impl Fn() + 'static,
        is_pending: bool,
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

        let quality_label = Label::builder()
            .label(format!("Quality: {}%", view.quality_percent))
            .halign(gtk4::Align::Start)
            .build();
        summary_box.append(&quality_label);

        let diversity_label = Label::builder()
            .label(format!("Diversity: {}%", view.diversity_percent))
            .halign(gtk4::Align::Start)
            .build();
        summary_box.append(&diversity_label);

        let frames_label = Label::builder()
            .label(format!("Frames accepted: {}", view.frames_accepted))
            .halign(gtk4::Align::Start)
            .build();
        summary_box.append(&frames_label);

        let guidance_label = Label::builder()
            .label(&view.guidance)
            .halign(gtk4::Align::Start)
            .wrap(true)
            .css_classes(["caption"])
            .build();
        summary_box.append(&guidance_label);

        group.append(&summary_box);

        let action_box = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(12)
            .halign(gtk4::Align::End)
            .build();

        let save_btn = gtk4::Button::builder()
            .label("Save Profile")
            .css_classes(["suggested-action"])
            .sensitive(!is_pending)
            .build();
        
        let resume_btn = gtk4::Button::builder()
            .label("Resume Capture")
            .sensitive(!is_pending)
            .build();

        let discard_btn = gtk4::Button::builder()
            .label("Discard")
            .sensitive(!is_pending)
            .build();

        action_box.append(&discard_btn);
        action_box.append(&resume_btn);
        action_box.append(&save_btn);
        group.append(&action_box);

        self.container.append(&group);

        &self.container
    }
}

impl Default for ReviewViewComponent {
    fn default() -> Self {
        Self::new()
    }
}
