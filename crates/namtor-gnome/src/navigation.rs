// SPDX-License-Identifier: GPL-3.0-only
//! Navigation controller for the enrollment flow using AdwNavigationView.
//!
//! This module manages the drill-down navigation through the enrollment
//! states: Setup → Capture → Review. Each page is an `AdwNavigationPage`
//! with consistent headers and back navigation.

use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, Button, Entry, Label, Orientation};
use libadwaita as adw;

use crate::view_model::{EnrollmentViewV3, ReviewViewV3};

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
}

impl EnrollmentNavigation {
    /// Create a new enrollment navigation controller.
    pub fn new() -> Self {
        // Create the navigation view
        let navigation_view = adw::NavigationView::new();

        // Setup page: enter name, privacy notice, Start button
        let (setup_page, start_button, name_entry) = Self::create_setup_page();

        // Capture page: progress, guidance, Stop button
        let (capture_page, stop_button, progress_label, guidance_label) = Self::create_capture_page();

        // Review page: summary, Save/Resume/Discard buttons
        let (review_page, save_button, resume_button, discard_button, summary_label) = Self::create_review_page();

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
        }
    }

    /// Create the setup page with name entry and privacy notice.
    fn create_setup_page() -> (adw::NavigationPage, Button, Entry) {
        let name_entry = Entry::builder()
            .placeholder_text("Enter profile name…")
            .accessible_role(gtk4::AccessibleRole::TextBox)
            .build();

        let name_label = Label::builder()
            .label("Profile Name")
            .halign(Align::Start)
            .css_classes(["heading"])
            .build();

        let name_box = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();
        name_box.append(&name_label);
        name_box.append(&name_entry);

        let privacy_label = Label::builder()
            .label(
                "Continuous capture mode\n\
                 Frames are analyzed locally and no raw images are stored.\n\
                 Only mathematical embeddings are saved.",
            )
            .halign(Align::Start)
            .justify(gtk4::Justification::Center)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();

        let start_button = Button::builder()
            .label("_Start")
            .halign(Align::Center)
            .css_classes(["suggested-action"])
            .use_underline(true)
            .build();

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
            .title("Enroll Face")
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

        let progress_label = Label::builder()
            .label("Frames: 0 · Quality: 0% · Diversity: 0%")
            .halign(Align::Center)
            .css_classes(["heading"])
            .build();

        let guidance_label = Label::builder()
            .label("Looking good…")
            .halign(Align::Center)
            .css_classes(["dim-label"])
            .wrap(true)
            .build();

        let stop_button = Button::builder()
            .label("_Stop")
            .halign(Align::Center)
            .css_classes(["destructive-action"])
            .use_underline(true)
            .build();

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
            .title("Capturing")
            .child(&content)
            .build();

        (page, stop_button, progress_label, guidance_label)
    }

    /// Create the review page with summary and actions.
    fn create_review_page() -> (adw::NavigationPage, Button, Button, Button, Label) {
        let summary_label = Label::builder()
            .label(
                "Frames accepted: 0\n\
                 Quality: 0%\n\
                 Diversity: 0%\n\
                 Ready to save",
            )
            .halign(Align::Center)
            .justify(gtk4::Justification::Center)
            .wrap(true)
            .css_classes(["heading"])
            .build();

        let button_box = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(12)
            .halign(Align::Center)
            .margin_top(12)
            .build();

        let save_button = Button::builder()
            .label("_Save")
            .halign(Align::Center)
            .css_classes(["suggested-action"])
            .use_underline(true)
            .build();

        let resume_button = Button::builder()
            .label("_Resume")
            .halign(Align::Center)
            .use_underline(true)
            .build();

        let discard_button = Button::builder()
            .label("_Discard")
            .halign(Align::Center)
            .css_classes(["destructive-action"])
            .use_underline(true)
            .build();

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
            .title("Review")
            .child(&content)
            .build();

        (page, save_button, resume_button, discard_button, summary_label)
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
        while self.navigation_view.visible_page().map(|p| p != self.setup_page).unwrap_or(false) {
            self.navigation_view.pop();
        }
    }

    /// Update the capture progress with live progress.
    pub fn update_capture_progress(&self, view: &EnrollmentViewV3) {
        self.progress_label.set_text(&format!(
            "Frames: {} · Quality: {}% · Diversity: {}%",
            view.frames_accepted, view.quality_percent, view.diversity_percent
        ));
        self.guidance_label.set_text(&view.guidance);
    }

    /// Update the review summary with the summary.
    pub fn update_review_summary(&self, view: &ReviewViewV3) {
        let ready_text = if view.ready_to_save {
            "Ready to save"
        } else {
            "Consider re-enrolling"
        };
        self.summary_label.set_text(&format!(
            "Frames accepted: {}\n\
             Quality: {}%\n\
             Diversity: {}%\n\
             {}",
            view.frames_accepted, view.quality_percent, view.diversity_percent, ready_text
        ));
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
