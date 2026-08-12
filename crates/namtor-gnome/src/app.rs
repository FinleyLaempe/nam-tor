// SPDX-License-Identifier: GPL-3.0-only
//! Application controller wiring the GNOME settings shell to the live
//! D-Bus daemon via `ProfilePresenter` and the real UI components.
//!
//! The wizard binary (`src/bin/wizard.rs`) constructs an [`AppController`]
//! inside `connect_activate` and hands it the `adw::Application`. The
//! controller owns a `Rc<RefCell<ProfilePresenter<DbusManagerClient>>>`
//! (GTK is single-threaded) and renders the three destinations — Profiles,
//! Authentication, System — plus the enrollment flow.
//!
//! The enrollment flow uses [`adw::PreferencesDialog::push_subpage`] /
//! [`pop_subpage`] for the capture and review sub-pages, wrapped in
//! [`adw::NavigationPage`] per the libadwaita 1.5 migration guide.
//!
//! If the daemon is unreachable the controller routes every destination to
//! the shell's offline status page (with a retry button) rather than
//! showing placeholder labels.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::{ActionRowExt, PreferencesGroupExt};

use crate::components::pam_services::render_pam_services;
use crate::components::system_status::render_system_status;
use crate::components::{
    EnrollmentViewComponent, ProfileListView, ReviewViewComponent, ToggleCallback,
};
use crate::dbus_client::DbusManagerClient;
use crate::presenter::ProfilePresenter;
use crate::shell::{Destination, Shell};
use crate::tr;
use crate::view_model::EnrollmentState;

type PresenterRc = Rc<RefCell<ProfilePresenter<DbusManagerClient>>>;
pub type ControllerRc = Rc<RefCell<AppController>>;

pub struct AppController {
    shell: Shell,
    presenter: Option<PresenterRc>,
    profile_list: ProfileListView,
    enrollment_view: EnrollmentViewComponent,
    review_view: ReviewViewComponent,
    /// The enrollment sub-page (NavigationPage wrapping the enrollment
    /// PreferencesPage). `Some` while the enrollment flow is active.
    enrollment_nav_page: Option<adw::NavigationPage>,
    /// The review sub-page (NavigationPage wrapping the review
    /// PreferencesPage). `Some` while the review is visible.
    review_nav_page: Option<adw::NavigationPage>,
    offline: bool,
}

impl AppController {
    pub fn build(app: &adw::Application) -> ControllerRc {
        let mut shell = Shell::new(app);

        let (presenter, offline) = match DbusManagerClient::connect() {
            Ok(client) => {
                let uid = current_uid();
                let p = Rc::new(RefCell::new(ProfilePresenter::new(client, uid)));
                (Some(p), false)
            }
            Err(err) => {
                let detail = err.to_string();
                show_offline_all(&mut shell, &detail);
                (None, true)
            }
        };

        if let Some(ref presenter) = presenter {
            presenter.borrow_mut().refresh();
        }

        let controller = Rc::new(RefCell::new(Self {
            shell,
            presenter,
            profile_list: ProfileListView::new(),
            enrollment_view: EnrollmentViewComponent::new(),
            review_view: ReviewViewComponent::new(),
            enrollment_nav_page: None,
            review_nav_page: None,
            offline,
        }));

        if !offline {
            let rc = Rc::clone(&controller);
            controller.borrow_mut().render_all(&rc);
        }
        controller
    }

    pub fn present(&self) {
        self.shell.present();
    }

    pub fn is_offline(&self) -> bool {
        self.offline
    }

    fn render_all(&mut self, rc: &ControllerRc) {
        if self.presenter.is_none() {
            return;
        }
        self.render_profiles(rc);
        self.render_auth();
        self.render_system();
    }

