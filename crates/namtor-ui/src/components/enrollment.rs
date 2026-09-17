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
use libadwaita::prelude::{ActionRowExt, EntryRowExt, PreferencesGroupExt, PreferencesPageExt};

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
    /// `announce` gates the screen-reader announcement of the guidance
    /// headline; see `ProgressAnnouncer`. The headline always renders.
    pub fn render(
        &mut self,
        view: Option<&EnrollmentViewV3>,
        announce: bool,
        on_start: impl Fn(&str) + 'static,
        on_stop: impl Fn() + 'static,
        on_cancel: impl Fn() + 'static,
    ) -> &adw::PreferencesPage {
        self.clear();

        if let Some(enrollment) = view {
            self.render_capture_phase(enrollment, announce, on_stop, on_cancel);
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

    fn render_capture_phase<F1, F2>(
        &mut self,
        view: &EnrollmentViewV3,
        announce: bool,
        on_stop: F1,
        on_cancel: F2,
    ) where
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
                announce,
            ));
        } else {
            hero_group.add(&scan_feedback::hero(
                &view.guidance,
                view.quality_bucket,
                announce,
            ));
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
        // `_opt`: before the first frame there is no measurement, and these
        // render an em dash rather than the seeded zeros.
        signals_group.add(&scan_feedback::image_quality_row_opt(
            view.measured_quality(),
        ));
        signals_group.add(&scan_feedback::angle_variety_row_opt(
            view.measured_diversity_percent(),
        ));
        signals_group.add(&scan_feedback::kept_rate_row(view.accept_rate_percent()));
        signals_group.add(&scan_feedback::attention_row(&view.guidance, announce));

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

    /// Render the in-progress save: a spinner and a line saying what is
    /// happening, with no controls.
    ///
    /// `EnrollmentState::Saving` was unreachable until the commit moved off
    /// the GTK main thread — the old `save_profile_v3` set the state and
    /// overwrote it inside one blocking call — so this arm rendered the
    /// capture page with every callback stubbed out and the window simply
    /// froze. There are deliberately no buttons: a commit in flight cannot be
    /// cancelled, and offering a control that does nothing is what the Error
    /// state used to do.
    pub fn render_saving(&mut self) -> &adw::PreferencesPage {
        self.clear();

        let group = adw::PreferencesGroup::new();
        group.update_property(&[gtk4::accessible::Property::Label(&tr("Saving profile"))]);

        let column = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(18)
            .halign(gtk4::Align::Center)
            .margin_top(24)
            .margin_bottom(24)
            .build();

        let spinner = gtk4::Spinner::builder()
            .spinning(true)
            .width_request(32)
            .height_request(32)
            .halign(gtk4::Align::Center)
            .build();
        column.append(&spinner);

        let headline = gtk4::Label::builder()
            .label(tr("Saving your face model…"))
            .css_classes(["title-2"])
            .halign(gtk4::Align::Center)
            .build();
        headline.set_accessible_role(gtk4::AccessibleRole::Status);
        headline.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Saving your face model",
        ))]);
        column.append(&headline);

        let detail = gtk4::Label::builder()
            .label(tr("This takes a moment. Do not close the window."))
            .css_classes(["dim-label"])
            .wrap(true)
            .justify(gtk4::Justification::Center)
            .max_width_chars(36)
            .halign(gtk4::Align::Center)
            .build();
        column.append(&detail);

        group.add(&column);
        self.page.add(&group);
        self.groups = vec![group];
        &self.page
    }

    /// Render a failed enrollment: what went wrong, what to do about it, and
    /// two buttons that work.
    ///
    /// The `Error` arm used to call `render` with `move |_| {}` for all three
    /// callbacks and throw the detail away with `let _ = detail;`. It built a
    /// Dismiss button, wired it, and never added it to a container, so the
    /// button was dropped at the end of the scope. The user was left on the
    /// capture page with Stop and Cancel doing nothing and no statement of
    /// what had failed — the destination for a missing camera, a polkit
    /// denial, a busy session and a review timeout alike.
    pub fn render_error(
        &mut self,
        detail: &str,
        error: Option<&crate::view_model::ErrorView>,
        on_dismiss: impl Fn() + 'static,
        on_cancel: impl Fn() + 'static,
    ) -> &adw::PreferencesPage {
        self.clear();

        let group = adw::PreferencesGroup::builder()
            .title(tr("Enrollment failed"))
            .build();
        group.update_property(&[gtk4::accessible::Property::Label(&tr("Enrollment failed"))]);

        // Prefer the structured error; fall back to the state's own detail so
        // there is always something to read.
        let (title, body) = match error {
            Some(e) => (e.title.clone(), e.detail.clone()),
            None => (tr("Enrollment failed"), detail.to_string()),
        };
        let err_row = adw::ActionRow::builder()
            .title(&title)
            .subtitle(&body)
            .css_classes(["error"])
            .build();
        err_row.set_subtitle_lines(0);
        err_row.set_accessible_role(gtk4::AccessibleRole::Alert);
        err_row.update_property(&[gtk4::accessible::Property::Label(&format!(
            "{title}. {body}"
        ))]);
        group.add(&err_row);

        // The recovery hint the presenter attached, when there is one.
        let recovery_group = error.and_then(|e| e.recovery.as_ref()).map(|hint| {
            let g = adw::PreferencesGroup::builder()
                .title(tr(&hint.title))
                .build();
            g.update_property(&[gtk4::accessible::Property::Label(&tr("How to fix this"))]);
            for (i, step) in hint.steps.iter().enumerate() {
                let row = adw::ActionRow::builder()
                    .title(crate::tr_args("{}. {}", &[&(i + 1).to_string(), step]))
                    .build();
                row.set_title_lines(0);
                g.add(&row);
            }
            g
        });

        let actions = adw::PreferencesGroup::new();
        let dismiss_btn = gtk4::Button::builder()
            .label(tr("_Try Again"))
            .use_underline(true)
            .css_classes(["suggested-action", "pill"])
            .halign(gtk4::Align::Center)
            .build();
        dismiss_btn.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Dismiss the error and try again",
        ))]);
        dismiss_btn.connect_clicked(move |_| on_dismiss());

        let cancel_btn = gtk4::Button::builder()
            .label(tr("_Cancel"))
            .use_underline(true)
            .css_classes(["pill"])
            .halign(gtk4::Align::Center)
            .build();
        cancel_btn.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Cancel enrollment and go back",
        ))]);
        cancel_btn.connect_clicked(move |_| on_cancel());

        let button_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(12)
            .halign(gtk4::Align::Center)
            .build();
        button_box.append(&dismiss_btn);
        button_box.append(&cancel_btn);
        actions.add(&button_box);

        self.page.add(&group);
        let mut groups = vec![group];
        if let Some(g) = recovery_group {
            self.page.add(&g);
            groups.push(g);
        }
        self.page.add(&actions);
        groups.push(actions);
        self.groups = groups;

        &self.page
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
                .title(crate::tr_args("{}: {}", &[&err.title, &err.detail]))
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
            summary.add(&scan_feedback::image_quality_row_opt(v.measured_quality()));
            summary.add(&scan_feedback::angle_variety_row_opt(
                v.measured_diversity_percent(),
            ));
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
