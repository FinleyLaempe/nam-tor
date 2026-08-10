// SPDX-License-Identifier: GPL-3.0-only
//! Navigation controller for the enrollment flow using AdwNavigationView.
//
// allow: SIZE_OK — the three `create_*_page` builders are integral parts of
// the `EnrollmentNavigation` controller (they construct the widgets the
// struct stores). Splitting them into a submodule would scatter one
// cohesive widget's construction across files. The pre-existing size
// (309 LOC) predates the task-13 accessibility additions; the separable
// concern (ProgressAnnouncer) was already extracted to `a11y.rs`.
//
//! This module manages the drill-down navigation through the enrollment
//! states: Setup → Capture → Review. Each page is an `AdwNavigationPage`
//! with consistent headers and back navigation.

use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, Button, Entry, Label, Orientation};
use libadwaita as adw;

use std::time::Instant;

use crate::tr;
use crate::view_model::{EnrollmentViewV3, ReviewViewV3};

pub use crate::a11y::{ProgressAnnouncer, PROGRESS_ANNOUNCE_INTERVAL};

/// Navigation controller for the enrollment flow.
pub struct EnrollmentNavigation {
    pub navigation_view: adw::NavigationView,
    pub setup_page: adw::NavigationPage,
    pub capture_page: adw::NavigationPage,
    pub review_page: adw::NavigationPage,
    pub start_button: Button,
    pub name_entry: Entry,
    pub stop_button: Button,
    pub save_button: Button,
    pub resume_button: Button,
    pub discard_button: Button,
    pub progress_label: Label,
    pub guidance_label: Label,
    pub summary_label: Label,
    pub progress_announcer: ProgressAnnouncer,
}

impl EnrollmentNavigation {
    /// Create a new enrollment navigation controller.
    pub fn new() -> Self {
        // Create the navigation view
        let navigation_view = adw::NavigationView::new();

        // Setup page: enter name, privacy notice, Start button
        let (setup_page, start_button, name_entry) = Self::create_setup_page();

        // Capture page: progress, guidance, Stop button
        let (capture_page, stop_button, progress_label, guidance_label) =
            Self::create_capture_page();

        // Review page: summary, Save/Resume/Discard buttons
        let (review_page, save_button, resume_button, discard_button, summary_label) =
            Self::create_review_page();

        Self {
            navigation_view,
            setup_page,
            capture_page,
            review_page,
            start_button,
            name_entry,
            stop_button,
            save_button,
            resume_button,
            discard_button,
            progress_label,
            guidance_label,
            summary_label,
            progress_announcer: ProgressAnnouncer::new(),
        }
    }

    /// Create the setup page with name entry and privacy notice.
    fn create_setup_page() -> (adw::NavigationPage, Button, Entry) {
        let name_entry = Entry::builder()
            .placeholder_text(tr("Enter profile name…"))
            .accessible_role(gtk4::AccessibleRole::TextBox)
            .build();
        name_entry.update_property(&[
            gtk4::accessible::Property::Label(&tr("Profile name")),
            gtk4::accessible::Property::Description(&tr(
                "Enter a descriptive name for this profile",
            )),
        ]);

        let name_label = Label::builder()
            .label(tr("Profile Name"))
            .halign(Align::Start)
            .css_classes(["heading"])
            .build();
        name_label.set_mnemonic_widget(Some(&name_entry));

        let name_box = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();
        name_box.append(&name_label);
        name_box.append(&name_entry);

        let privacy_label = Label::builder()
            .label(tr("Continuous capture mode\n\
                 Frames are analyzed locally and no raw images are stored.\n\
                 Only the encrypted face model is saved on this device."))
            .halign(Align::Start)
            .justify(gtk4::Justification::Center)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        privacy_label.set_accessible_role(gtk4::AccessibleRole::Note);

        let start_button = Button::builder()
            .label(tr("_Start"))
            .halign(Align::Center)
            .css_classes(["suggested-action"])
            .use_underline(true)
            .tooltip_text(tr("Begin face enrollment capture"))
            .build();
        start_button.update_property(&[
            gtk4::accessible::Property::Label(&tr("Start enrollment")),
            gtk4::accessible::Property::Description(&tr(
                "Begin the face enrollment capture process",
            )),
        ]);

        let content = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .margin_start(24)
            .margin_end(24)
            .margin_top(24)
            .margin_bottom(24)
            .spacing(12)
            .build();
        content.append(&name_box);
        content.append(&privacy_label);
        content.append(&start_button);

        let page = adw::NavigationPage::builder()
            .title(tr("Enroll Face"))
            .child(&content)
            .build();

        (page, start_button, name_entry)
    }

