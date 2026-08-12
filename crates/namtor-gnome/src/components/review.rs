// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment review view component.
//!
//! Displays a summary of the capture quality and coverage before the user
//! commits the profile to the store. No biometric data or raw scores are
//! exposed. Builds an [`adw::PreferencesPage`] (wrapped in a
//! [`adw::NavigationPage`] by the caller for `push_subpage`).

use crate::tr;
use crate::view_model::ReviewViewV3;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::{PreferencesGroupExt, PreferencesPageExt};

pub struct ReviewViewComponent {
    page: adw::PreferencesPage,
    group: Option<adw::PreferencesGroup>,
}

impl ReviewViewComponent {
    pub fn new() -> Self {
        let page = adw::PreferencesPage::new();
        Self { page, group: None }
    }

    /// Renders the review summary into the component's `PreferencesPage`.
    pub fn render(
        &mut self,
        view: &ReviewViewV3,
        on_save: impl Fn() + 'static,
        on_resume: impl Fn() + 'static,
        on_discard: impl Fn() + 'static,
        is_pending: bool,
    ) -> &adw::PreferencesPage {
        self.clear();

        let group = adw::PreferencesGroup::builder()
            .title(tr("Review Enrollment"))
            .build();
        group.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Enrollment review group",
        ))]);

        let quality_row = adw::ActionRow::builder()
            .title(tr(&format!("Quality: {}%", view.quality_percent)))
            .build();
        quality_row.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Capture quality percentage",
        ))]);
        group.add(&quality_row);

        let diversity_row = adw::ActionRow::builder()
            .title(tr(&format!("Diversity: {}%", view.diversity_percent)))
            .build();
        diversity_row.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Pose diversity percentage",
        ))]);
        group.add(&diversity_row);

        let frames_row = adw::ActionRow::builder()
            .title(tr(&format!("Frames accepted: {}", view.frames_accepted)))
            .build();
        frames_row.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Number of accepted frames",
        ))]);
        group.add(&frames_row);

        let guidance_row = adw::ActionRow::builder().title(tr(&view.guidance)).build();
        guidance_row.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Enrollment guidance",
        ))]);
        group.add(&guidance_row);

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

        group.add(&discard_btn);
        group.add(&resume_btn);
        group.add(&save_btn);

        self.page.add(&group);
        self.group = Some(group);

        &self.page
    }

    fn clear(&mut self) {
        if let Some(g) = self.group.take() {
            self.page.remove(&g);
        }
    }

    /// Get a reference to the underlying `PreferencesPage`.
    pub fn page(&self) -> &adw::PreferencesPage {
        &self.page
    }

    /// Wrap the component's `PreferencesPage` in a `NavigationPage` ready
    /// for `push_subpage`.
    pub fn navigation_page(&self, title: &str) -> adw::NavigationPage {
        adw::NavigationPage::builder()
            .title(tr(title))
            .child(&self.page)
            .build()
    }
}

impl Default for ReviewViewComponent {
    fn default() -> Self {
        Self::new()
    }
}
