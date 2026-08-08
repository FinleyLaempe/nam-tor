// SPDX-License-Identifier: GPL-3.0-only
//! Headless presenter driving the GNOME management UI.
//!
//! The GTK view ([`crate::bin::wizard`]) is a thin adapter that renders
//! [`PresenterModel`] and forwards user actions to [`ProfilePresenter`]. All
//! business logic — when to show recovery guidance, when to demand a merge
//! confirmation, how to bucket quality labels — lives here so it is testable
//! without a display server.
//!
//! The presenter is a small state machine. Every public action method
//! returns the new [`PresenterModel`] so the view can diff against the
//! previous render.

use crate::client::{FinishResult, ManagerClient, ManagerError, ProfileId, SessionId};
use crate::view_model::*;
/// State machine. Constructed with any [`ManagerClient`]; the view holds one
/// of these and calls action methods in response to signal handlers.
pub struct ProfilePresenter<C: ManagerClient> {
    client: C,
    model: PresenterModel,
    /// Cached lookup of profile id → name so confirmation dialogs can show
    /// human-readable names without an extra round-trip.
    name_index: std::collections::HashMap<String, String>,
    /// Pending task to allow cancellation of in-flight D-Bus calls.
    #[allow(dead_code)]
    pending_task: Option<glib::JoinHandle<()>>,
}

impl<C: ManagerClient> ProfilePresenter<C> {
    pub fn new(client: C) -> Self {
        Self {
            client,
            model: PresenterModel {
                profiles: Vec::new(),
                status: None,
                enrollment: None,
                review: None,
                pending_merge: None,
                pending_replace: None,
                error: None,
                message: None,
                is_loading: false,
                reduced_protection_warning: false,
            },
            name_index: std::collections::HashMap::new(),
            pending_task: None,
        }
    }

    pub fn model(&self) -> &PresenterModel {
        &self.model
    }

    pub fn client_mut(&mut self) -> &mut C {
        &mut self.client
    }

    /// Refresh profile list + status. Idempotent; the view calls it on
    /// activate and after any mutating action.
    pub fn refresh(&mut self) -> &PresenterModel {
        self.model.error = None;
        self.model.is_loading = true;

        match self.client.list_profiles() {
            Ok(list) => {
                self.name_index.clear();
                for row in &list {
                    self.name_index.insert(row.id.clone(), row.name.clone());
                }
                self.model.profiles = list.into_iter().map(ProfileRowView::from_info).collect();
            }
            Err(err) => self.set_error(&err),
        }
        match self.client.status() {
            Ok(status) => {
                self.model.reduced_protection_warning = status.always_on_emitter;
                self.model.status = Some(status);
            }
            Err(err) if self.model.error.is_none() => self.set_error(&err),
            Err(_) => {}
        }
        self.model.is_loading = false;
        &self.model
    }