    /// Create the capture page with progress and guidance.
    fn create_capture_page() -> (adw::NavigationPage, Button, Label, Label) {
        let activity = gtk4::Spinner::builder()
            .spinning(true)
            .halign(Align::Center)
            .valign(Align::Center)
            .build();
        activity.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Capture in progress",
        ))]);

        let progress_label = Label::builder()
            .label(tr("Frames: 0 · Quality: 0% · Diversity: 0%"))
            .halign(Align::Center)
            .css_classes(["heading"])
            .build();
        progress_label.set_accessible_role(gtk4::AccessibleRole::Status);
        progress_label.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Enrollment capture progress",
        ))]);

        let guidance_label = Label::builder()
            .label(tr("Looking good…"))
            .halign(Align::Center)
            .css_classes(["dim-label"])
            .wrap(true)
            .build();
        guidance_label.set_accessible_role(gtk4::AccessibleRole::Status);
        guidance_label.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Live capture guidance",
        ))]);

        let stop_button = Button::builder()
            .label(tr("_Stop"))
            .halign(Align::Center)
            .css_classes(["destructive-action"])
            .use_underline(true)
            .tooltip_text(tr("Stop capture and review the result"))
            .build();
        stop_button.update_property(&[
            gtk4::accessible::Property::Label(&tr("Stop capture")),
            gtk4::accessible::Property::Description(&tr(
                "Stop capturing and review the enrolled profile",
            )),
        ]);

        let content = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .margin_start(24)
            .margin_end(24)
            .margin_top(48)
            .margin_bottom(48)
            .spacing(16)
            .build();
        content.append(&activity);
        content.append(&progress_label);
        content.append(&guidance_label);
        content.append(&stop_button);

        let page = adw::NavigationPage::builder()
            .title(tr("Capturing"))
            .child(&content)
            .build();

        (page, stop_button, progress_label, guidance_label)
    }

    /// Create the review page with summary and actions.
    fn create_review_page() -> (adw::NavigationPage, Button, Button, Button, Label) {
        let summary_label = Label::builder()
            .label(tr("Frames accepted: 0\n\
                 Quality: 0%\n\
                 Diversity: 0%\n\
                 Ready to save"))
            .halign(Align::Center)
            .justify(gtk4::Justification::Center)
            .wrap(true)
            .css_classes(["heading"])
            .build();
        summary_label.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Enrollment review summary",
        ))]);

        let button_box = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(12)
            .halign(Align::Center)
            .margin_top(12)
            .build();

        let save_button = Button::builder()
            .label(tr("_Save"))
            .halign(Align::Center)
            .css_classes(["suggested-action"])
            .use_underline(true)
            .tooltip_text(tr("Save the enrolled profile"))
            .build();
        save_button.update_property(&[
            gtk4::accessible::Property::Label(&tr("Save profile")),
            gtk4::accessible::Property::Description(&tr("Commit the captured profile to storage")),
        ]);

        let resume_button = Button::builder()
            .label(tr("_Resume"))
            .halign(Align::Center)
            .use_underline(true)
            .tooltip_text(tr("Resume capture to improve quality"))
            .build();
        resume_button.update_property(&[
            gtk4::accessible::Property::Label(&tr("Resume capture")),
            gtk4::accessible::Property::Description(&tr("Return to capturing to add more frames")),
        ]);

        let discard_button = Button::builder()
            .label(tr("_Discard"))
            .halign(Align::Center)
            .css_classes(["destructive-action"])
            .use_underline(true)
            .tooltip_text(tr("Discard the captured profile"))
            .build();
        discard_button.update_property(&[
            gtk4::accessible::Property::Label(&tr("Discard profile")),
            gtk4::accessible::Property::Description(&tr(
                "Throw away the captured profile without saving",
            )),
        ]);

        button_box.append(&save_button);
        button_box.append(&resume_button);
        button_box.append(&discard_button);

        let content = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .margin_start(24)
            .margin_end(24)
            .margin_top(24)
            .margin_bottom(24)
            .spacing(12)
            .build();
        content.append(&summary_label);
        content.append(&button_box);

        let page = adw::NavigationPage::builder()
            .title(tr("Review"))
            .child(&content)
            .build();

        (
            page,
            save_button,
            resume_button,
            discard_button,
            summary_label,
        )
    }

    /// Push the setup page onto the navigation stack.
    pub fn show_setup(&self) {
        self.navigation_view.push(&self.setup_page);
    }

    /// Push the capture page onto the navigation stack.
    pub fn show_capture(&self) {
        self.navigation_view.push(&self.capture_page);
    }

    /// Push the review page onto the navigation stack.
    pub fn show_review(&self) {
        self.navigation_view.push(&self.review_page);
    }

    /// Pop the current page and return to the previous one.
    pub fn pop(&self) {
        self.navigation_view.pop();
    }

    /// Pop all pages and return to the root.
    pub fn pop_to_root(&self) {
        // Pop until we're at the root - pop_to_tag needs a string tag which we don't use
        while self
            .navigation_view
            .visible_page()
            .map(|p| p != self.setup_page)
            .unwrap_or(false)
        {
            self.navigation_view.pop();
        }
    }

    /// Update the capture progress with live progress. The screen-reader
    /// announcement is gated by [`ProgressAnnouncer`] so AT-SPI is not
    /// spammed faster than 4 times per second.
    pub fn update_capture_progress(&self, view: &EnrollmentViewV3) {
        let text = format!(
            "Frames: {} · Quality: {}% · Diversity: {}%",
            view.frames_accepted, view.quality_percent, view.diversity_percent
        );
        self.progress_label.set_text(&tr(&text));
        self.guidance_label.set_text(&tr(&view.guidance));
    }

    /// Update the capture progress and return whether the change should be
    /// announced to the screen reader, honouring the 250 ms throttle.
    pub fn update_capture_progress_throttled(
        &mut self,
        view: &EnrollmentViewV3,
        now: Instant,
    ) -> bool {
        let announce = self.progress_announcer.should_announce(now);
        let text = format!(
            "Frames: {} · Quality: {}% · Diversity: {}%",
            view.frames_accepted, view.quality_percent, view.diversity_percent
        );
        self.progress_label.set_text(&tr(&text));
        self.guidance_label.set_text(&tr(&view.guidance));
        announce
    }

    /// Update the review summary with the summary.
    pub fn update_review_summary(&self, view: &ReviewViewV3) {
        let ready_text = if view.ready_to_save {
            tr("Ready to save")
        } else {
            tr("Consider re-enrolling")
        };
        let body = format!(
            "Frames accepted: {}\n\
             Quality: {}%\n\
             Diversity: {}%\n\
             {}",
            view.frames_accepted, view.quality_percent, view.diversity_percent, ready_text
        );
        self.summary_label.set_text(&tr(&body));
    }

    /// Set the sensitive state of the start button.
    pub fn set_start_sensitive(&self, sensitive: bool) {
        self.start_button.set_sensitive(sensitive);
    }

    /// Get the profile name from the entry.
    pub fn profile_name(&self) -> String {
        self.name_entry.text().to_string()
    }

    /// Clear the name entry.
    pub fn clear_name_entry(&self) {
        self.name_entry.set_text("");
    }
}

impl Default for EnrollmentNavigation {
    fn default() -> Self {
        Self::new()
    }
}
