// SPDX-License-Identifier: GPL-3.0-only
//! Application controller wiring the settings shell to the live
//! D-Bus daemon via `ProfilePresenter` and the real UI components.
//!
//! The wizard binary (`src/bin/wizard.rs`) constructs an [`AppController`]
//! inside `connect_activate` and hands it the `adw::Application`. The
//! controller owns a `Rc<RefCell<ProfilePresenter<DbusManagerClient>>>`
//! (GTK is single-threaded) and renders the three destinations — Profiles,
//! Authentication, System — plus the enrollment flow.
//!
//! The enrollment flow uses [`Shell::push_subpage`] / [`Shell::pop_subpage`]
//! for the capture and review sub-pages, wrapped in
//! [`adw::NavigationPage`] per the libadwaita 1.5 migration guide.
//!
//! If the daemon is unreachable the controller routes every destination to
//! the shell's offline status page (with a retry button) rather than
//! showing placeholder labels.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use glib::ControlFlow;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::{ActionRowExt, MessageDialogExt, PreferencesGroupExt};

use crate::components::pam_services::render_pam_services;
use crate::components::system_status::render_system_status;
use crate::components::{
    EnrollmentViewComponent, ProfileListView, RenameCallback, ReviewViewComponent, ToggleCallback,
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
    /// The 500 ms progress poll, while one is running.
    ///
    /// Holding the id is what stops the timer multiplying. The poll used to
    /// be registered at the tail of the `Capturing` arm of `render_enrollment`
    /// with its id discarded, and its own callback calls `render_enrollment`,
    /// which re-enters that arm and registers another — while the original
    /// returns `ControlFlow::Continue` and survives. The count doubled every
    /// tick that produced progress: 2 after half a second, 1024 after five,
    /// each one issuing its own blocking D-Bus round trip on the GTK main
    /// thread. The scan page froze the app within seconds of a real scan.
    poll_source: Option<glib::SourceId>,
    offline: bool,
}