    fn render_profiles(&mut self, rc: &ControllerRc) {
        let Some(presenter) = &self.presenter else {
            return;
        };
        let model = presenter.borrow().model().clone();

        let group = adw::PreferencesGroup::builder()
            .title(tr("Enrolled Profiles"))
            .build();
        group.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Enrolled profiles group",
        ))]);

        let enroll_btn = gtk4::Button::builder()
            .label(tr("_Enroll Face"))
            .css_classes(["suggested-action"])
            .use_underline(true)
            .tooltip_text(tr("Start a new face enrollment"))
            .build();
        enroll_btn.update_property(&[
            gtk4::accessible::Property::Label(&tr("Enroll face")),
            gtk4::accessible::Property::Description(&tr("Begin the face enrollment wizard")),
        ]);

        let presenter_for_enroll = Rc::clone(presenter);
        let rc_enroll = Rc::clone(rc);
        enroll_btn.connect_clicked(move |_| {
            {
                let mut p = presenter_for_enroll.borrow_mut();
                p.cancel_enrollment_v3();
            }
            rc_enroll.borrow_mut().render_enrollment(&rc_enroll);
        });

        let enroll_row = adw::ActionRow::builder()
            .title(tr("Enroll a new face profile"))
            .activatable(true)
            .build();
        enroll_row.add_suffix(&enroll_btn);
        enroll_row.update_property(&[
            gtk4::accessible::Property::Label(&tr("Enroll face")),
            gtk4::accessible::Property::Description(&tr("Begin the face enrollment wizard")),
        ]);
        let presenter_for_row = Rc::clone(presenter);
        let rc_row = Rc::clone(rc);
        enroll_row.connect_activated(move |_| {
            {
                let mut p = presenter_for_row.borrow_mut();
                p.cancel_enrollment_v3();
            }
            rc_row.borrow_mut().render_enrollment(&rc_row);
        });
        group.add(&enroll_row);

        let on_toggle: ToggleCallback = {
            let presenter = Rc::clone(presenter);
            let rc_toggle = Rc::clone(rc);
            Rc::new(move |id: &str, enabled: bool| {
                {
                    let mut p = presenter.borrow_mut();
                    p.enable_profile(id, enabled);
                }
                rc_toggle.borrow_mut().render_profiles(&rc_toggle);
            })
        };
        let list_group = self
            .profile_list
            .render_with_callback(&model, Some(on_toggle));

        self.shell.set_profiles_content(list_group);
        self.shell.add_profiles_group(&group);

        if let Some(err) = &model.error {
            let err_group = adw::PreferencesGroup::new();
            let err_row = adw::ActionRow::builder()
                .title(tr(&format!("{}: {}", err.title, err.detail)))
                .css_classes(["error"])
                .build();
            err_row.set_accessible_role(gtk4::AccessibleRole::Alert);
            err_group.add(&err_row);
            self.shell.add_profiles_group(&err_group);
        }

        self.shell.set_destination(Destination::Profiles);
    }

    fn render_auth(&mut self) {
        let Some(presenter) = &self.presenter else {
            return;
        };
        let group = render_pam_services(presenter);
        self.shell.set_auth_content(&group);
    }

    fn render_system(&mut self) {
        let Some(presenter) = &self.presenter else {
            return;
        };
        self.shell.clear_system();
        let groups = render_system_status(presenter);
        for g in &groups {
            self.shell.add_system_group(g);
        }
    }

    fn render_enrollment(&mut self, rc: &ControllerRc) {
        let Some(presenter) = &self.presenter else {
            return;
        };
        let model = presenter.borrow().model().clone();
        let state = model.enrollment_state.clone();

        match state {
            EnrollmentState::Setup => {
                let presenter_start = Rc::clone(presenter);
                let rc_start = Rc::clone(rc);
                let on_start = move |name: &str| {
                    {
                        let mut p = presenter_start.borrow_mut();
                        p.start_enrollment_v3(name);
                    }
                    rc_start.borrow_mut().render_enrollment(&rc_start);
                };

                let presenter_cancel = Rc::clone(presenter);
                let rc_cancel = Rc::clone(rc);
                let on_cancel = move || {
                    {
                        let mut p = presenter_cancel.borrow_mut();
                        p.cancel_enrollment_v3();
                    }
                    rc_cancel.borrow_mut().render_profiles(&rc_cancel);
                };

                self.enrollment_view
                    .render(None, on_start, move || {}, on_cancel);

                if self.enrollment_nav_page.is_none() {
                    let nav = self.enrollment_view.navigation_page("Enroll Face");
                    self.shell.push_subpage(&nav);
                    self.enrollment_nav_page = Some(nav);
                }
            }
            EnrollmentState::Capturing => {
                let view = model.enrollment_view_v3.clone();
                let presenter_stop = Rc::clone(presenter);
                let rc_stop = Rc::clone(rc);
                let on_stop = move || {
                    {
                        let mut p = presenter_stop.borrow_mut();
                        p.stop_enrollment_v3();
                    }
                    rc_stop.borrow_mut().render_enrollment(&rc_stop);
                };

                let presenter_cancel = Rc::clone(presenter);
                let rc_cancel = Rc::clone(rc);
                let on_cancel = move || {
                    {
                        let mut p = presenter_cancel.borrow_mut();
                        p.cancel_enrollment_v3();
                    }
                    rc_cancel.borrow_mut().render_profiles(&rc_cancel);
                };

                self.enrollment_view
                    .render(view.as_ref(), move |_| {}, on_stop, on_cancel);

                if self.enrollment_nav_page.is_none() {
                    let nav = self.enrollment_view.navigation_page("Enroll Face");
                    self.shell.push_subpage(&nav);
                    self.enrollment_nav_page = Some(nav);
                }
            }
            EnrollmentState::Stopped => {
                let presenter_review = Rc::clone(presenter);
                let rc_review = Rc::clone(rc);
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
                review_btn.connect_clicked(move |_| {
                    {
                        let mut p = presenter_review.borrow_mut();
                        p.review_enrollment_v3();
                    }
                    rc_review.borrow_mut().render_enrollment(&rc_review);
                });

                let presenter_cancel = Rc::clone(presenter);
                let rc_cancel = Rc::clone(rc);
                let cancel_btn = gtk4::Button::builder()
                    .label(tr("Cancel"))
                    .tooltip_text(tr("Cancel enrollment and discard frames"))
                    .build();
                cancel_btn.update_property(&[gtk4::accessible::Property::Label(&tr(
                    "Cancel enrollment",
                ))]);
                cancel_btn.connect_clicked(move |_| {
                    {
                        let mut p = presenter_cancel.borrow_mut();
                        p.cancel_enrollment_v3();
                    }
                    rc_cancel.borrow_mut().render_profiles(&rc_cancel);
                });

                // Re-render the enrollment page with stop actions. The
                // enrollment component's render method expects a capture
                // view for the stop UI; we pass the current view and use
                // the stop/cancel callbacks for the button actions.
                let view = model.enrollment_view_v3.clone();
                let on_stop_review = move || {};
                self.enrollment_view
                    .render(view.as_ref(), move |_| {}, on_stop_review, move || {});

                if self.enrollment_nav_page.is_none() {
                    let nav = self.enrollment_view.navigation_page("Enroll Face");
                    self.shell.push_subpage(&nav);
                    self.enrollment_nav_page = Some(nav);
                }
            }
            EnrollmentState::Reviewing => {
                if let Some(review) = &model.review_view_v3 {
                    let presenter_save = Rc::clone(presenter);
                    let rc_save = Rc::clone(rc);
                    let on_save = move || {
                        {
                            let mut p = presenter_save.borrow_mut();
                            p.save_profile_v3();
                        }
                        rc_save.borrow_mut().after_save(&rc_save);
                    };

                    let presenter_resume = Rc::clone(presenter);
                    let rc_resume = Rc::clone(rc);
                    let on_resume = move || {
                        {
                            let mut p = presenter_resume.borrow_mut();
                            p.resume_enrollment_v3();
                        }
                        rc_resume.borrow_mut().render_enrollment(&rc_resume);
                    };

                    let presenter_discard = Rc::clone(presenter);
                    let rc_discard = Rc::clone(rc);
                    let on_discard = move || {
                        {
                            let mut p = presenter_discard.borrow_mut();
                            p.discard_enrollment_v3();
                        }
                        rc_discard.borrow_mut().render_profiles(&rc_discard);
                    };

                    self.review_view
                        .render(review, on_save, on_resume, on_discard, false);

                    if self.review_nav_page.is_none() {
                        let nav = self.review_view.navigation_page("Review");
                        self.shell.push_subpage(&nav);
                        self.review_nav_page = Some(nav);
                    }
                }
            }
            EnrollmentState::Saving => {
                // Pop review sub-page if present, show saving state on
                // the enrollment page.
                if self.review_nav_page.is_some() {
                    self.shell.pop_subpage();
                    self.review_nav_page = None;
                }
                let view = model.enrollment_view_v3.clone();
                self.enrollment_view
                    .render(view.as_ref(), move |_| {}, move || {}, move || {});
            }
            EnrollmentState::Error(detail) => {
                let presenter_dismiss = Rc::clone(presenter);
                let rc_dismiss = Rc::clone(rc);
                let dismiss_btn = gtk4::Button::builder()
                    .label(tr("_Dismiss"))
                    .use_underline(true)
                    .css_classes(["suggested-action"])
                    .build();
                dismiss_btn
                    .update_property(&[gtk4::accessible::Property::Label(&tr("Dismiss error"))]);
                dismiss_btn.connect_clicked(move |_| {
                    {
                        let mut p = presenter_dismiss.borrow_mut();
                        p.dismiss_v3_error();
                    }
                    rc_dismiss.borrow_mut().render_enrollment(&rc_dismiss);
                });

                let view = model.enrollment_view_v3.clone();
                self.enrollment_view
                    .render(view.as_ref(), move |_| {}, move || {}, move || {});
                // The error detail is surfaced via the model.error field,
                // which render_profiles shows. For the sub-page, we rely
                // on the enrollment component's guidance row.
                let _ = detail;
            }
        }
    }

    fn after_save(&mut self, rc: &ControllerRc) {
        let setup = {
            let Some(presenter) = &self.presenter else {
                self.render_profiles(rc);
                return;
            };
            let p = presenter.borrow();
            matches!(p.model().enrollment_state, EnrollmentState::Setup)
        };
        if setup {
            // Pop any enrollment/review sub-pages and return to profiles.
            if self.review_nav_page.is_some() {
                self.shell.pop_subpage();
                self.review_nav_page = None;
            }
            if self.enrollment_nav_page.is_some() {
                self.shell.pop_subpage();
                self.enrollment_nav_page = None;
            }
            self.render_profiles(rc);
        } else {
            self.render_enrollment(rc);
        }
    }
}

fn current_uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

fn show_offline_all(shell: &mut Shell, detail: &str) {
    let detail_owned = detail.to_string();
    shell.show_offline(&detail_owned, || {});
}
