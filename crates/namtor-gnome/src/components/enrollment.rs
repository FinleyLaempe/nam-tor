// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment progress view component.
//!
//! Provides the user interface for creating a new profile, including
//! identity entry, privacy notices, and real-time capture progress. The
//! component builds an [`adw::PreferencesPage`] (wrapped in an
//! [`adw::NavigationPage`] by the caller for `push_subpage`) for each phase.

use crate::tr;
use crate::view_model::EnrollmentViewV3;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::{EntryRowExt, PreferencesGroupExt, PreferencesPageExt};

pub struct EnrollmentViewComponent {
    page: adw::PreferencesPage,
    group: Option<adw::PreferencesGroup>,
}

impl EnrollmentViewComponent {
    pub fn new() -> Self {
        let page = adw::PreferencesPage::new();
        Self { page, group: None }
    }

    /// Renders the enrollment state into the component's `PreferencesPage`.
    ///
    /// If `view` is `None`, the component renders the setup phase (Name entry + Privacy).
    /// If `view` is `Some`, it renders the capture progress phase.
    pub fn render(
        &mut self,
        view: Option<&EnrollmentViewV3>,
        on_start: impl Fn(&str) + 'static,
        on_stop: impl Fn() + 'static,
        on_cancel: impl Fn() + 'static,
    ) -> &adw::PreferencesPage {
        self.clear();

        if let Some(enrollment) = view {
            self.render_capture_phase(enrollment, on_stop, on_cancel);
        } else {
            self.render_setup_phase(on_start, on_cancel);
        }

        &self.page
    }

