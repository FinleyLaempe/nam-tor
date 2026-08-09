// SPDX-License-Identifier: GPL-3.0-only
//! KCM bridge: the Rust backend a thin C++ `KCModule` shim exposes to QML.
//!
//! Every operation routes through the [`Manager`] D-Bus proxy
//! (`org.namtor.Manager` on the system bus). The bridge holds no template
//! store handle, no socket, and no biometric state — it is fully unprivileged
//! and fail-closed: a D-Bus error becomes [`KcmError::DaemonUnavailable`] or
//! [`KcmError::Call`], never a silent fallback.
//!
//! The C++ shim (`namtor_kcm.cpp`) independently implements the same contract
//! via Qt D-Bus async (`QDBusPendingCall` + watcher) so the GUI thread never
//! blocks. The Rust bridge is the single source of truth for the wire types
//! and is exercised by the hermetic contract suite in [`crate::contract`].

use zbus::{connection::Connection, proxy::CacheProperties};

use crate::proxy::ManagerProxy;
use crate::types::{
    EnrollOutcome, EnrollmentProgress, KcmError, ProfileInfo, ReviewOutcome, StatusReport,
};

/// Well-known D-Bus name and object path of the manager service (Task 8).
pub const MANAGER_BUS_NAME: &str = "org.namtor.Manager";
pub const MANAGER_OBJECT_PATH: &str = "/org/namtor/Manager";

/// Connection to the `org.namtor.Manager` service.
///
/// Constructed for production with [`KcmBridge::connect`] (system bus), or
/// for tests with [`KcmBridge::new`] against an injected connection (e.g. a
/// p2p socket pair driving a fake service, see [`crate::fake`]).
pub struct KcmBridge {
    proxy: ManagerProxy<'static>,
}

impl KcmBridge {
    /// Production constructor: connect to the system bus and bind a proxy to
    /// `org.namtor.Manager` at the canonical object path.
    pub async fn connect() -> Result<Self, KcmError> {
        let conn = Connection::system().await.map_err(KcmError::from)?;
        Self::new(&conn).await
    }

    /// Test / embedded constructor: build the proxy against an existing
    /// connection. The destination is always the well-known manager name so
    /// the same code path serves session-bus and p2p transports.
    pub async fn new(conn: &Connection) -> Result<Self, KcmError> {
        let proxy = ManagerProxy::builder(conn)
            .destination(MANAGER_BUS_NAME)
            .map_err(KcmError::from)?
            .path(MANAGER_OBJECT_PATH)
            .map_err(KcmError::from)?
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .map_err(KcmError::from)?;
        Ok(Self { proxy })
    }

    /// `ListProfiles` → [`ProfileInfo`] list for the QML model.
    pub async fn list_profiles(&self) -> Result<Vec<ProfileInfo>, KcmError> {
        self.proxy.list_profiles().await.map_err(KcmError::from)
    }

    /// `StartEnrollment(uid, name) -> session_id`. Returns the fresh
    /// enrollment session id the daemon allocated.
    pub async fn start_enrollment(&self, uid: u32, name: &str) -> Result<String, KcmError> {
        self.proxy
            .start_enrollment(uid, name)
            .await
            .map_err(KcmError::from)
    }

    /// `CancelEnrollment(session)`. Best-effort: the daemon cancels capture
    /// and discards reservoir.
    pub async fn cancel_enrollment(&self, session: &str) -> Result<(), KcmError> {
        self.proxy
            .cancel_enrollment(session)
            .await
            .map_err(KcmError::from)
    }

    /// `CommitProfile(session) -> profile_id`. Commits the enrollment
    /// templates and returns the new profile id.
    pub async fn commit_profile(&self, session: &str) -> Result<String, KcmError> {
        self.proxy
            .commit_profile(session)
            .await
            .map_err(KcmError::from)
    }

    /// `ReviewProfile(uid, id) -> ReviewOutcome`. Coarse re-review of an
    /// existing profile: quality bucket + needs_reenroll flag. No score.
    pub async fn review_profile(&self, uid: u32, id: &str) -> Result<ReviewOutcome, KcmError> {
        self.proxy
            .review_profile(uid, id)
            .await
            .map_err(KcmError::from)
    }

    pub async fn rename_profile(&self, uid: u32, id: &str, name: &str) -> Result<(), KcmError> {
        self.proxy
            .rename_profile(uid, id, name)
            .await
            .map_err(KcmError::from)
    }

    pub async fn enable_profile(&self, uid: u32, id: &str, enabled: bool) -> Result<(), KcmError> {
        self.proxy
            .enable_profile(uid, id, enabled)
            .await
            .map_err(KcmError::from)
    }

    pub async fn delete_profile(&self, uid: u32, id: &str) -> Result<(), KcmError> {
        self.proxy
            .delete_profile(uid, id)
            .await
            .map_err(KcmError::from)
    }

    pub async fn merge_profiles(&self, uid: u32, src: &str, dst: &str) -> Result<(), KcmError> {
        self.proxy
            .merge_profiles(uid, src, dst)
            .await
            .map_err(KcmError::from)
    }

