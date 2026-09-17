// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment review view component.
//!
//! Displays a summary of the capture quality and coverage before the user
//! commits the profile to the store. No biometric data or raw scores are
//! exposed. Builds an [`adw::PreferencesPage`] (wrapped in a
//! [`adw::NavigationPage`] by the caller for `push_subpage`).

use crate::components::scan_feedback;
use crate::tr;
use crate::view_model::ReviewViewV3;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::{MessageDialogExt, PreferencesGroupExt, PreferencesPageExt};

pub struct ReviewViewComponent {
    page: adw::PreferencesPage,
    /// Groups currently on the page. A re-render removes the previous set
    /// before adding the new one.
    groups: Vec<adw::PreferencesGroup>,
}

impl ReviewViewComponent {
    pub fn new() -> Self {
        let page = adw::PreferencesPage::new();
        Self {
            page,
            groups: Vec::new(),
        }
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

        // Hero: the review screen answers "can I save this?", so the
        // headline is the verdict and the daemon's guidance is the detail
        // under it. The guidance used to be dropped here entirely.
        // Derive the quality verdict from the quality number, using the same
        // boundaries the daemon uses.
        //
        // This was `if view.ready_to_save { 2 } else { 1 }`. EnrollmentReviewV3
        // carries no bucket, so the screen invented one from readiness — a
        // different measurement entirely. A barely-usable scan that had
        // collected enough angles announced "12% — Good"; a sharp scan short
        // on angles announced "88% — Fair". The number and the word beside it
        // told different stories, and because the invented bucket was never 0
        // the review silhouette could not show the "Poor" tint at all.
        let verdict_bucket = crate::view_model::quality_bucket_from_percent(view.quality_percent);
        let advice = match view.guidance.hint() {
            Some(hint) => format!("{} — {}", view.guidance.label(), hint),
            None => view.guidance.label(),
        };
        let headline = if view.ready_to_save {
            tr("Ready to save")
        } else {
            tr("Not ready to save yet")
        };

        let hero_group = adw::PreferencesGroup::new();
        hero_group.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Enrollment review group",
        ))]);
        hero_group.add(&scan_feedback::hero_with(
            &headline,
            view.ready_to_save,
            Some(advice),
            verdict_bucket,
            // The review screen is rendered once on arrival, not polled, so
            // its headline always announces.
            true,
        ));

        let signals_group = adw::PreferencesGroup::builder()
            .title(tr("What the scan collected"))
            .build();
        signals_group.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Scan quality signals",
        ))]);
        signals_group.add(&scan_feedback::image_quality_row(
            view.quality_percent,
            verdict_bucket,
        ));

        signals_group.add(&scan_feedback::angle_variety_row(view.diversity_percent));
        signals_group.add(&scan_feedback::attention_row(&view.guidance, true, true));
        signals_group.add(&scan_feedback::count_row(
            "Accepted",
            "Kept for your face model",
            view.frames_accepted,
            "Accepted views",
        ));

        let group = adw::PreferencesGroup::new();
        group.update_property(&[gtk4::accessible::Property::Label(&tr("Review actions"))]);

        let save_btn = gtk4::Button::builder()
            .label(tr("_Save Profile"))
            .css_classes(["pill"])
            .halign(gtk4::Align::Center)
            .sensitive(!is_pending)
            .use_underline(true)
            .tooltip_text(tr("Save the enrolled profile"))
            .build();
        // Only the action that is actually right gets the accent: saving a
        // scan the daemon calls unfinished should not look like the default.
        if view.ready_to_save {
            save_btn.add_css_class("suggested-action");
        }
        save_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Save profile")),
            gtk4::accessible::Property::Description(&tr("Commit the captured profile to storage")),
        ]);
        // Saving a scan the daemon calls unfinished stays possible — it is a
        // deliberate escape hatch — but it must not be silent. Nothing told
        // the user the resulting profile may simply fail to recognise them,
        // which is the outcome they would otherwise discover at the lock
        // screen.
        if view.ready_to_save {
            save_btn.connect_clicked(move |_| on_save());
        } else {
            let on_save = std::rc::Rc::new(on_save);
            save_btn.connect_clicked(move |btn| {
                let on_save = std::rc::Rc::clone(&on_save);
                let dialog = adw::MessageDialog::builder()
                    .heading(tr("Save an unfinished scan?"))
                    .body(tr(
                        "This scan did not collect enough for the daemon to call it \
                         ready. Saving it now may produce a profile that does not \
                         recognise you, and you would find out at the lock screen. \
                         Resuming the capture takes a few more seconds.",
                    ))
                    .build();
                if let Some(win) = btn.root().and_downcast_ref::<adw::ApplicationWindow>() {
                    dialog.set_transient_for(Some(win));
                }
                dialog.set_accessible_role(gtk4::AccessibleRole::Dialog);
                dialog.add_response("resume", &tr("_Keep Capturing"));
                dialog.add_response("save", &tr("_Save Anyway"));
                dialog.set_response_appearance("save", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("resume"));
                dialog.set_close_response("resume");
                dialog.connect_response(None, move |dlg, response| {
                    if response == "save" {
                        on_save();
                    }
                    dlg.close();
                });
                dialog.present();
            });
        }

        let resume_btn = gtk4::Button::builder()
            .label(tr("_Resume Capture"))
            .css_classes(["pill"])
            .halign(gtk4::Align::Center)
            .sensitive(!is_pending)
            .use_underline(true)
            .tooltip_text(tr("Resume capture to improve quality"))
            .build();
        if !view.ready_to_save {
            resume_btn.add_css_class("suggested-action");
        }
        resume_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Resume capture")),
            gtk4::accessible::Property::Description(&tr("Return to capturing to add more frames")),
        ]);
        resume_btn.connect_clicked(move |_| on_resume());

        let discard_btn = gtk4::Button::builder()
            .label(tr("_Discard"))
            .sensitive(!is_pending)
            .use_underline(true)
            .halign(gtk4::Align::Center)
            .css_classes(["destructive-action", "flat"])
            .tooltip_text(tr("Discard the captured profile"))
            .build();
        discard_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Discard profile")),
            gtk4::accessible::Property::Description(&tr(
                "Throw away the captured profile without saving",
            )),
        ]);
        discard_btn.connect_clicked(move |_| on_discard());

        let buttons = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(6)
            .halign(gtk4::Align::Center)
            .build();
        if view.ready_to_save {
            buttons.append(&save_btn);
            buttons.append(&resume_btn);
        } else {
            buttons.append(&resume_btn);
            buttons.append(&save_btn);
        }
        buttons.append(&discard_btn);
        group.add(&buttons);

        self.page.add(&hero_group);
        self.page.add(&signals_group);
        self.page.add(&group);
        self.groups = vec![hero_group, signals_group, group];

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
    /// for `push_subpage`.
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

impl Default for ReviewViewComponent {
    fn default() -> Self {
        Self::new()
    }
}