impl AppController {
    pub fn build(app: &adw::Application) -> ControllerRc {
        let mut shell = Shell::new(app);

        let (presenter, offline_detail) = match DbusManagerClient::connect() {
            Ok(client) => {
                let uid = current_uid();
                let p = Rc::new(RefCell::new(ProfilePresenter::new(client, uid)));
                (Some(p), None)
            }
            Err(err) => {
                let detail = err.to_string();
                shell.show_offline(&detail, || {});
                (None, Some(detail))
            }
        };
        let offline = offline_detail.is_some();

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
            poll_source: None,
            offline,
        }));

        // USA-2: wire the offline retry button to retry_connection. The
        // connect error detail is re-rendered so the user keeps seeing why
        // the daemon could not be reached.
        if let Some(detail) = offline_detail {
            let rc = Rc::clone(&controller);
            controller
                .borrow_mut()
                .shell
                .show_offline(&detail, move || {
                    rc.borrow_mut().retry_connection(&rc);
                });
        } else {
            let rc = Rc::clone(&controller);
            controller.borrow_mut().render_all(&rc);
        }

        // A user-driven back — the header-bar arrow, Escape, or the swipe-back
        // gesture AdwNavigationView provides by default — pops the page
        // without telling us. Nothing observed that, so the controller kept
        // `enrollment_nav_page: Some(_)` for a page no longer on the stack:
        // pressing "Enroll Face" again rendered into a detached page and
        // appeared to do nothing for the rest of the process lifetime, the
        // daemon session stayed open with the camera and IR emitter running,
        // and the progress poll kept firing because the state was still
        // Capturing.
        {
            let rc = Rc::clone(&controller);
            let nav = controller.borrow().shell.navigation_view().clone();
            nav.connect_popped(move |_, page| {
                let mut c = rc.borrow_mut();
                c.on_subpage_popped(page);
                drop(c);
                let rc2 = Rc::clone(&rc);
                rc2.borrow_mut().render_profiles(&rc2);
            });
        }

        controller
    }

    /// Reconcile our state with a page the user popped themselves.
    ///
    /// Cancels the enrollment session behind the page so the camera and the IR
    /// emitter stop, stops the progress poll, and forgets the page so the next
    /// "Enroll Face" pushes a fresh one.
    fn on_subpage_popped(&mut self, page: &adw::NavigationPage) {
        let was_enrollment = self.enrollment_nav_page.as_ref().is_some_and(|p| p == page);
        let was_review = self.review_nav_page.as_ref().is_some_and(|p| p == page);
        if !was_enrollment && !was_review {
            return;
        }

        self.stop_polling();
        if let Some(presenter) = &self.presenter {
            let mut p = presenter.borrow_mut();
            if !matches!(p.model().enrollment_state, EnrollmentState::Setup) {
                p.cancel_enrollment_v3();
            }
        }
        if was_enrollment {
            self.enrollment_nav_page = None;
        }
        if was_review {
            self.review_nav_page = None;
        }
    }

    /// Cancel the progress poll if one is running.
    fn stop_polling(&mut self) {
        if let Some(id) = self.poll_source.take() {
            id.remove();
        }
    }

    /// USA-2: re-run `DbusManagerClient::connect()` and rebuild the UI.
    /// Wired to the offline retry button.
    pub fn retry_connection(&mut self, rc: &ControllerRc) {
        self.apply_connect_result(rc, DbusManagerClient::connect());
    }

    /// Handle a connect attempt's result: on success, swap to the online
    /// presenter and re-render; on failure, show the offline page with the
    /// error detail and re-arm retry. Extracted from `retry_connection` so
    /// tests can drive both outcomes without a real D-Bus connection.
    ///
    /// **This is primarily for testing** — production code should call
    /// `retry_connection()` which wraps this method.
    pub fn apply_connect_result(
        &mut self,
        rc: &ControllerRc,
        result: namtor_client::types::ManagerResult<DbusManagerClient>,
    ) {
        match result {
            Ok(client) => {
                let uid = current_uid();
                let p = Rc::new(RefCell::new(ProfilePresenter::new(client, uid)));
                self.presenter = Some(p);
                self.offline = false;
                if let Some(presenter) = self.presenter.as_ref() {
                    presenter.borrow_mut().refresh();
                }
                self.render_all(rc);
            }
            Err(err) => {
                let detail = err.to_string();
                let rc_retry = Rc::clone(rc);
                self.shell.show_offline(&detail, move || {
                    rc_retry.borrow_mut().retry_connection(&rc_retry);
                });
            }
        }
    }

    /// Test-only constructor for behavioural tests. Constructs an
    /// `AppController` in offline state with no presenter, so tests
    /// can drive `apply_connect_result` with fake D-Bus clients.
    ///
    /// **This is for tests only** — production code must use `build()`.
    pub fn new_offline_for_test(app: &adw::Application) -> ControllerRc {
        let shell = Shell::new(app);
        Rc::new(RefCell::new(Self {
            shell,
            presenter: None,
            profile_list: ProfileListView::new(),
            enrollment_view: EnrollmentViewComponent::new(),
            review_view: ReviewViewComponent::new(),
            enrollment_nav_page: None,
            review_nav_page: None,
            poll_source: None,
            offline: true,
        }))
    }

    /// Test-only accessor for behavioural tests.
    ///
    /// **This is for tests only** — not part of the production API.
    pub fn is_offline_for_test(&self) -> bool {
        self.offline
    }

    /// Test-only accessor for behavioural tests.
    ///
    /// **This is for tests only** — not part of the production API.
    pub fn has_presenter_for_test(&self) -> bool {
        self.presenter.is_some()
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
        let on_rename: RenameCallback = {
            let presenter = Rc::clone(presenter);
            let rc_rename = Rc::clone(rc);
            Rc::new(move |id: &str, current_name: &str| {
                // USA-10: open a rename dialog with an EntryRow pre-filled
                show_rename_dialog(&presenter, &rc_rename, id, current_name);
            })
        };
        let list_group =
            self.profile_list
                .render_with_callback(&model, Some(on_toggle), Some(on_rename));

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
                    let pending = {
                        let mut p = presenter_start.borrow_mut();
                        p.start_enrollment_v3(name);
                        p.model().pending_replace.clone()
                    };
                    if let Some(pending) = pending {
                        // USA-3: duplicate name detected. Show a
                        // confirmation dialog before replacing.
                        show_replace_dialog(&presenter_start, &rc_start, &pending.old_name);
                    } else {
                        rc_start.borrow_mut().render_enrollment(&rc_start);
                    }
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

                // At most one poll. See the `poll_source` field.
                if self.poll_source.is_none() {
                    let presenter_poll = Rc::clone(presenter);
                    let rc_poll = Rc::clone(rc);
                    let id = glib::timeout_add_local(Duration::from_millis(500), move || {
                        let still_capturing = {
                            let p = presenter_poll.borrow();
                            matches!(p.model().enrollment_state, EnrollmentState::Capturing)
                        };
                        if !still_capturing {
                            // The id is consumed by returning Break, so clear
                            // it here or the next scan would see a stale
                            // `Some` and never start a poll at all.
                            rc_poll.borrow_mut().poll_source = None;
                            return ControlFlow::Break;
                        }
                        let has_progress = {
                            let mut p = presenter_poll.borrow_mut();
                            p.poll_progress_v3()
                        };
                        if has_progress {
                            rc_poll.borrow_mut().render_enrollment(&rc_poll);
                        }
                        ControlFlow::Continue
                    });
                    self.poll_source = Some(id);
                }
            }
            EnrollmentState::Stopped => {
                let presenter_review = Rc::clone(presenter);
                let rc_review = Rc::clone(rc);
                let on_review = move || {
                    {
                        let mut p = presenter_review.borrow_mut();
                        p.review_enrollment_v3();
                    }
                    rc_review.borrow_mut().render_enrollment(&rc_review);
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

                // USA-5: pass the model error so it renders at the top
                // of the stopped view.
                let view = model.enrollment_view_v3.clone();
                let error = model.error.as_ref();
                self.enrollment_view
                    .render_stopped(view.as_ref(), error, on_review, on_cancel);

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
                // A failed scan must say what failed and offer a way out. The
                // poll stops here too: nothing is capturing any more, and a
                // live timer would keep re-rendering this page. Taken by hand
                // rather than through `stop_polling`, because `presenter` is
                // an outstanding immutable borrow of `self`.
                if let Some(id) = self.poll_source.take() {
                    id.remove();
                }

                let presenter_dismiss = Rc::clone(presenter);
                let rc_dismiss = Rc::clone(rc);
                let on_dismiss = move || {
                    {
                        let mut p = presenter_dismiss.borrow_mut();
                        p.dismiss_v3_error();
                    }
                    rc_dismiss.borrow_mut().render_enrollment(&rc_dismiss);
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

                self.enrollment_view.render_error(
                    &detail,
                    model.error.as_ref(),
                    on_dismiss,
                    on_cancel,
                );

                if self.enrollment_nav_page.is_none() {
                    let nav = self.enrollment_view.navigation_page("Enroll Face");
                    self.shell.push_subpage(&nav);
                    self.enrollment_nav_page = Some(nav);
                }
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

fn show_replace_dialog(presenter: &PresenterRc, rc: &ControllerRc, name: &str) {
    let dialog = adw::MessageDialog::builder()
        .heading(format!("Replace profile '{name}'?"))
        .body(format!(
            "A profile named '{name}' already exists. Replace it with a new enrollment?"
        ))
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("replace", "Replace");
    dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);

    let presenter_replace = Rc::clone(presenter);
    let rc_replace = Rc::clone(rc);
    dialog.connect_response(None, move |dlg, response| {
        let mut p = presenter_replace.borrow_mut();
        if response == "replace" {
            p.confirm_replace_v3();
            drop(p);
            rc_replace.borrow_mut().render_enrollment(&rc_replace);
        } else {
            p.cancel_replace();
            drop(p);
            rc_replace.borrow_mut().render_enrollment(&rc_replace);
        }
        dlg.close();
    });
    dialog.present();
}

/// USA-10: Shows a rename dialog with an AdwEntryRow pre-filled with the
/// current profile name. On confirm, validates non-empty and calls
/// presenter.rename_profile(). On cancel, closes with no mutation.
/// Empty names show an inline error and do not close the dialog.
fn show_rename_dialog(
    presenter: &PresenterRc,
    rc: &ControllerRc,
    profile_id: &str,
    current_name: &str,
) {
    let dialog = adw::MessageDialog::builder()
        .heading(tr("Rename Profile"))
        .body(tr("Enter a new name for this profile:"))
        .build();
    dialog.set_accessible_role(gtk4::AccessibleRole::Dialog);
    dialog.update_property(&[gtk4::accessible::Property::Label(&tr("Rename profile"))]);

    // Create an EntryRow for the new name input
    let entry_row = adw::EntryRow::builder()
        .title(tr("Profile _Name"))
        .use_underline(true)
        .text(current_name)
        .build();
    entry_row.update_property(&[
        gtk4::accessible::Property::Label(&tr("Profile name")),
        gtk4::accessible::Property::Description(&tr("Enter a descriptive name for this profile")),
    ]);

    // Wrap the entry row in a preferences group for proper styling
    let group = adw::PreferencesGroup::new();
    group.add(&entry_row);
    dialog.set_extra_child(Some(&group));

    dialog.add_response("cancel", &tr("Cancel"));
    dialog.add_response("rename", &tr("Rename"));
    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("rename"));
    dialog.set_close_response("cancel");

    let presenter_rename = Rc::clone(presenter);
    let rc_rename = Rc::clone(rc);
    let profile_id = profile_id.to_string();
    dialog.connect_response(None, move |dlg, response| {
        if response == "rename" {
            let new_name = entry_row.text().to_string();
            if new_name.trim().is_empty() {
                // Show inline error - do not close dialog
                entry_row.add_css_class("error");
                return;
            }
            let mut p = presenter_rename.borrow_mut();
            p.rename_profile(&profile_id, &new_name);
            drop(p);
            rc_rename.borrow_mut().render_profiles(&rc_rename);
        }
        dlg.close();
    });
    dialog.present();
}
