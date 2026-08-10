// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment review view component.
//!
//! Displays a summary of the capture quality and coverage before the user
//! commits the profile to the store. No biometric data or raw scores are
//! exposed.

use crate::tr;
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
        on_save: impl Fn() + 'static,
        on_resume: impl Fn() + 'static,
        on_discard: impl Fn() + 'static,
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
            .label(tr("Review Enrollment"))
            .css_classes(["title-1"])
            .halign(gtk4::Align::Start)
            .build();
        title.set_accessible_role(gtk4::AccessibleRole::Heading);
        title.update_property(&[gtk4::accessible::Property::Level(1)]);
        group.append(&title);

        let summary_box = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(8)
            .build();

        let quality_label = Label::builder()
            .label(tr(&format!("Quality: {}%", view.quality_percent)))
            .halign(gtk4::Align::Start)
            .build();
        quality_label.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Capture quality percentage",
        ))]);
        summary_box.append(&quality_label);

        let diversity_label = Label::builder()
            .label(tr(&format!("Diversity: {}%", view.diversity_percent)))
            .halign(gtk4::Align::Start)
            .build();
        diversity_label.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Pose diversity percentage",
        ))]);
        summary_box.append(&diversity_label);

        let frames_label = Label::builder()
            .label(tr(&format!("Frames accepted: {}", view.frames_accepted)))
            .halign(gtk4::Align::Start)
            .build();
        frames_label.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Number of accepted frames",
        ))]);
        summary_box.append(&frames_label);

        let guidance_label = Label::builder()
            .label(tr(&view.guidance))
            .halign(gtk4::Align::Start)
            .wrap(true)
            .css_classes(["caption"])
            .build();
        guidance_label.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Enrollment guidance",
        ))]);
        summary_box.append(&guidance_label);

        group.append(&summary_box);

        let action_box = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(12)
            .halign(gtk4::Align::End)
            .build();

        let save_btn = gtk4::Button::builder()
            .label(tr("_Save Profile"))
            .css_classes(["suggested-action"])
            .sensitive(!is_pending)
            .use_underline(true)
            .tooltip_text(tr("Save the enrolled profile"))
            .build();
        save_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Save profile")),
            gtk4::accessible::Property::Description(&tr("Commit the captured profile to storage")),
        ]);
        save_btn.connect_clicked(move |_| on_save());

        let resume_btn = gtk4::Button::builder()
            .label(tr("_Resume Capture"))
            .sensitive(!is_pending)
            .use_underline(true)
            .tooltip_text(tr("Resume capture to improve quality"))
            .build();
        resume_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Resume capture")),
            gtk4::accessible::Property::Description(&tr("Return to capturing to add more frames")),
        ]);
        resume_btn.connect_clicked(move |_| on_resume());

        let discard_btn = gtk4::Button::builder()
            .label(tr("_Discard"))
            .sensitive(!is_pending)
            .use_underline(true)
            .css_classes(["destructive-action"])
            .tooltip_text(tr("Discard the captured profile"))
            .build();
        discard_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Discard profile")),
            gtk4::accessible::Property::Description(&tr(
                "Throw away the captured profile without saving",
            )),
        ]);
        discard_btn.connect_clicked(move |_| on_discard());

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
