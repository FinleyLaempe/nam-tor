// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment progress view component.
//!
//! Provides the user interface for creating a new profile, including
//! identity entry, privacy notices, and real-time capture progress.

use crate::view_model::EnrollmentViewV3;
use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Label, Orientation};

pub struct EnrollmentViewComponent {
    container: GtkBox,
}

impl EnrollmentViewComponent {
    pub fn new() -> Self {
        let container = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(12)
            .build();
        Self { container }
    }

    /// Renders the enrollment state.
    ///
    /// If `view` is `None`, the component renders the setup phase (Name entry + Privacy).
    /// If `view` is `Some`, it renders the capture progress phase.
    pub fn render(
        &self,
        view: Option<&EnrollmentViewV3>,
        on_start: impl Fn(&str) + 'static,
        on_stop: impl Fn() + 'static,
        on_cancel: impl Fn() + 'static,
    ) -> &GtkBox {
        while let Some(child) = self.container.first_child() {
            self.container.remove(&child);
        }

        if let Some(enrollment) = view {
            self.render_capture_phase(enrollment, on_stop, on_cancel);
        } else {
            self.render_setup_phase(on_start, on_cancel);
        }

        &self.container
    }

    fn render_setup_phase<F>(&self, _on_start: F, _on_cancel: impl Fn() + 'static)
    where
        F: Fn(&str) + 'static,
    {
        let group = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(16)
            .build();

        let title = Label::builder()
            .label("Add New Profile")
            .css_classes(["title-1"])
            .halign(gtk4::Align::Start)
            .build();
        title.set_accessible_role(gtk4::AccessibleRole::Heading);
        title.update_property(&[gtk4::accessible::Property::Level(1)]);
        group.append(&title);

        // Name Entry
        let name_box = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();

        let name_label = Label::builder()
            .label("Profile Name")
            .halign(gtk4::Align::Start)
            .build();
        name_label.set_accessible_role(gtk4::AccessibleRole::Label);
        name_box.append(&name_label);

        let name_entry = gtk4::Entry::builder()
            .placeholder_text("e.g. Home Office")
            .accessible_role(gtk4::AccessibleRole::TextBox)
            .build();
        name_entry.update_property(&[
            gtk4::accessible::Property::Label("Profile Name"),
            gtk4::accessible::Property::Description(
                "Enter a descriptive name for this facial profile",
            ),
        ]);
        name_box.append(&name_entry);
        group.append(&name_box);

        // Privacy Explanation
        let privacy_box = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();

        let privacy_text = Label::builder()
            .label("Your biometric templates are encrypted and stored locally on this device. No biometric data leaves the system.")
            .wrap(true)
            .css_classes(["caption"])
            .halign(gtk4::Align::Start)
            .build();
        privacy_text.set_accessible_role(gtk4::AccessibleRole::Note);
        privacy_box.append(&privacy_text);
        group.append(&privacy_box);

        let start_btn = gtk4::Button::builder()
            .label("Start Enrollment")
            .css_classes(["suggested-action"])
            .halign(gtk4::Align::End)
            .build();
        start_btn.update_property(&[gtk4::accessible::Property::Description(
            "Begin the facial capture process",
        )]);
        group.append(&start_btn);

        let cancel_btn = gtk4::Button::builder()
            .label("Cancel")
            .margin_top(12)
            .halign(gtk4::Align::Center)
            .build();
        
        group.append(&cancel_btn);

        self.container.append(&group);
    }

    fn render_capture_phase<F1, F2>(&self, view: &EnrollmentViewV3, _on_stop: F1, _on_cancel: F2)
    where
        F1: Fn() + 'static,
        F2: Fn() + 'static,
    {
        let container = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(16)
            .build();

        let spinner = gtk4::Spinner::builder()
            .halign(gtk4::Align::Center)
            .build();
        spinner.start();
        container.append(&spinner);

        let stats_box = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(24)
            .halign(gtk4::Align::Center)
            .build();

        let quality_label = Label::builder()
            .label(format!("Quality: {}%", view.quality_percent))
            .build();
        stats_box.append(&quality_label);

        let diversity_label = Label::builder()
            .label(format!("Diversity: {}%", view.diversity_percent))
            .build();
        stats_box.append(&diversity_label);

        container.append(&stats_box);

        let counts_label = Label::builder()
            .label(format!(
                "Accepted: {} / Seen: {}",
                view.frames_accepted, view.frames_seen
            ))
            .css_classes(["caption"])
            .halign(gtk4::Align::Center)
            .build();
        container.append(&counts_label);

        let guidance = Label::builder()
            .label(&view.guidance)
            .halign(gtk4::Align::Center)
            .wrap(true)
            .css_classes(["guidance"])
            .build();
        guidance.set_accessible_role(gtk4::AccessibleRole::Status);
        container.append(&guidance);

        let bar = gtk4::ProgressBar::builder()
            .fraction(view.diversity_percent as f64 / 100.0)
            .margin_top(12)
            .build();
        bar.update_property(&[gtk4::accessible::Property::Label(
            "Enrollment Diversity Progress",
        )]);
        container.append(&bar);

        let action_box = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(12)
            .halign(gtk4::Align::Center)
            .margin_top(24)
            .build();

        let stop_btn = gtk4::Button::builder()
            .label("Stop")
            .css_classes(["suggested-action"])
            .build();
        
        let cancel_btn = gtk4::Button::builder()
            .label("Cancel")
            .build();

        action_box.append(&stop_btn);
        action_box.append(&cancel_btn);
        container.append(&action_box);

        self.container.append(&container);
    }
}

impl Default for EnrollmentViewComponent {
    fn default() -> Self {
        Self::new()
    }
}
