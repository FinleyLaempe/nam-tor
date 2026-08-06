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

use crate::client::{
    EnrollmentProgress, FinishResult, ManagerClient, ManagerError, ProfileId, ProfileInfo,
    QualityLabel, SessionId, StatusInfo,
};

/// Coarse quality bucket the user sees, plus a guidance flag the view renders
/// as an accessibility-friendly alert when diversity is low.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityView {
    pub label: String,
    pub show_guidance: bool,
}

impl QualityView {
    pub fn from_label(label: QualityLabel) -> Self {
        match label {
            QualityLabel::Low => Self {
                label: "low".into(),
                show_guidance: true,
            },
            QualityLabel::Medium => Self {
                label: "medium".into(),
                show_guidance: false,
            },
            QualityLabel::High => Self {
                label: "high".into(),
                show_guidance: false,
            },
        }
    }
}

/// One row in the profile list. The view renders name, enabled toggle, and a
/// localized "last used" line. No biometric data, ever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRowView {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub created: u64,
    pub last_used: Option<u64>,
}

impl ProfileRowView {
    pub fn from_info(info: ProfileInfo) -> Self {
        Self {
            id: info.id,
            name: info.name,
            enabled: info.enabled,
            created: info.created,
            last_used: info.last_used,
        }
    }
}

/// Recovery guidance shown when the daemon is unreachable. The text is
/// deliberately actionable, not a raw error dump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryHint {
    pub title: String,
    pub steps: Vec<String>,
}

impl RecoveryHint {
    pub fn for_error(err: &ManagerError) -> Option<Self> {
        match err {
            ManagerError::DaemonUnavailable(detail) => Some(Self {
                title: "Nam-tor daemon is not running".into(),
                steps: vec![
                    "Start it with: systemctl --user start namtor-daemon".into(),
                    format!("Detail: {detail}"),
                ],
            }),
            ManagerError::ServiceNotRegistered => Some(Self {
                title: "Nam-tor service is not registered on the session bus".into(),
                steps: vec![
                    "Install the namtor-daemon D-Bus service file".into(),
                    "Then run: systemctl --user start namtor-daemon".into(),
                ],
            }),
            ManagerError::PolkitDenied(detail) => Some(Self {
                title: "Administrator approval required".into(),
                steps: vec![format!("Polkit denied: {detail}")],
            }),
            _ => None,
        }
    }
}

/// View model rendered by the GTK window. Every field is plain data the view
/// can serialize into Atk roles and gettext strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenterModel {
    pub profiles: Vec<ProfileRowView>,
    pub status: Option<StatusInfo>,
    pub enrollment: Option<EnrollmentView>,
    pub pending_merge: Option<PendingMerge>,
    pub pending_replace: Option<PendingReplace>,
    pub error: Option<ErrorView>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentView {
    pub session_id: String,
    pub frames_captured: u32,
    pub quality: QualityView,
    pub coverage_percent: u32,
}

impl EnrollmentView {
    pub fn from_progress(p: EnrollmentProgress) -> Self {
        let quality = QualityView::from_label(p.quality_label);
        Self {
            session_id: p.session_id,
            frames_captured: p.frames_captured,
            quality,
            coverage_percent: p.coverage_percent,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMerge {
    pub src_id: String,
    pub src_name: String,
    pub dst_id: String,
    pub dst_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReplace {
    pub id: String,
    pub old_name: String,
    pub new_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorView {
    pub title: String,
    pub detail: String,
    pub recovery: Option<RecoveryHint>,
}

/// State machine. Constructed with any [`ManagerClient`]; the view holds one
/// of these and calls action methods in response to signal handlers.
pub struct ProfilePresenter<C: ManagerClient> {
    client: C,
    model: PresenterModel,
    /// Cached lookup of profile id → name so confirmation dialogs can show
    /// human-readable names without an extra round-trip.
    name_index: std::collections::HashMap<String, String>,
}

impl<C: ManagerClient> ProfilePresenter<C> {
    pub fn new(client: C) -> Self {
        Self {
            client,
            model: PresenterModel {
                profiles: Vec::new(),
                status: None,
                enrollment: None,
                pending_merge: None,
                pending_replace: None,
                error: None,
                message: None,
            },
            name_index: std::collections::HashMap::new(),
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
            Ok(status) => self.model.status = Some(status),
            Err(err) if self.model.error.is_none() => self.set_error(&err),
            Err(_) => {}
        }
        &self.model
    }

    /// Begin a new enrollment. On success the presenter enters the
    /// `enrolling` state and the view switches to the capture page.
    pub fn create_profile(&mut self, name: &str) -> &PresenterModel {
        self.model.error = None;
        self.model.message = None;
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

    /// Poll the live enrollment. The view calls this on a refresh tick; the
    /// presenter surfaces quality guidance as soon as the daemon reports
    /// `quality_label == "low"`.
    pub fn poll_enrollment(&mut self) -> &PresenterModel {
        let Some(enrollment) = self.model.enrollment.clone() else {
            return &self.model;
        };
        let session = SessionId(enrollment.session_id);
        match self.client.get_progress(&session) {
            Ok(progress) => {
                self.model.enrollment = Some(EnrollmentView::from_progress(progress));
            }
            Err(err) => self.set_error(&err),
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
