// SPDX-License-Identifier: GPL-3.0-only
//! The [`ManagerClient`] trait every manager backend implements.
//!
//! The GNOME presenter depends only on this trait; the real D-Bus client
//! ([`crate::dbus_client::DbusManagerClient`]) and the test fake
//! ([`crate::fake::FakeManagerClient`]) are interchangeable.
//!
//! Method names mirror the frozen `org.namtor.Manager` (v1),
//! `org.namtor.Manager2` (v2), and `org.namtor.Manager3` (v3) interfaces 1:1.
//! The `uid` parameter on v1 mutation methods is the *target* uid — for
//! self-management the UI passes the current user's uid. `ListProfiles` takes
//! no uid because the daemon resolves the caller from `SO_PEERCRED` and
//! returns only the caller's profiles.
//!
//! The `EnrollmentProgress` signal is surfaced via [`Self::poll_progress`]
//! so the trait stays synchronous and trivially testable; the live client
//! caches the latest signal emission per session.

use crate::types::{
    EnrollmentProgress, EnrollmentProgressV3, EnrollmentReviewV3, ManagerResult, PamServiceInfo,
    ProfileId, ProfileInfo, ProtectionStatus, ReviewOutcome, SessionId, StatusReport,
};

/// Trait every manager backend implements. The GNOME presenter depends only
/// on this trait; the real D-Bus client and the test fake are interchangeable.
pub trait ManagerClient {
    // v1 methods

    /// `ListProfiles` → `a(ssbtt)`. Returns only profiles owned by the
    /// caller.
    fn list_profiles(&self) -> ManagerResult<Vec<ProfileInfo>>;

    /// `StartEnrollment(uid, name) → session`. Creates and arms a session.
    fn start_enrollment(&self, uid: u32, name: &str) -> ManagerResult<SessionId>;

    /// `CancelEnrollment(session)`. Disarms; no profile is written.
    fn cancel_enrollment(&self, session: &SessionId) -> ManagerResult<()>;

    /// `CommitProfile(session) → profile`. Finalises a session into a
    /// persistent profile. Returns the new profile id.
    fn commit_profile(&self, session: &SessionId) -> ManagerResult<String>;

    /// `ReviewProfile(uid, id) → (sbs)`. Coarse re-review of an existing
    /// profile.
    fn review_profile(&self, uid: u32, id: &ProfileId) -> ManagerResult<ReviewOutcome>;

    /// `RenameProfile(uid, id, name)`. Relabel a profile.
    fn rename_profile(&self, uid: u32, id: &ProfileId, new_name: &str) -> ManagerResult<()>;

    /// `EnableProfile(uid, id, enabled)`. Toggle a profile.
    fn enable_profile(&self, uid: u32, id: &ProfileId, enabled: bool) -> ManagerResult<()>;

    /// `DeleteProfile(uid, id)`. Remove a profile.
    fn delete_profile(&self, uid: u32, id: &ProfileId) -> ManagerResult<()>;

    /// `MergeProfiles(uid, src, dst)`. Fold `src` into `dst`, remove `src`.
    fn merge_profiles(&self, uid: u32, src: &ProfileId, dst: &ProfileId) -> ManagerResult<()>;

    /// `ReplaceProfile(uid, id, name)`. Re-name an existing profile in
    /// place, typically before re-enrolling.
    fn replace_profile(&self, uid: u32, id: &ProfileId, name: &str) -> ManagerResult<()>;

    /// `GetStatus` → `(ua(ssbtt)b)`. Daemon health + profile snapshot.
    fn get_status(&self) -> ManagerResult<StatusReport>;

    /// Poll the latest cached `EnrollmentProgress` signal for the given
    /// session. Returns `Ok(None)` when no signal has arrived since the
    /// last poll.
    fn poll_progress(&self, session: &SessionId) -> ManagerResult<Option<EnrollmentProgress>>;

    // v2 methods — org.namtor.Manager2

    /// `ListPamServices` → `a(ssbss)`. Detected PAM auth services.
    fn list_pam_services(&self) -> ManagerResult<Vec<PamServiceInfo>>;

    /// `SetPamServiceEnabled(service_id, enabled) → success`. Toggle namtor
    /// for a PAM service. Requires polkit `org.namtor.configure-pam`.
    fn set_pam_service_enabled(&self, service_id: &str, enabled: bool) -> ManagerResult<bool>;

    /// `GetProtectionStatus` → `(sbs)`. Template storage protection status.
    fn get_protection_status(&self) -> ManagerResult<ProtectionStatus>;

    // v3 methods — org.namtor.Manager3

    /// v3: `StartEnrollment(uid, name) → session`. Creates and arms a v3
    /// enrollment session with the worker lifecycle.
    fn start_enrollment_v3(&self, uid: u32, name: &str) -> ManagerResult<SessionId>;

    /// v3: `StopEnrollment(session)`. Stops capture, retains reservoir,
    /// transitions to the stopped review state.
    fn stop_enrollment_v3(&self, session: &str) -> ManagerResult<()>;

    /// v3: `ResumeEnrollment(session)`. Resumes capture from the stopped
    /// state, preserving the existing reservoir.
    fn resume_enrollment_v3(&self, session: &str) -> ManagerResult<()>;

    /// v3: `ReviewEnrollment(session) → (sbuuus)`. Returns a review snapshot
    /// of the stopped enrollment.
    fn review_enrollment_v3(&self, session: &str) -> ManagerResult<EnrollmentReviewV3>;

    /// v3: `CommitProfile(session) → profile`. Finalises a v3 session into a
    /// persistent profile. Returns the new profile id.
    fn commit_profile_v3(&self, session: &str) -> ManagerResult<ProfileId>;

    /// v3: `CancelEnrollment(session)`. Disarms; no profile is written.
    fn cancel_enrollment_v3(&self, session: &str) -> ManagerResult<()>;

    /// v3: `PollProgress(session) → (has_progress, EnrollmentProgressV3)`.
    /// Polls the daemon for the latest enrollment progress. Returns
    /// `Ok(None)` when the session has no progress available.
    fn poll_progress_v3(&self, session: &str) -> ManagerResult<Option<EnrollmentProgressV3>>;

    /// v3: `NegotiateVersion(version) → agreed_version`. Handshake for the
    /// v3 interface.
    fn negotiate_v3(&self, version: u32) -> ManagerResult<u32>;
}
