// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment progress view component.
//!
//! Provides the user interface for creating a new profile, including
//! identity entry, privacy notices, and real-time capture progress. The
//! component builds an [`adw::PreferencesPage`] (wrapped in an
//! [`adw::NavigationPage`] by the caller for `push_subpage`) for each phase.

use crate::components::scan_feedback;
use crate::tr;
use crate::view_model::EnrollmentViewV3;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::{EntryRowExt, PreferencesGroupExt, PreferencesPageExt};

pub struct EnrollmentViewComponent {
    page: adw::PreferencesPage,
    /// Groups currently on the page. A re-render removes the previous set
    /// before adding the new one.
    groups: Vec<adw::PreferencesGroup>,
}

impl EnrollmentViewComponent {
    pub fn new() -> Self {
        let page = adw::PreferencesPage::new();
        Self {
            page,
            groups: Vec::new(),
        }
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
        self.groups = vec![group];
    }

    fn render_capture_phase<F1, F2>(&mut self, view: &EnrollmentViewV3, on_stop: F1, on_cancel: F2)
    where
        F1: Fn() + 'static,
        F2: Fn() + 'static,
    {
        // Hero: the silhouette stand-in for the camera view, with the
        // guidance headline under it. `quality_bucket` and the typed
        // `GuidanceView` drive the styling — never a translated string.
        let hero_group = adw::PreferencesGroup::new();
        hero_group.update_property(&[gtk4::accessible::Property::Label(&tr("Live scan guidance"))]);

        // USA-9: once the scan has everything it needs, the headline stops
        // asking for more and says so — "keep going" would be wrong advice.
        let complete = view.diversity_percent >= 100 && view.quality_bucket >= 2;
        if complete {
            hero_group.add(&scan_feedback::hero_with(
                &tr("Enrollment complete"),
                true,
                Some(tr("Press Stop and review to save this profile.")),
                view.quality_bucket,
            ));
        } else {
            hero_group.add(&scan_feedback::hero(&view.guidance, view.quality_bucket));
        }

        // Breakdown: three separately measured signals plus the one the
        // daemon says it is waiting on, so a low reading is explainable.
        let signals_group = adw::PreferencesGroup::builder()
            .title(tr("Scan quality"))
            .description(tr("What the scan measures, and what it is waiting on."))
            .build();
        signals_group.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Scan quality signals",
        ))]);
        signals_group.add(&scan_feedback::image_quality_row(
            view.quality_percent,
            view.quality_bucket,
        ));
        signals_group.add(&scan_feedback::angle_variety_row(view.diversity_percent));
        signals_group.add(&scan_feedback::kept_rate_row(view.accept_rate_percent()));
        signals_group.add(&scan_feedback::attention_row(&view.guidance));

        // Counters: the three numbers on the wire, each labelled for what it
        // actually is. `frames_seen == accepted + rejected` holds, so the
        // three rows add up in front of the user.
        let counts_group = adw::PreferencesGroup::builder()
            .title(tr("Views this session"))
            .build();
        counts_group.update_property(&[gtk4::accessible::Property::Label(&tr("Capture counters"))]);
        counts_group.add(&scan_feedback::count_row(
            "Accepted",
            "Kept for your face model",
            view.frames_accepted,
            "Accepted views",
        ));
        counts_group.add(&scan_feedback::count_row(
            "Seen",
            "Views sent for checking",
            view.frames_seen,
            "Views seen",
        ));
        counts_group.add(&scan_feedback::count_row(
            "Rejected",
            "Not usable, so discarded",
            view.frames_rejected,
            "Rejected views",
        ));

        let actions_group = adw::PreferencesGroup::new();
        actions_group.update_property(&[gtk4::accessible::Property::Label(&tr("Capture actions"))]);

        let stop_btn = gtk4::Button::builder()
            .label(tr("_Stop and review"))
            .css_classes(["suggested-action", "pill"])
            .use_underline(true)
            .halign(gtk4::Align::Center)
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
            .css_classes(["flat"])
            .halign(gtk4::Align::Center)
            .tooltip_text(tr("Cancel enrollment and discard frames"))
            .build();
        cancel_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Cancel enrollment")),
            gtk4::accessible::Property::Description(&tr(
                "Discard the enrollment and return to the profile list",
            )),
        ]);
        cancel_btn.connect_clicked(move |_| on_cancel());

        let buttons = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(6)
            .halign(gtk4::Align::Center)
            .build();
        buttons.append(&stop_btn);
        buttons.append(&cancel_btn);
        actions_group.add(&buttons);

        self.page.add(&hero_group);
        self.page.add(&signals_group);
        self.page.add(&counts_group);
        self.page.add(&actions_group);
        self.groups = vec![hero_group, signals_group, counts_group, actions_group];
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

        let summary_group = view.map(|v| {
            let summary = adw::PreferencesGroup::builder()
                .title(tr("What the scan collected"))
                .build();
            summary.update_property(&[gtk4::accessible::Property::Label(&tr(
                "Scan quality signals",
            ))]);
            summary.add(&scan_feedback::image_quality_row(
                v.quality_percent,
                v.quality_bucket,
            ));
            summary.add(&scan_feedback::angle_variety_row(v.diversity_percent));
            summary.add(&scan_feedback::kept_rate_row(v.accept_rate_percent()));
            summary.add(&scan_feedback::count_row(
                "Accepted",
                "Kept for your face model",
                v.frames_accepted,
                "Accepted views",
            ));
            summary.add(&scan_feedback::count_row(
                "Seen",
                "Views sent for checking",
                v.frames_seen,
                "Views seen",
            ));
            summary.add(&scan_feedback::count_row(
                "Rejected",
                "Not usable, so discarded",
                v.frames_rejected,
                "Rejected views",
            ));
            summary
        });

        let review_btn = gtk4::Button::builder()
            .label(tr("_Review Enrollment"))
            .css_classes(["suggested-action", "pill"])
            .halign(gtk4::Align::Center)
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
            .css_classes(["flat"])
            .halign(gtk4::Align::Center)
            .tooltip_text(tr("Cancel enrollment and discard frames"))
            .build();
        cancel_btn.update_property(&[gtk4::accessible::Property::Label(&tr("Cancel enrollment"))]);
        cancel_btn.connect_clicked(move |_| on_cancel());

        let buttons = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(6)
            .halign(gtk4::Align::Center)
            .build();
        buttons.append(&review_btn);
        buttons.append(&cancel_btn);
        group.add(&buttons);

        self.page.add(&group);
        self.groups = vec![group];
        if let Some(summary) = summary_group {
            self.page.add(&summary);
            self.groups.push(summary);
        }

        &self.page
    }

    fn clear(&mut self) {
        for group in self.groups.drain(..) {
            self.page.remove(&group);
        }
    }

    /// Get a reference to the underlying `PreferencesPage`.
    pub fn page(&self) -> &adw::PreferencesPage {
        &self.page
    }

    /// Wrap the component's `PreferencesPage` in a `NavigationPage` ready
    /// for `push_subpage`. The `title` is shown in the navigation header.
    pub fn navigation_page(&self, title: &str) -> adw::NavigationPage {
        // The sub-page carries its own AdwHeaderBar so AdwNavigationView
        // renders a back button and the page title above the content.
        let toolbar = adw::ToolbarView::builder().content(&self.page).build();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        adw::NavigationPage::builder()
            .title(tr(title))
            .child(&toolbar)
            .build()
    }
}

impl Default for EnrollmentViewComponent {
    fn default() -> Self {
        Self::new()
    }
}
