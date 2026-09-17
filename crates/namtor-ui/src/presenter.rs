// SPDX-License-Identifier: GPL-3.0-only
//! Headless presenter driving the Nam-tor management UI.
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
//!
//! # Async dispatch
//!
//! The synchronous action methods (e.g. [`Self::refresh`], [`Self::start_enrollment`])
//! are the testable surface — they call the [`ManagerClient`] directly and
//! update the model in place. The UI shell wraps these in
//! [`Self::dispatch_async`] which spawns the call on a background thread so
//! the GTK main loop never blocks on a D-Bus round-trip. Cancellation is
//! handled via [`Self::cancel_pending`] which aborts the in-flight task.

use std::sync::Arc;

use crate::client::{
    EnrollmentProgress, EnrollmentProgressV3, ManagerClient, ManagerError, PamServiceCompatibility,
    ProfileId, SessionId, StatusReport,
};
use crate::view_model::*;

/// State machine. Constructed with any [`ManagerClient`]; the view holds one
/// of these and calls action methods in response to signal handlers.
///
/// The client is wrapped in `Arc` so async dispatch can share it across
/// threads. `C: ManagerClient + Send + Sync` is required for async use;
/// synchronous tests do not spawn threads and thus work with any `C`.
pub struct ProfilePresenter<C: ManagerClient> {
    client: Arc<C>,
    model: PresenterModel,
    /// Cached lookup of profile id → name so confirmation dialogs can show
    /// human-readable names without an extra round-trip.
    name_index: std::collections::HashMap<String, String>,
    /// The uid of the user whose profiles are being managed. Passed to v1
    /// mutation methods that require a target uid.
    uid: u32,
    /// Pending async task handle. When `Some`, an operation is in flight;
    /// [`Self::cancel_pending`] aborts it.
    pending_task: Option<glib::JoinHandle<()>>,
    /// Consecutive failed progress polls. Reset by any successful poll.
    poll_failures: u8,
}

impl<C: ManagerClient> ProfilePresenter<C> {
    /// Construct a presenter for `uid` backed by `client`.
    pub fn new(client: C, uid: u32) -> Self {
        Self {
            client: Arc::new(client),
            model: PresenterModel::default(),
            name_index: std::collections::HashMap::new(),
            uid,
            pending_task: None,
            poll_failures: 0,
        }
    }

    /// Construct a presenter from an already-shared `Arc<C>`. Tests use this
    /// to share a single fake across multiple presenter instances (simulating
    /// reconnect).
    pub fn from_arc(client: Arc<C>, uid: u32) -> Self {
        Self {
            client,
            model: PresenterModel::default(),
            name_index: std::collections::HashMap::new(),
            uid,
            pending_task: None,
            poll_failures: 0,
        }
    }

    /// The uid being managed.
    pub fn uid(&self) -> u32 {
        self.uid
    }

    pub fn model(&self) -> &PresenterModel {
        &self.model
    }

    /// Borrow the underlying client (e.g. for direct test assertions).
    pub fn client(&self) -> &C {
        &self.client
    }

    /// Clone the `Arc` to the client — used by async dispatch to share the
    /// client across threads.
    pub fn client_arc(&self) -> Arc<C> {
        Arc::clone(&self.client)
    }

    /// True when an async operation is in flight.
    pub fn is_busy(&self) -> bool {
        self.pending_task.is_some()
    }

    /// Abort the in-flight async task, if any. The model is left in its
    /// current state; the caller may call [`Self::dismiss_error`] or
    /// [`Self::refresh`] afterwards.
    pub fn cancel_pending(&mut self) {
        if let Some(handle) = self.pending_task.take() {
            handle.abort();
        }
        self.model.is_loading = false;
    }

    // -----------------------------------------------------------------------
    // Sync action methods (testable surface)
    // -----------------------------------------------------------------------

    /// Refresh profile list, status, PAM services, and protection status.
    /// Idempotent; the view calls it on activate and after any mutating
    /// action. On reconnect this restores the full UI state.
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

        // GetStatus determines daemon readiness. We call it once and reuse
        // the result for status, readiness, and emitter-armed.
        let status_result = self.client.get_status();
        match &status_result {
            Ok(StatusReport { emitter_armed, .. }) => {
                self.model.emitter_armed_warning = *emitter_armed;
            }
            Err(err) if self.model.error.is_none() => self.set_error(err),
            Err(_) => {}
        }
        self.model.status = status_result.ok();
        self.model.is_loading = false;

