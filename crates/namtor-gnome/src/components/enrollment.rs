// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment progress view component.
//!
//! Provides the user interface for creating a new profile, including
//! identity entry, privacy notices, and real-time capture progress.

use crate::tr;
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

    fn render_setup_phase<F>(&self, on_start: F, on_cancel: impl Fn() + 'static)
    where
        F: Fn(&str) + 'static,
    {
        let group = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(16)
            .build();

        let title = Label::builder()
            .label(tr("Add New Profile"))
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
            .label(tr("_Profile Name"))
            .halign(gtk4::Align::Start)
            .use_underline(true)
            .build();
        name_label.set_accessible_role(gtk4::AccessibleRole::Label);
        name_box.append(&name_label);

        let name_entry = gtk4::Entry::builder()
            .placeholder_text(tr("e.g. Home Office"))
            .accessible_role(gtk4::AccessibleRole::TextBox)
            .build();
        name_entry.update_property(&[
            gtk4::accessible::Property::Label(&tr("Profile name")),
            gtk4::accessible::Property::Description(&tr(
                "Enter a descriptive name for this profile",
            )),
        ]);
        name_label.set_mnemonic_widget(Some(&name_entry));
        name_box.append(&name_entry);
        group.append(&name_box);

        // Privacy Explanation
        let privacy_box = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();

        let privacy_text = Label::builder()
            .label(tr(
                "Your face model is encrypted and stored locally on this device. No face data leaves the system.",
            ))
            .wrap(true)
            .css_classes(["caption"])
            .halign(gtk4::Align::Start)
            .build();
        privacy_text.set_accessible_role(gtk4::AccessibleRole::Note);
        privacy_box.append(&privacy_text);
        group.append(&privacy_box);

        let start_btn = gtk4::Button::builder()
            .label(tr("_Start Enrollment"))
            .css_classes(["suggested-action"])
            .halign(gtk4::Align::End)
            .use_underline(true)
            .tooltip_text(tr("Begin the face enrollment capture process"))
            .build();
        start_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Start enrollment")),
            gtk4::accessible::Property::Description(&tr(
                "Begin the face enrollment capture process",
            )),
        ]);
        let entry_for_start = name_entry.clone();
        start_btn.connect_clicked(move |_| {
            let name = entry_for_start.text().to_string();
            on_start(&name);
        });
        group.append(&start_btn);

        let cancel_btn = gtk4::Button::builder()
            .label(tr("Cancel"))
            .margin_top(12)
            .halign(gtk4::Align::Center)
            .tooltip_text(tr("Cancel profile creation"))
            .build();
        cancel_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Cancel enrollment")),
            gtk4::accessible::Property::Description(&tr(
                "Discard the new profile and return to the list",
            )),
        ]);
        cancel_btn.connect_clicked(move |_| on_cancel());

        group.append(&cancel_btn);

        self.container.append(&group);
    }

    fn render_capture_phase<F1, F2>(&self, view: &EnrollmentViewV3, on_stop: F1, on_cancel: F2)
    where
        F1: Fn() + 'static,
        F2: Fn() + 'static,
    {
        let container = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(16)
            .build();

        let spinner = gtk4::Spinner::builder().halign(gtk4::Align::Center).build();
        spinner.start();
        container.append(&spinner);

        let stats_box = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(24)
            .halign(gtk4::Align::Center)
            .build();

        let quality_label = Label::builder()
            .label(tr(&format!("Quality: {}%", view.quality_percent)))
            .build();
        quality_label.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Capture quality percentage",
        ))]);
        stats_box.append(&quality_label);

        let diversity_label = Label::builder()
            .label(tr(&format!("Diversity: {}%", view.diversity_percent)))
            .build();
        diversity_label.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Pose diversity percentage",
        ))]);
        stats_box.append(&diversity_label);

        container.append(&stats_box);

        let counts_label = Label::builder()
            .label(tr(&format!(
                "Accepted: {} / Seen: {}",
                view.frames_accepted, view.frames_seen
            )))
            .css_classes(["caption"])
            .halign(gtk4::Align::Center)
            .build();
        counts_label.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Frames accepted and seen counts",
        ))]);
        container.append(&counts_label);

        let guidance = Label::builder()
            .label(tr(&view.guidance))
            .halign(gtk4::Align::Center)
            .wrap(true)
            .css_classes(["guidance"])
            .build();
        guidance.set_accessible_role(gtk4::AccessibleRole::Status);
        guidance.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Live capture guidance",
        ))]);
        container.append(&guidance);

        let bar = gtk4::ProgressBar::builder()
            .fraction(view.diversity_percent as f64 / 100.0)
            .margin_top(12)
            .build();
        bar.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Enrollment diversity progress",
        ))]);
        container.append(&bar);

        let action_box = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(12)
            .halign(gtk4::Align::Center)
            .margin_top(24)
            .build();

        let stop_btn = gtk4::Button::builder()
            .label(tr("_Stop"))
            .css_classes(["suggested-action"])
            .use_underline(true)
            .tooltip_text(tr("Stop capture and review the result"))
            .build();
        stop_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Stop capture")),
            gtk4::accessible::Property::Description(&tr(
                "Stop capturing and review the enrolled profile",
            )),
        ]);
        stop_btn.connect_clicked(move |_| on_stop());

        let cancel_btn = gtk4::Button::builder()
            .label(tr("Cancel"))
            .tooltip_text(tr("Cancel enrollment and discard frames"))
            .build();
        cancel_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Cancel enrollment")),
            gtk4::accessible::Property::Description(&tr(
                "Discard the enrollment and return to the profile list",
            )),
        ]);
        cancel_btn.connect_clicked(move |_| on_cancel());

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
