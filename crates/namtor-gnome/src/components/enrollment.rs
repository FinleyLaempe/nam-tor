// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment progress view component.
//!
//! Provides the user interface for creating a new profile, including
//! identity entry, privacy notices, and real-time capture progress.

use crate::view_model::EnrollmentView;
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
        view: Option<&EnrollmentView>,
        _on_start: impl Fn(&str) + 'static,
        _on_cancel: impl Fn() + 'static,
    ) -> &GtkBox {
        while let Some(child) = self.container.first_child() {
            self.container.remove(&child);
        }

        if let Some(enrollment) = view {
            self.render_capture_phase(enrollment);
        } else {
            self.render_setup_phase(_on_start);
        }

        let cancel_btn = gtk4::Button::builder()
            .label("Cancel")
            .margin_top(12)
            .halign(gtk4::Align::Center)
            .build();

        self.container.append(&cancel_btn);

        &self.container
    }

    fn render_setup_phase<F>(&self, _on_start: F)
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

        self.container.append(&group);
    }

    fn render_capture_phase(&self, view: &EnrollmentView) {
        let frame = gtk4::Frame::builder().label("Capturing").build();
        frame.update_property(&[gtk4::accessible::Property::Label(
            "Enrollment Capture Progress",
        )]);

        let inner = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .build();

        let progress_text = Label::builder()
            .label(format!(
                "Frames captured: {}  ·  Coverage: {}%",
                view.frames_captured, view.coverage_percent
            ))
            .halign(gtk4::Align::Start)
            .build();
        progress_text.set_accessible_role(gtk4::AccessibleRole::Status);
        inner.append(&progress_text);

        if view.quality.show_guidance {
            let guidance = Label::builder()
                .label("Low quality — look at the camera and slowly turn your head left and right.")
                .halign(gtk4::Align::Start)
                .css_classes(["warning"])
                .wrap(true)
                .accessible_role(gtk4::AccessibleRole::Alert)
                .build();
            guidance.update_property(&[gtk4::accessible::Property::Label("Quality Guidance")]);
            inner.append(&guidance);
        }

        let bar = gtk4::ProgressBar::builder()
            .fraction(view.coverage_percent as f64 / 100.0)
            .build();
        bar.update_property(&[gtk4::accessible::Property::Label(
            "Enrollment Coverage Progress",
        )]);
        inner.append(&bar);

        frame.set_child(Some(&inner));
        self.container.append(&frame);
    }
}

impl Default for EnrollmentViewComponent {
    fn default() -> Self {
        Self::new()
    }
}