        // Compose the sub-destinations so a single refresh() restores the
        // full UI state (PAM inventory + system status).
        self.refresh_pam_services();
        self.refresh_system_status();
        &self.model
    }

    /// Refresh the PAM service inventory and categorize them for the simplified view.
    pub fn refresh_pam_services(&mut self) -> &PresenterModel {
        match self.client.list_pam_services() {
            Ok(services) => {
                let rows: Vec<PamServiceRowView> = services
                    .into_iter()
                    .map(PamServiceRowView::from_info)
                    .collect();

                let mut auth_view = AuthenticationView::default();
                let mut safe = Vec::new();
                let mut risky = Vec::new();
                let mut unsupported = Vec::new();

                for row in &rows {
                    match row.compatibility {
                        PamServiceCompatibility::Safe => safe.push(row.clone()),
                        PamServiceCompatibility::Risky => risky.push(row.clone()),
                        PamServiceCompatibility::Unsupported => unsupported.push(row.clone()),
                    }
                }

                auth_view.recommended_service =
                    safe.iter().find(|s| s.id == "gdm-password").cloned();

                auth_view.safe_services = safe;
                auth_view.risky_services = risky;
                auth_view.unsupported_services = unsupported;

                self.model.authentication = Some(auth_view);
                self.model.pam_services = rows;
            }
            Err(err) if self.model.error.is_none() => self.set_error(&err),
            Err(_) => {}
        }
        &self.model
    }

    /// Toggle a PAM service from the simplified view.
    pub fn toggle_pam_service(&mut self, id: &str, enabled: bool) -> &PresenterModel {
        self.request_pam_toggle(id, enabled)
    }

    /// Refresh daemon, camera, and model status for the system destination.
    pub fn refresh_system_status(&mut self) -> &PresenterModel {
        let daemon_reachable = self.client.get_status().is_ok();
        let protection = self.client.get_protection_status();

        self.model.system = Some(SystemView {
            daemon_running: daemon_reachable,
            // Probed locally. The wire has no readiness method, so this was
            // `daemon_reachable` — the System page told a user with no camera
            // that their camera was detected.
            camera_detected: crate::supported_camera_present(),
            // Sound: the daemon validates its models and spawns the worker
            // before it binds the socket, so a reachable daemon has them.
            models_loaded: daemon_reachable,
            protection: if let Ok(p) = protection {
                ProtectionView::from_status(&p, self.model.emitter_armed_warning)
            } else {
                ProtectionView::default()
            },
        });
        &self.model
    }

    /// Start a new enrollment session. Replaces the old two-step
    /// `create_profile` + `start_enrollment` with a single
    /// `StartEnrollment(uid, name)` call that matches the daemon contract.
    pub fn start_enrollment(&mut self, name: &str) -> &PresenterModel {
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

        match self.client.start_enrollment(self.uid, name) {
            Ok(session) => match self.client.poll_progress(&session) {
                Ok(Some(progress)) => {
                    self.model.enrollment = Some(EnrollmentView::from_progress(progress));
                }
                Ok(None) => {
                    // No signal yet; show an initial enrollment view.
                    self.model.enrollment = Some(EnrollmentView {
                        session_id: session.0.clone(),
                        frames_captured: 0,
                        quality: QualityView::from_label(crate::client::QualityLabel::Low),
                        coverage_percent: 0,
                    });
                }
                Err(err) => self.set_error(&err),
            },
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    /// Poll the latest enrollment progress. The view calls this on a timer
    /// (e.g. every 500 ms) while `model.enrollment` is `Some`.
    pub fn poll_progress(&mut self) -> &PresenterModel {
        let Some(enrollment) = self.model.enrollment.clone() else {
            return &self.model;
        };
        let session = SessionId(enrollment.session_id);
        match self.client.poll_progress(&session) {
            Ok(Some(progress)) => {
                self.model.enrollment = Some(EnrollmentView::from_progress(progress));
            }
            Ok(None) => {}
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    /// Request a review of the in-flight enrollment. Fetches the latest
    /// progress and stages it in `model.review`.
    pub fn request_review(&mut self) -> &PresenterModel {
        let Some(enrollment) = self.model.enrollment.clone() else {
            return &self.model;
        };
        let session_id = enrollment.session_id.clone();
        let session = SessionId(session_id.clone());
        match self.client.poll_progress(&session) {
            Ok(Some(EnrollmentProgress {
                coverage_percent,
                quality,
                frames_captured,
                ..
            })) => {
                self.model.review = Some(ReviewView {
                    profile_id: session_id,
                    needs_reenroll: matches!(quality, crate::client::QualityLabel::Low),
                    quality_label: quality.as_str().into(),
                    coverage_percent,
                    frames_captured,
                })
            }
            Ok(None) => {
                self.model.review = Some(ReviewView {
                    profile_id: session_id,
                    needs_reenroll: false,
                    quality_label: enrollment.quality.label.clone(),
                    coverage_percent: enrollment.coverage_percent,
                    frames_captured: enrollment.frames_captured,
                })
            }
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    /// Commit the in-flight enrollment to a persistent profile. This calls
    /// `CommitProfile(session)` on the daemon, which writes the template to
    /// the encrypted store. On success the enrollment view is cleared and
    /// the profile list is refreshed so the new profile appears. On failure
    /// the enrollment view is also cleared (the daemon removes the session
    /// regardless of outcome) and the error is surfaced.
    pub fn commit_profile(&mut self) -> &PresenterModel {
        let Some(enrollment) = self.model.enrollment.clone() else {
            return &self.model;
        };
        let session = SessionId(enrollment.session_id);
        match self.client.commit_profile(&session) {
            Ok(profile_id) => {
                self.model.enrollment = None;
                self.model.review = None;
                self.model.message = Some(format!("Enrollment complete — profile {profile_id}"));
                self.refresh();
            }
            Err(err) => {
                self.model.enrollment = None;
                self.model.review = None;
                self.set_error(&err);
            }
        }
        &self.model
    }

    /// Retry enrollment from scratch — clears the review and returns to the
    /// capture phase.
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
                self.model.review = None;
                self.model.message = Some("Enrollment cancelled".into());
            }
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    // -----------------------------------------------------------------------
    // v3 enrollment lifecycle (org.namtor.Manager3)
    // -----------------------------------------------------------------------

    /// v3: Start a new enrollment session. Transitions to Capturing on
    /// success. Validates the profile name before calling the daemon.
    pub fn start_enrollment_v3(&mut self, name: &str) -> &PresenterModel {
        self.model.error = None;
        self.model.message = None;

        if self.model.enrollment_state == EnrollmentState::Capturing
            || self.model.enrollment_state == EnrollmentState::Saving
        {
            self.model.error = Some(ErrorView {
                title: "Enrollment in progress".into(),
                detail: "An enrollment is already active".into(),
                recovery: None,
            });
            return &self.model;
        }

        if name.trim().is_empty() {
            self.model.enrollment_state = EnrollmentState::Error("Name cannot be empty".into());
            self.model.error = Some(ErrorView {
                title: "Invalid name".into(),
                detail: "Profile name cannot be empty".into(),
                recovery: None,
            });
            return &self.model;
        }

        if name.len() > 64 {
            self.model.enrollment_state = EnrollmentState::Error("Name too long".into());
            self.model.error = Some(ErrorView {
                title: "Invalid name".into(),
                detail: "Profile name is too long (max 64 characters)".into(),
                recovery: None,
            });
            return &self.model;
        }

        // USA-3: duplicate-name check. If the name matches an existing
        // profile, stage a `PendingReplace` so the UI can prompt the user
        // to confirm overwriting the old profile (not a silent duplicate).
        if self.name_index.values().any(|n| n == name) {
            let existing_id = self
                .name_index
                .iter()
                .find(|(_, n)| n.as_str() == name)
                .map(|(id, _)| id.clone())
                .unwrap_or_default();
            self.model.pending_replace = Some(PendingReplace {
                id: existing_id,
                old_name: name.to_string(),
                new_name: name.to_string(),
            });
            self.model.error = Some(ErrorView {
                title: "Duplicate name".into(),
                detail: format!("A profile named '{name}' already exists. Replace it?"),
                recovery: None,
            });
            return &self.model;
        }

        match self.client.start_enrollment_v3(self.uid, name) {
            Ok(session) => {
                self.model.enrollment_state = EnrollmentState::Capturing;
                self.model.enrollment_session_v3 = Some(session.0.clone());
                self.model.review_view_v3 = None;
                self.model.enrollment_view_v3 = Some(EnrollmentViewV3 {
                    phase: "capturing".into(),
                    quality_percent: 0,
                    diversity_percent: 0,
                    frames_seen: 0,
                    frames_accepted: 0,
                    frames_rejected: 0,
                    quality_bucket: 0,
                    guidance: crate::client::Guidance::Capturing.into(),
                });
            }
            Err(err) => {
                self.model.enrollment_state = EnrollmentState::Error(err.to_string());
                self.set_error(&err);
            }
        }
        &self.model
    }

    /// Consecutive failed polls tolerated before the scan is failed. At the
    /// 500 ms poll interval this is about a second and a half of silence —
    /// long enough to ride out a dropped D-Bus call, short enough that a dead
    /// daemon is reported while the user is still looking at the screen.
    const MAX_POLL_FAILURES: u8 = 3;

    pub fn poll_progress_v3(&mut self) -> bool {
        let Some(ref session) = self.model.enrollment_session_v3 else {
            return false;
        };
        if self.model.enrollment_state != EnrollmentState::Capturing {
            return true;
        }
        match self.client.poll_progress_v3(session) {
            Ok(Some(progress)) => {
                self.poll_failures = 0;
                self.model.enrollment_view_v3 = Some(EnrollmentViewV3::from_progress(&progress));
                true
            }
            // A successful call with nothing new is still a live daemon.
            Ok(None) => {
                self.poll_failures = 0;
                false
            }
            // A failed poll is not progress. `Err(_) => true` discarded the
            // error and reported "re-render", so the page redrew the last
            // frozen numbers forever: a daemon that died mid-scan looked
            // exactly like a scan that had stopped improving, with no error,
            // no state change and nothing to act on.
            //
            // One transient failure is not fatal — D-Bus can drop a call
            // while the daemon is busy — so a short run is tolerated before
            // the scan is failed.
            Err(err) => {
                self.poll_failures = self.poll_failures.saturating_add(1);
                if self.poll_failures >= Self::MAX_POLL_FAILURES {
                    self.model.enrollment_state = EnrollmentState::Error(err.to_string());
                    self.set_error(&err);
                    return true;
                }
                false
            }
        }
    }

    /// v3: Handle a progress signal from the daemon. Ignores stale signals
    /// (session id does not match the active session). The view calls this
    /// from a glib idle callback after receiving the D-Bus signal.
    pub fn on_progress_v3(&mut self, progress: &EnrollmentProgressV3) -> &PresenterModel {
        let Some(active) = &self.model.enrollment_session_v3 else {
            return &self.model;
        };
        if progress.session != *active {
            return &self.model;
        }
        if self.model.enrollment_state != EnrollmentState::Capturing {
            return &self.model;
        }
        self.model.enrollment_view_v3 = Some(EnrollmentViewV3::from_progress(progress));
        &self.model
    }

    /// v3: Stop the active enrollment. Transitions to Stopped.
    pub fn stop_enrollment_v3(&mut self) -> &PresenterModel {
        let Some(session) = self.model.enrollment_session_v3.clone() else {
            return &self.model;
        };
        if self.model.enrollment_state != EnrollmentState::Capturing {
            return &self.model;
        }
        match self.client.stop_enrollment_v3(&session) {
            Ok(()) => {
                self.model.enrollment_state = EnrollmentState::Stopped;
                if let Some(view) = &mut self.model.enrollment_view_v3 {
                    view.phase = "stopped".into();
                }
            }
            Err(err) => {
                self.model.enrollment_state = EnrollmentState::Error(err.to_string());
                self.set_error(&err);
            }
        }
        &self.model
    }

    /// v3: Fetch a review snapshot for the stopped enrollment. Transitions
    /// to Reviewing.
    pub fn review_enrollment_v3(&mut self) -> &PresenterModel {
        let Some(session) = self.model.enrollment_session_v3.clone() else {
            return &self.model;
        };
        if self.model.enrollment_state != EnrollmentState::Stopped {
            return &self.model;
        }
        match self.client.review_enrollment_v3(&session) {
            Ok(review) => {
                self.model.review_view_v3 = Some(ReviewViewV3::from_review(&review));
                self.model.enrollment_state = EnrollmentState::Reviewing;
            }
            Err(err) => {
                self.model.enrollment_state = EnrollmentState::Error(err.to_string());
                self.set_error(&err);
            }
        }
        &self.model
    }

    /// v3: Resume capture from the stopped state. Transitions back to
    /// Capturing.
    pub fn resume_enrollment_v3(&mut self) -> &PresenterModel {
        let Some(session) = self.model.enrollment_session_v3.clone() else {
            return &self.model;
        };
        if self.model.enrollment_state != EnrollmentState::Stopped
            && self.model.enrollment_state != EnrollmentState::Reviewing
        {
            return &self.model;
        }
        match self.client.resume_enrollment_v3(&session) {
            Ok(()) => {
                self.model.enrollment_state = EnrollmentState::Capturing;
                self.model.review_view_v3 = None;
                if let Some(view) = &mut self.model.enrollment_view_v3 {
                    view.phase = "capturing".into();
                }
            }
            Err(err) => {
                self.model.enrollment_state = EnrollmentState::Error(err.to_string());
                self.set_error(&err);
            }
        }
        &self.model
    }

    /// v3: Commit the enrollment to a persistent profile. Transitions to
    /// Saving, then back to Setup (profile list) on success.
    pub fn save_profile_v3(&mut self) -> &PresenterModel {
        let Some(session) = self.model.enrollment_session_v3.clone() else {
            return &self.model;
        };
        if self.model.enrollment_state != EnrollmentState::Reviewing
            && self.model.enrollment_state != EnrollmentState::Stopped
        {
            return &self.model;
        }
        if self.model.enrollment_state == EnrollmentState::Saving {
            return &self.model;
        }
        self.model.enrollment_state = EnrollmentState::Saving;
        match self.client.commit_profile_v3(&session) {
            Ok(profile_id) => {
                self.model.enrollment_state = EnrollmentState::Setup;
                self.model.enrollment_session_v3 = None;
                self.model.enrollment_view_v3 = None;
                self.model.review_view_v3 = None;
                self.model.message = Some(format!("Enrollment complete — profile {profile_id}"));
                self.refresh();
            }
            Err(err) => {
                self.model.enrollment_state = EnrollmentState::Stopped;
                self.set_error(&err);
            }
        }
        &self.model
    }

    /// v3: Discard the enrollment. Cancels on the daemon side and returns
    /// to the profile list.
    pub fn discard_enrollment_v3(&mut self) -> &PresenterModel {
        let Some(session) = self.model.enrollment_session_v3.clone() else {
            self.model.enrollment_state = EnrollmentState::Setup;
            return &self.model;
        };
        let _ = self.client.cancel_enrollment_v3(&session);
        self.model.enrollment_state = EnrollmentState::Setup;
        self.model.enrollment_session_v3 = None;
        self.model.enrollment_view_v3 = None;
        self.model.review_view_v3 = None;
        self.model.message = Some("Enrollment discarded".into());
        &self.model
    }

    /// v3: Cancel from any state. Same as discard but may be called from
    /// any enrollment state (including Error).
    pub fn cancel_enrollment_v3(&mut self) -> &PresenterModel {
        if let Some(session) = self.model.enrollment_session_v3.clone() {
            let _ = self.client.cancel_enrollment_v3(&session);
        }
        self.model.enrollment_state = EnrollmentState::Setup;
        self.model.enrollment_session_v3 = None;
        self.model.enrollment_view_v3 = None;
        self.model.review_view_v3 = None;
        self.model.error = None;
        &self.model
    }

    /// v3: Dismiss the error state and return to Setup so the user can
    /// start again.
    pub fn dismiss_v3_error(&mut self) -> &PresenterModel {
        if matches!(self.model.enrollment_state, EnrollmentState::Error(_)) {
            self.model.enrollment_state = EnrollmentState::Setup;
            self.model.enrollment_session_v3 = None;
            self.model.enrollment_view_v3 = None;
            self.model.review_view_v3 = None;
            self.model.error = None;
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
        match self.client.merge_profiles(self.uid, &src, &dst) {
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
        match self
            .client
            .replace_profile(self.uid, &id, &pending.new_name)
        {
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

    /// USA-3: Confirm a replace triggered by a duplicate-name enrollment
    /// attempt. Deletes the old profile and starts a fresh v3 enrollment
    /// with the same name so the user goes through the normal capture flow.
    pub fn confirm_replace_v3(&mut self) -> &PresenterModel {
        let Some(pending) = self.model.pending_replace.clone() else {
            return &self.model;
        };
        match self
            .client
            .delete_profile(self.uid, &ProfileId(pending.id.clone()))
        {
            Ok(()) => {
                self.model.pending_replace = None;
                self.model.error = None;
                self.name_index.remove(&pending.id);
                self.start_enrollment_v3(&pending.new_name);
            }
            Err(err) => self.set_error(&err),
        }
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
        match self
            .client
            .rename_profile(self.uid, &ProfileId(id.into()), new_name)
        {
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
        match self
            .client
            .enable_profile(self.uid, &ProfileId(id.into()), enabled)
        {
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
        match self.client.delete_profile(self.uid, &ProfileId(id.into())) {
            Ok(()) => {
                self.model.message = Some("Profile deleted".into());
                self.refresh();
            }
            Err(err) => self.set_error(&err),
        }
        &self.model
    }

    /// v2: Toggle a PAM service. Requires polkit `org.namtor.configure-pam`.
    ///
    /// This is the raw apply — it bypasses the high-impact confirmation
    /// staging. The view should call [`Self::request_pam_toggle`] instead,
    /// which stages risky enable requests. Tests and `confirm_pam_toggle`
    /// call this directly.
    ///
    /// Unsupported services are refused here as a defence-in-depth guard;
    /// the view greys out their switches, and [`Self::request_pam_toggle`]
    /// also refuses them.
    pub fn set_pam_service_enabled(&mut self, service_id: &str, enabled: bool) -> &PresenterModel {
        self.model.error = None;

        if let Some(svc) = self.model.pam_services.iter().find(|s| s.id == service_id) {
            if svc.compatibility == crate::client::PamServiceCompatibility::Unsupported {
                self.model.error = Some(ErrorView {
                    title: "Unsupported PAM service".into(),
                    detail: format!(
                        "'{}' cannot be enabled — its PAM stack shape is incompatible.",
                        svc.display_name
                    ),
                    recovery: None,
                });
                return &self.model;
            }
        }

        match self.client.set_pam_service_enabled(service_id, enabled) {
            Ok(success) => {
                if success {
                    self.model.message = Some(format!(
                        "PAM service '{service_id}' {}",
                        if enabled { "enabled" } else { "disabled" }
                    ));
                } else {
                    self.model.error = Some(ErrorView {
                        title: "PAM toggle failed".into(),
                        detail: format!(
                            "Daemon refused to {state} '{service_id}'",
                            state = if enabled { "enable" } else { "disable" }
                        ),
                        recovery: None,
                    });
                }
                self.refresh();
            }
            Err(err) => {
                self.set_error(&err);
                // Re-read the service list so the switch snaps back to what is
                // actually configured. `refresh_pam_services`, not `refresh`:
                // the latter opens with `self.model.error = None` and would
                // erase the error we just recorded.
                self.refresh_pam_services();
            }
        }
        &self.model
    }

    /// v2: Request a PAM service toggle. This is the entry point the view
    /// calls when the user flips a switch.
    ///
    /// - Disabling any service applies immediately.
    /// - Enabling a `Safe` service applies immediately.
    /// - Enabling an `Unsupported` service is refused with an error.
    /// - Enabling a `Risky` (high-impact) service stages a
    ///   [`PendingPamToggle`] in the model. The view shows the experimental-
    ///   risk confirmation dialog; the user must acknowledge before
    ///   [`Self::confirm_pam_toggle`] applies the toggle.
    pub fn request_pam_toggle(&mut self, service_id: &str, enabled: bool) -> &PresenterModel {
        self.model.error = None;
        self.model.pending_pam_toggle = None;

        let Some(svc) = self
            .model
            .pam_services
            .iter()
            .find(|s| s.id == service_id)
            .cloned()
        else {
            self.model.error = Some(ErrorView {
                title: "PAM service not found".into(),
                detail: format!("Unknown PAM service '{service_id}'"),
                recovery: None,
            });
            return &self.model;
        };

        if svc.compatibility == crate::client::PamServiceCompatibility::Unsupported {
            self.model.error = Some(ErrorView {
                title: "Unsupported PAM service".into(),
                detail: format!(
                    "'{}' cannot be enabled — its PAM stack shape is incompatible.",
                    svc.display_name
                ),
                recovery: None,
            });
            return &self.model;
        }

        if !enabled {
            return self.set_pam_service_enabled(service_id, false);
        }

        if svc.compatibility == crate::client::PamServiceCompatibility::Risky {
            self.model.pending_pam_toggle = Some(PendingPamToggle {
                id: svc.id.clone(),
                display_name: svc.display_name.clone(),
                enabled: true,
                high_impact: true,
            });
            return &self.model;
        }

        self.set_pam_service_enabled(service_id, true)
    }

    /// v2: Apply the staged high-impact PAM toggle after the user confirms.
    pub fn confirm_pam_toggle(&mut self) -> &PresenterModel {
        let Some(pending) = self.model.pending_pam_toggle.clone() else {
            return &self.model;
        };
        self.model.pending_pam_toggle = None;
        self.set_pam_service_enabled(&pending.id, pending.enabled)
    }

    /// v2: Cancel the staged high-impact PAM toggle. The switch reverts to
    /// its previous (disabled) state on the next render.
    pub fn cancel_pam_toggle(&mut self) -> &PresenterModel {
        self.model.pending_pam_toggle = None;
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
            ManagerError::Denied(_) => "Authentication denied".into(),
            ManagerError::Challenge(_) => "Authentication required".into(),
            ManagerError::NotFound(_) => "Not found".into(),
            ManagerError::Exists(_) => "Already exists".into(),
            ManagerError::SessionNotFound(_) => "Session not found".into(),
            ManagerError::SessionBusy(_) => "Session busy".into(),
            ManagerError::State(_) => "Operation failed".into(),
            ManagerError::IncompatibleVersion(_) => "Version mismatch".into(),
            ManagerError::Unsupported(_) => "Unsupported".into(),
            ManagerError::Rollback(_) => "Rollback".into(),
            ManagerError::Conflict(_) => "Conflict".into(),
            ManagerError::Transport(_) => "Connection error".into(),
        };
        self.model.error = Some(ErrorView {
            title,
            detail: err.to_string(),
            recovery,
        });
    }
}

impl<C: ManagerClient + Send + Sync + 'static> ProfilePresenter<C> {
    /// Dispatch a blocking action on the glib thread pool so the GTK main
    /// loop never blocks on a D-Bus round-trip. The `action` closure runs
    /// on a worker thread; it receives a cloned `Arc<C>` and should send
    /// its result back to the main thread via a `glib::MainContext::channel`
    /// set up by the view. The join handle is stored in `pending_task` for
    /// cancellation via [`Self::cancel_pending`].
    ///
    /// **Note:** This method requires a running `glib::MainContext` (i.e.
    /// the GTK application is active). It is not used in unit tests, which
    /// call the synchronous methods directly.
    pub fn dispatch_async<F>(&mut self, action: F)
    where
        F: FnOnce(Arc<C>) + Send + 'static,
    {
        // Abort any previous in-flight task before starting a new one.
        self.cancel_pending();
        let client = Arc::clone(&self.client);
        self.model.is_loading = true;
        let handle = glib::spawn_future(async move {
            action(client);
        });
        self.pending_task = Some(handle);
    }

    /// Mark the async task as complete and clear the loading flag. Called
    /// from the view's idle callback after the background result has been
    /// applied to the model.
    pub fn mark_async_complete(&mut self) {
        self.pending_task = None;
        self.model.is_loading = false;
    }
}

/// Convenience: run a presenter action and return the resulting model clone.
/// Used by tests to snapshot state without borrowing the presenter.
pub fn after<C: ManagerClient + Sized>(
    presenter: &mut ProfilePresenter<C>,
    action: impl FnOnce(&mut ProfilePresenter<C>),
) -> PresenterModel {
    action(presenter);
    presenter.model().clone()
}