    pub fn create_profile(&mut self, name: &str) -> &PresenterModel {
        self.model.error = None;
        self.model.message = None;

        if name.trim().is_empty() {
            self.model.error = Some(ErrorView {
                title: "Invalid name".into(),
                detail: "Profile name cannot be empty".into(),
                recovery: None,
            });
            return &self.model;
        }

        if name.len() > 64 {
            self.model.error = Some(ErrorView {
                title: "Invalid name".into(),
                detail: "Profile name is too long (max 64 characters)".into(),
                recovery: None,
            });
            return &self.model;
        }

        if self.name_index.values().any(|n| n == name) {
            self.model.error = Some(ErrorView {
                title: "Duplicate name".into(),
                detail: format!("A profile named '{name}' already exists"),
                recovery: None,
            });
            return &self.model;
        }

        match self.client.create_profile(name) {
            Ok(session) => match self.client.start_enrollment(&session) {
                Ok(()) => match self.client.get_progress(&session) {
                    Ok(progress) => {
                        self.model.enrollment = Some(EnrollmentView::from_progress(progress));
                    }
                    Err(err) => self.set_error(&err),
                },
                Err(err) => self.set_error(&err),
            },
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    pub fn poll_enrollment(&mut self) -> &PresenterModel {
        let Some(enrollment) = self.model.enrollment.clone() else {
            return &self.model;
        };
        let session = SessionId(enrollment.session_id);
        match self.client.get_progress(&session) {
            Ok(progress) => {
                self.model.enrollment = Some(EnrollmentView::from_progress(progress));
            }
            Err(err) => {
                // If the daemon is unavailable, we set an error.
                // The view will likely show the RecoveryView.
                self.set_error(&err);
            }
        }
        &self.model
    }

    /// Finalize enrollment. On success the presenter clears the enrollment
    /// view and refreshes the profile list so the new profile appears.
    pub fn finish_enrollment(&mut self) -> &PresenterModel {
        let Some(enrollment) = self.model.enrollment.clone() else {
            return &self.model;
        };
        let session = SessionId(enrollment.session_id);
        match self.client.finish_enrollment(&session) {
            Ok(FinishResult {
                success,
                profile_id,
                reason,
            }) => {
                self.model.enrollment = None;
                if success {
                    self.model.message = Some(format!(
                        "Enrollment complete{}",
                        profile_id
                            .as_ref()
                            .map(|id| format!(" — profile {id}"))
                            .unwrap_or_default()
                    ));
                    self.refresh();
                } else {
                    let reason_msg = reason
                        .clone()
                        .unwrap_or_else(|| "Enrollment did not complete".into());
                    self.model.message = Some(reason_msg.clone());
                    self.model.error = Some(ErrorView {
                        title: "Enrollment failed".into(),
                        detail: reason_msg,
                        recovery: None,
                    });
                }
            }
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    pub fn request_review(&mut self) -> &PresenterModel {
        let Some(enrollment) = self.model.enrollment.clone() else {
            return &self.model;
        };
        let session = SessionId(enrollment.session_id);
        match self.client.get_progress(&session) {
            Ok(progress) => {
                self.model.review = Some(ReviewView {
                    coverage_percent: progress.coverage_percent,
                    quality_label: progress.quality_label.as_str().into(),
                    template_count: progress.frames_captured,
                });
            }
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    pub fn commit_profile(&mut self) -> &PresenterModel {
        let Some(_review) = self.model.review.clone() else {
            return &self.model;
        };
        &self.model
    }

    pub fn retry_enrollment(&mut self) -> &PresenterModel {
        self.model.review = None;
        &self.model
    }

    /// Cancel an in-flight enrollment. The presenter clears local state and
    /// asks the daemon to clean up its side.
    pub fn cancel_enrollment(&mut self) -> &PresenterModel {
        let Some(enrollment) = self.model.enrollment.clone() else {
            return &self.model;
        };
        let session = SessionId(enrollment.session_id);
        match self.client.cancel_enrollment(&session) {
            Ok(()) => {
                self.model.enrollment = None;
                self.model.message = Some("Enrollment cancelled".into());
            }
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    /// Stage a merge for confirmation. The view shows a confirmation dialog
    /// and only calls [`Self::confirm_merge`] if the user accepts.
    pub fn request_merge(&mut self, src_id: &str, dst_id: &str) -> &PresenterModel {
        self.model.error = None;
        if src_id == dst_id {
            self.model.error = Some(ErrorView {
                title: "Invalid merge".into(),
                detail: "Cannot merge a profile with itself".into(),
                recovery: None,
            });
            return &self.model;
        }
        let src_name = self.name_index.get(src_id).cloned();
        let dst_name = self.name_index.get(dst_id).cloned();
        match (src_name, dst_name) {
            (Some(src_name), Some(dst_name)) => {
                self.model.pending_merge = Some(PendingMerge {
                    src_id: src_id.into(),
                    src_name,
                    dst_id: dst_id.into(),
                    dst_name,
                });
            }
            _ => {
                self.model.error = Some(ErrorView {
                    title: "Cannot stage merge".into(),
                    detail: "source or destination profile not found".into(),
                    recovery: None,
                });
            }
        }
        &self.model
    }

    pub fn confirm_merge(&mut self) -> &PresenterModel {
        let Some(pending) = self.model.pending_merge.clone() else {
            return &self.model;
        };
        let src = ProfileId(pending.src_id);
        let dst = ProfileId(pending.dst_id);
        match self.client.merge_profiles(&src, &dst) {
            Ok(()) => {
                self.model.pending_merge = None;
                self.model.message = Some(format!(
                    "Merged '{}' into '{}'",
                    pending.src_name, pending.dst_name
                ));
                self.refresh();
            }
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    pub fn cancel_merge(&mut self) -> &PresenterModel {
        self.model.pending_merge = None;
        &self.model
    }

    /// Stage a replace (overwrite) for confirmation. `name` is the new
    /// display name the re-enrolled profile will keep.
    pub fn request_replace(&mut self, id: &str, new_name: &str) -> &PresenterModel {
        self.model.error = None;
        let old_name = self.name_index.get(id).cloned();
        match old_name {
            Some(old_name) => {
                self.model.pending_replace = Some(PendingReplace {
                    id: id.into(),
                    old_name,
                    new_name: new_name.into(),
                });
            }
            None => {
                self.model.error = Some(ErrorView {
                    title: "Cannot stage replace".into(),
                    detail: "profile not found".into(),
                    recovery: None,
                });
            }
        }
        &self.model
    }

    pub fn confirm_replace(&mut self) -> &PresenterModel {
        let Some(pending) = self.model.pending_replace.clone() else {
            return &self.model;
        };
        let id = ProfileId(pending.id);
        match self.client.replace_profile(&id, &pending.new_name) {
            Ok(()) => {
                self.model.pending_replace = None;
                self.model.message = Some(format!(
                    "Replaced '{}' with new enrollment as '{}'",
                    pending.old_name, pending.new_name
                ));
                self.refresh();
            }
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    pub fn cancel_replace(&mut self) -> &PresenterModel {
        self.model.pending_replace = None;
        &self.model
    }

    pub fn rename_profile(&mut self, id: &str, new_name: &str) -> &PresenterModel {
        self.model.error = None;
        if new_name.trim().is_empty() {
            self.model.error = Some(ErrorView {
                title: "Invalid name".into(),
                detail: "Profile name cannot be empty".into(),
                recovery: None,
            });
            return &self.model;
        }
        if self.name_index.values().any(|n| n == new_name) {
            self.model.error = Some(ErrorView {
                title: "Duplicate name".into(),
                detail: "A profile with this name already exists".into(),
                recovery: None,
            });
            return &self.model;
        }
        match self.client.rename_profile(&ProfileId(id.into()), new_name) {
            Ok(()) => {
                self.model.message = Some(format!("Renamed profile to '{new_name}'"));
                self.refresh();
            }
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    pub fn enable_profile(&mut self, id: &str, enabled: bool) -> &PresenterModel {
        self.model.error = None;
        match self.client.enable_profile(&ProfileId(id.into()), enabled) {
            Ok(()) => {
                self.model.message = Some(format!(
                    "Profile {}",
                    if enabled { "enabled" } else { "disabled" }
                ));
                self.refresh();
            }
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    pub fn delete_profile(&mut self, id: &str) -> &PresenterModel {
        self.model.error = None;
        match self.client.delete_profile(&ProfileId(id.into())) {
            Ok(()) => {
                self.model.message = Some("Profile deleted".into());
                self.refresh();
            }
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    pub fn dismiss_error(&mut self) -> &PresenterModel {
        self.model.error = None;
        &self.model
    }

    fn set_error(&mut self, err: &ManagerError) {
        let recovery = RecoveryHint::for_error(err);
        let title = match err {
            ManagerError::DaemonUnavailable(_) => "Daemon unavailable".into(),
            ManagerError::ServiceNotRegistered => "Service not registered".into(),
            ManagerError::PolkitDenied(_) => "Authentication denied".into(),
            ManagerError::NotFound(_) => "Not found".into(),
            ManagerError::Conflict(_) => "Conflict".into(),
            ManagerError::NotEnrolling(_) => "Not enrolling".into(),
            ManagerError::Transport(_) => "Connection error".into(),
        };
        self.model.error = Some(ErrorView {
            title,
            detail: err.to_string(),
            recovery,
        });
    }
}

/// Convenience: run a presenter action and return the resulting model clone.
/// Used by tests to snapshot state without borrowing the presenter.
pub fn after<C: ManagerClient, R>(
    presenter: &mut ProfilePresenter<C>,
    action: impl FnOnce(&mut ProfilePresenter<C>) -> R,
) -> PresenterModel
where
    R: Sized,
{
    action(presenter);
    presenter.model().clone()
}