    pub async fn replace_profile(&self, uid: u32, id: &str, name: &str) -> Result<(), KcmError> {
        self.proxy
            .replace_profile(uid, id, name)
            .await
            .map_err(KcmError::from)
    }

    /// `GetStatus() -> StatusReport`. Aggregated daemon/camera state for the
    /// KCM header.
    pub async fn get_status(&self) -> Result<StatusReport, KcmError> {
        self.proxy.get_status().await.map_err(KcmError::from)
    }

    /// `ApiVersion` property. Matches `contract::API_VERSION` on the daemon.
    pub async fn api_version(&self) -> Result<u32, KcmError> {
        self.proxy.api_version().await.map_err(KcmError::from)
    }
}

/// QML-facing list model wrapper. Holds the last fetched profile snapshot so
/// QML can render without touching the bridge; call [`ProfileList::refresh`]
/// to reload. No biometric data lives here — [`ProfileInfo`] is metadata only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileList {
    profiles: Vec<ProfileInfo>,
}

impl ProfileList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&ProfileInfo> {
        self.profiles.get(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ProfileInfo> {
        self.profiles.iter()
    }

    /// Reload from the bridge. Returns the count for convenience.
    pub async fn refresh(&mut self, bridge: &KcmBridge) -> Result<usize, KcmError> {
        self.profiles = bridge.list_profiles().await?;
        Ok(self.profiles.len())
    }

    /// Sorted, stable view for QML (by name). Does not re-fetch.
    pub fn sorted_by_name(&self) -> Vec<&ProfileInfo> {
        let mut v: Vec<&ProfileInfo> = self.profiles.iter().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }
}

/// State machine the enrollment wizard binds to. Transitions are driven by
/// [`EnrollmentController`] calls; no frames or scores are stored here.
///
/// With the frozen contract (Task 8), progress is signal-only — there is no
/// `GetProgress` poll method. The controller exposes `commit` (replaces the
/// draft `finish`) and tracks the last progress signal received via
/// [`EnrollmentController::set_progress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentState {
    Idle,
    Enrolling,
    Cancelling,
    Finished(EnrollOutcome),
}

/// Drives a single enrollment session through the bridge. The QML wizard owns
/// one controller per enrollment attempt; `cancel` is idempotent so a stray
/// back-button press cannot corrupt daemon state.
///
/// Stale-response sequencing: the controller tracks a `generation` counter.
/// When the QML wizard restarts enrollment (e.g. after a daemon restart), it
/// creates a new controller — the old controller's in-flight responses are
/// ignored because the new controller has a different session id.
#[derive(Debug, Clone)]
pub struct EnrollmentController {
    session_id: String,
    uid: u32,
    state: EnrollmentState,
    last_progress: Option<EnrollmentProgress>,
}

impl EnrollmentController {
    pub fn new(session_id: String, uid: u32) -> Self {
        Self {
            session_id,
            uid,
            state: EnrollmentState::Idle,
            last_progress: None,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn uid(&self) -> u32 {
        self.uid
    }

    pub fn state(&self) -> EnrollmentState {
        self.state
    }

    pub fn last_progress(&self) -> Option<&EnrollmentProgress> {
        self.last_progress.as_ref()
    }

    /// Update progress from a signal. Only accepts signals for this
    /// controller's session — signals from a stale/old session are ignored
    /// (stale-response sequencing).
    pub fn set_progress(&mut self, progress: &EnrollmentProgress) -> bool {
        if progress.session_id != self.session_id {
            return false;
        }
        if !matches!(self.state, EnrollmentState::Enrolling) {
            return false;
        }
        self.last_progress = Some(progress.clone());
        true
    }

    pub async fn start(&mut self, bridge: &KcmBridge) -> Result<(), KcmError> {
        bridge.start_enrollment(self.uid, "").await?;
        self.state = EnrollmentState::Enrolling;
        Ok(())
    }

    /// `CommitProfile` — commits the enrollment and returns the new profile
    /// id. Transitions to `Finished(Success)` on success.
    pub async fn commit(&mut self, bridge: &KcmBridge) -> Result<String, KcmError> {
        let profile_id = bridge.commit_profile(&self.session_id).await?;
        self.state = EnrollmentState::Finished(EnrollOutcome::Success);
        Ok(profile_id)
    }

    pub async fn cancel(&mut self, bridge: &KcmBridge) -> Result<(), KcmError> {
        if matches!(self.state, EnrollmentState::Finished(_)) {
            return Ok(());
        }
        self.state = EnrollmentState::Cancelling;
        // Daemon cancellation is best-effort: a transient D-Bus error here
        // must not strand the controller in Cancelling. Fall through to the
        // terminal state regardless so QML can leave the wizard.
        let _ = bridge.cancel_enrollment(&self.session_id).await;
        self.state = EnrollmentState::Finished(EnrollOutcome::Cancelled);
        Ok(())
    }

    /// Test-only helper to force the controller into a given state without
    /// a live bridge. Used by the stale-progress test to set up the
    /// `Enrolling` state without a D-Bus round-trip.
    #[cfg(test)]
    pub fn set_state_for_test(&mut self, state: EnrollmentState) {
        self.state = state;
    }
}