    fn render_setup_phase<F>(&mut self, on_start: F, on_cancel: impl Fn() + 'static)
    where
        F: Fn(&str) + 'static,
    {
        let group = adw::PreferencesGroup::builder()
            .title(tr("Add New Profile"))
            .description(tr(
                "Your face model is encrypted and stored locally on this device. \
                 No face data leaves the system.",
            ))
            .build();
        group.update_property(&[gtk4::accessible::Property::Label(&tr(
            "New profile setup group",
        ))]);

        let name_row = adw::EntryRow::builder()
            .title(tr("_Profile Name"))
            .use_underline(true)
            .build();
        name_row.update_property(&[
            gtk4::accessible::Property::Label(&tr("Profile name")),
            gtk4::accessible::Property::Description(&tr(
                "Enter a descriptive name for this profile",
            )),
        ]);

        let start_btn = gtk4::Button::builder()
            .label(tr("_Start Enrollment"))
            .css_classes(["suggested-action"])
            .use_underline(true)
            .tooltip_text(tr("Begin the face enrollment capture process"))
            .build();
        start_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Start enrollment")),
            gtk4::accessible::Property::Description(&tr(
                "Begin the face enrollment capture process",
            )),
        ]);
        let name_row_for_start = name_row.clone();
        start_btn.connect_clicked(move |_| {
            let name = name_row_for_start.text().to_string();
            on_start(&name);
        });
        let cancel_btn = gtk4::Button::builder()
            .label(tr("Cancel"))
            .tooltip_text(tr("Cancel profile creation"))
            .build();
        cancel_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Cancel enrollment")),
            gtk4::accessible::Property::Description(&tr(
                "Discard the new profile and return to the list",
            )),
        ]);
        cancel_btn.connect_clicked(move |_| on_cancel());

        name_row.add_suffix(&start_btn);
        group.add(&name_row);
        group.add(&cancel_btn);

        self.page.add(&group);
        self.group = Some(group);
    }

    fn render_capture_phase<F1, F2>(&mut self, view: &EnrollmentViewV3, on_stop: F1, on_cancel: F2)
    where
        F1: Fn() + 'static,
        F2: Fn() + 'static,
    {
        let is_good_position = view.guidance.to_lowercase().contains("looking good");
        let face_icon = gtk4::Image::from_icon_name("avatar-default-symbolic");
        face_icon.set_pixel_size(128);
        if is_good_position {
            face_icon.set_css_classes(&["success"]);
        } else {
            face_icon.set_css_classes(&["dim-label"]);
        }
        face_icon.set_accessible_role(gtk4::AccessibleRole::Img);
        face_icon.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Face position outline — center your face in the frame",
        ))]);

        let icon_clamp = adw::Clamp::builder()
            .child(&face_icon)
            .halign(gtk4::Align::Center)
            .margin_top(24)
            .margin_bottom(24)
            .build();

        let guidance_label = gtk4::Label::builder().label(tr(&view.guidance)).build();
        if is_good_position {
            guidance_label.set_css_classes(&["heading", "success"]);
        } else {
            guidance_label.set_css_classes(&["body"]);
        }
        guidance_label.set_accessible_role(gtk4::AccessibleRole::Status);
        guidance_label.update_property(&[
            gtk4::accessible::Property::Label(&tr("Live capture guidance")),
            gtk4::accessible::Property::Description(&tr(
                "Real-time positioning feedback during enrollment",
            )),
        ]);

        let group = adw::PreferencesGroup::builder()
            .title(tr("Capturing enrollment"))
            .build();
        group.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Capture progress group",
        ))]);

        group.add(&icon_clamp);
        group.add(&guidance_label);

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

        let counts_row = adw::ActionRow::builder()
            .title(tr(&format!(
                "Accepted: {} / Seen: {}",
                view.frames_accepted, view.frames_seen
            )))
            .build();
        counts_row.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Frames accepted and seen counts",
        ))]);
        group.add(&counts_row);

        let bar = gtk4::ProgressBar::builder()
            .fraction(view.diversity_percent as f64 / 100.0)
            .build();
        bar.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Enrollment diversity progress",
        ))]);
        group.add(&bar);

        // USA-9: when diversity_percent >= 100 and quality_bucket is good
        // (>= 2), show a completion cue and style the Stop button as
        // suggested-action.
        let complete = view.diversity_percent >= 100 && view.quality_bucket >= 2;
        let stop_label = if complete {
            tr("Enrollment complete — press Stop to review")
        } else {
            tr("_Stop")
        };

        if complete {
            let cue_row = adw::ActionRow::builder()
                .title(tr("Enrollment complete — press Stop to review"))
                .css_classes(["success"])
                .build();
            cue_row.set_accessible_role(gtk4::AccessibleRole::Status);
            cue_row.update_property(&[gtk4::accessible::Property::Label(&tr(
                "Enrollment complete",
            ))]);
            group.add(&cue_row);
        }

        let stop_btn = gtk4::Button::builder()
            .label(&stop_label)
            .css_classes(["suggested-action"])
            .use_underline(!complete)
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

        group.add(&stop_btn);
        group.add(&cancel_btn);

        self.page.add(&group);
        self.group = Some(group);
    }

    /// Render the stopped state with a summary, optional error (USA-5),
    /// and Review + Cancel buttons.
    pub fn render_stopped(
        &mut self,
        view: Option<&EnrollmentViewV3>,
        error: Option<&crate::view_model::ErrorView>,
        on_review: impl Fn() + 'static,
        on_cancel: impl Fn() + 'static,
    ) -> &adw::PreferencesPage {
        self.clear();

        let group = adw::PreferencesGroup::builder()
            .title(tr("Enrollment stopped"))
            .build();
        group.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Enrollment stopped group",
        ))]);

        // USA-5: render the error at the top of the Stopped branch so
        // save errors are visible in the enrollment view.
        if let Some(err) = error {
            let err_row = adw::ActionRow::builder()
                .title(tr(&format!("{}: {}", err.title, err.detail)))
                .css_classes(["error"])
                .build();
            err_row.set_accessible_role(gtk4::AccessibleRole::Alert);
            group.add(&err_row);
        }

        if let Some(v) = view {
            let quality_row = adw::ActionRow::builder()
                .title(tr(&format!("Quality: {}%", v.quality_percent)))
                .build();
            group.add(&quality_row);

            let diversity_row = adw::ActionRow::builder()
                .title(tr(&format!("Diversity: {}%", v.diversity_percent)))
                .build();
            group.add(&diversity_row);

            let counts_row = adw::ActionRow::builder()
                .title(tr(&format!(
                    "Accepted: {} / Seen: {}",
                    v.frames_accepted, v.frames_seen
                )))
                .build();
            group.add(&counts_row);
        }

        let review_btn = gtk4::Button::builder()
            .label(tr("_Review Enrollment"))
            .css_classes(["suggested-action"])
            .use_underline(true)
            .tooltip_text(tr("Review the captured enrollment"))
            .build();
        review_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Review enrollment")),
            gtk4::accessible::Property::Description(&tr(
                "Review the captured frames before saving",
            )),
        ]);
        review_btn.connect_clicked(move |_| on_review());

        let cancel_btn = gtk4::Button::builder()
            .label(tr("Cancel"))
            .tooltip_text(tr("Cancel enrollment and discard frames"))
            .build();
        cancel_btn.update_property(&[gtk4::accessible::Property::Label(&tr("Cancel enrollment"))]);
        cancel_btn.connect_clicked(move |_| on_cancel());

        group.add(&review_btn);
        group.add(&cancel_btn);

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
    /// for `push_subpage`. The `title` is shown in the navigation header.
    pub fn navigation_page(&self, title: &str) -> adw::NavigationPage {
        adw::NavigationPage::builder()
            .title(tr(title))
            .child(&self.page)
            .build()
    }
}

impl Default for EnrollmentViewComponent {
    fn default() -> Self {
        Self::new()
    }
}
