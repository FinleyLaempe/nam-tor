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
//! If the daemon is unreachable the controller routes every destination to
//! the shell's offline status page (with a retry button) rather than
//! showing placeholder labels.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, Button, Label, Orientation};
use libadwaita as adw;

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
    status_label: Label,
    offline: bool,
}

impl AppController {
    pub fn build(app: &adw::Application) -> ControllerRc {
        let shell = Shell::new(app);

        let (presenter, offline) = match DbusManagerClient::connect() {
            Ok(client) => {
                let uid = current_uid();
                let p = Rc::new(RefCell::new(ProfilePresenter::new(client, uid)));
                (Some(p), false)
            }
            Err(err) => {
                let detail = err.to_string();
                show_offline_all(&shell, &detail);
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
            status_label: Label::builder()
                .halign(Align::Start)
                .css_classes(["dim-label", "caption"])
                .wrap(true)
                .build(),
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

        let container = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(12)
            .build();

        let header = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(12)
            .build();

        let title = Label::builder()
            .label(tr("Enrolled Profiles"))
            .css_classes(["heading"])
            .halign(Align::Start)
            .hexpand(true)
            .build();
        title.set_accessible_role(gtk4::AccessibleRole::Heading);
        title.update_property(&[gtk4::accessible::Property::Level(1)]);
        header.append(&title);

        let enroll_btn = Button::builder()
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
            let mut p = presenter_for_enroll.borrow_mut();
            p.cancel_enrollment_v3();
            drop(p);
            rc_enroll.borrow_mut().render_enrollment(&rc_enroll);
        });
        header.append(&enroll_btn);
        container.append(&header);

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
        let list_widget = self
            .profile_list
            .render_with_callback(&model, Some(on_toggle));
        container.append(list_widget);

        if let Some(msg) = &model.message {
            self.status_label.set_text(&tr(msg));
            self.status_label
                .update_property(&[gtk4::accessible::Property::Label(&tr("Status message"))]);
            container.append(&self.status_label);
        }

        if let Some(err) = &model.error {
            let err_label = Label::builder()
                .label(tr(&format!("{}: {}", err.title, err.detail)))
                .css_classes(["error"])
                .wrap(true)
                .halign(Align::Start)
                .build();
            err_label.set_accessible_role(gtk4::AccessibleRole::Alert);
            container.append(&err_label);
        }

        self.shell.set_profiles_content(&container);
    }

    fn render_auth(&self) {
        let Some(presenter) = &self.presenter else {
            return;
        };
        let container = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(12)
            .build();
        render_pam_services(&container, presenter);
        self.shell.set_auth_content(&container);
    }

    fn render_system(&self) {
        let Some(presenter) = &self.presenter else {
            return;
        };
        let container = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(12)
            .build();
        render_system_status(&container, presenter);
        self.shell.set_system_content(&container);
    }

    fn render_enrollment(&mut self, rc: &ControllerRc) {
        let Some(presenter) = &self.presenter else {
            return;
        };
        let model = presenter.borrow().model().clone();
        let state = model.enrollment_state.clone();

        let container = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(12)
            .build();

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

                let widget = self
                    .enrollment_view
                    .render(None, on_start, move || {}, on_cancel);
                container.append(widget);
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

                let widget =
                    self.enrollment_view
                        .render(view.as_ref(), move |_| {}, on_stop, on_cancel);
                container.append(widget);
            }
            EnrollmentState::Stopped => {
                let presenter_review = Rc::clone(presenter);
                let rc_review = Rc::clone(rc);
                let review_btn = Button::builder()
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
                container.append(&review_btn);

                let presenter_cancel = Rc::clone(presenter);
                let rc_cancel = Rc::clone(rc);
                let cancel_btn = Button::builder()
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
                container.append(&cancel_btn);
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

                    let widget = self.review_view.render(
                        review,
                        on_save,
                        on_resume,
                        on_discard,
                        state == EnrollmentState::Saving,
                    );
                    container.append(widget);
                } else {
                    let fallback = Label::builder()
                        .label(tr("Review unavailable. Press back to return."))
                        .wrap(true)
                        .halign(Align::Start)
                        .build();
                    container.append(&fallback);
                }
            }
            EnrollmentState::Saving => {
                let saving = Label::builder()
                    .label(tr("Saving profile…"))
                    .css_classes(["heading"])
                    .halign(Align::Center)
                    .build();
                saving.set_accessible_role(gtk4::AccessibleRole::Status);
                container.append(&saving);
            }
            EnrollmentState::Error(detail) => {
                let err_label = Label::builder()
                    .label(tr(&format!("Enrollment error: {detail}")))
                    .css_classes(["error"])
                    .wrap(true)
                    .halign(Align::Start)
                    .build();
                err_label.set_accessible_role(gtk4::AccessibleRole::Alert);
                container.append(&err_label);

                let presenter_dismiss = Rc::clone(presenter);
                let rc_dismiss = Rc::clone(rc);
                let dismiss_btn = Button::builder()
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
                container.append(&dismiss_btn);
            }
        }

        let back_btn = Button::builder()
            .label(tr("_Back to Profiles"))
            .use_underline(true)
            .halign(Align::Start)
            .margin_top(12)
            .build();
        back_btn.update_property(&[gtk4::accessible::Property::Label(&tr("Back to profiles"))]);
        let presenter_back = Rc::clone(presenter);
        let rc_back = Rc::clone(rc);
        back_btn.connect_clicked(move |_| {
            {
                let mut p = presenter_back.borrow_mut();
                if !matches!(p.model().enrollment_state, EnrollmentState::Saving) {
                    p.cancel_enrollment_v3();
                }
            }
            rc_back.borrow_mut().render_profiles(&rc_back);
        });
        container.append(&back_btn);

        self.shell.set_profiles_content(&container);
        self.shell.set_destination(Destination::Profiles);
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
            self.render_profiles(rc);
        } else {
            self.render_enrollment(rc);
        }
    }
}

fn current_uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

fn show_offline_all(shell: &Shell, detail: &str) {
    let detail_owned = detail.to_string();
    shell.show_offline(&detail_owned, || {});
}
