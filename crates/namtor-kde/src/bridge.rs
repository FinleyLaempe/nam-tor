// SPDX-License-Identifier: GPL-3.0-only
//! KCM bridge: the Rust backend a thin C++ `KCModule` shim exposes to QML.
//!
//! Every operation routes through the [`Manager`] D-Bus proxy
//! (`org.namtor.Manager` on the session bus). The bridge holds no template
//! store handle, no socket, and no biometric state — it is fully unprivileged
//! and fail-closed: a D-Bus error becomes [`KcmError::DaemonUnavailable`] or
//! [`KcmError::Call`], never a silent fallback.

use zbus::{connection::Connection, proxy::CacheProperties};

use crate::proxy::ManagerProxy;
use crate::types::{
    EnrollOutcome, EnrollmentProgress, FinishResult, KcmError, ProfileInfo, StatusInfo,
};

/// Well-known D-Bus name and object path of the manager service (Task 25).
pub const MANAGER_BUS_NAME: &str = "org.namtor.Manager";
pub const MANAGER_OBJECT_PATH: &str = "/org/namtor/Manager";

/// Connection to the `org.namtor.Manager` service.
///
/// Constructed for production with [`KcmBridge::connect`] (session bus), or
/// for tests with [`KcmBridge::new`] against an injected connection (e.g. a
/// p2p socket pair driving a fake service, see [`crate::fake`]).
pub struct KcmBridge {
    proxy: ManagerProxy<'static>,
}

impl KcmBridge {
    /// Production constructor: connect to the session bus and bind a proxy to
    /// `org.namtor.Manager` at the canonical object path.
    pub async fn connect() -> Result<Self, KcmError> {
        let conn = Connection::session().await.map_err(KcmError::from)?;
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

    /// `CreateProfile(name) -> session_id`. Returns the fresh enrollment
    /// session id the daemon allocated for the new profile.
    pub async fn create_profile(&self, name: &str) -> Result<String, KcmError> {
        self.proxy
            .create_profile(name)
            .await
            .map_err(KcmError::from)
    }

    pub async fn start_enrollment(&self, session_id: &str) -> Result<(), KcmError> {
        self.proxy
            .start_enrollment(session_id)
            .await
            .map_err(KcmError::from)
    }

    pub async fn get_progress(&self, session_id: &str) -> Result<EnrollmentProgress, KcmError> {
        self.proxy
            .get_progress(session_id)
            .await
            .map_err(KcmError::from)
    }

    pub async fn finish_enrollment(&self, session_id: &str) -> Result<FinishResult, KcmError> {
        self.proxy
            .finish_enrollment(session_id)
            .await
            .map_err(KcmError::from)
    }

    pub async fn cancel_enrollment(&self, session_id: &str) -> Result<(), KcmError> {
        self.proxy
            .cancel_enrollment(session_id)
            .await
            .map_err(KcmError::from)
    }

    pub async fn merge_profiles(&self, src: &str, dst: &str) -> Result<(), KcmError> {
        self.proxy
            .merge_profiles(src, dst)
            .await
            .map_err(KcmError::from)
    }

    pub async fn replace_profile(&self, id: &str, name: &str) -> Result<(), KcmError> {
        self.proxy
            .replace_profile(id, name)
            .await
            .map_err(KcmError::from)
    }

    pub async fn rename_profile(&self, id: &str, new_name: &str) -> Result<(), KcmError> {
        self.proxy
            .rename_profile(id, new_name)
            .await
            .map_err(KcmError::from)
    }

    pub async fn enable_profile(&self, id: &str, enabled: bool) -> Result<(), KcmError> {
        self.proxy
            .enable_profile(id, enabled)
            .await
            .map_err(KcmError::from)
    }

    pub async fn delete_profile(&self, id: &str) -> Result<(), KcmError> {
        self.proxy.delete_profile(id).await.map_err(KcmError::from)
    }

    pub async fn status(&self) -> Result<StatusInfo, KcmError> {
        self.proxy.status().await.map_err(KcmError::from)
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
#[derive(Debug, Clone)]
pub struct EnrollmentController {
    session_id: String,
    state: EnrollmentState,
    last_progress: Option<EnrollmentProgress>,
}

impl EnrollmentController {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            state: EnrollmentState::Idle,
            last_progress: None,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn state(&self) -> EnrollmentState {
        self.state
    }

    pub fn last_progress(&self) -> Option<&EnrollmentProgress> {
        self.last_progress.as_ref()
    }

    pub async fn start(&mut self, bridge: &KcmBridge) -> Result<(), KcmError> {
        bridge.start_enrollment(&self.session_id).await?;
        self.state = EnrollmentState::Enrolling;
        Ok(())
    }

    pub async fn poll_progress(
        &mut self,
        bridge: &KcmBridge,
    ) -> Result<EnrollmentProgress, KcmError> {
        let p = bridge.get_progress(&self.session_id).await?;
        self.last_progress = Some(p.clone());
        Ok(p)
    }

    pub async fn finish(&mut self, bridge: &KcmBridge) -> Result<EnrollOutcome, KcmError> {
        let result = bridge.finish_enrollment(&self.session_id).await?;
        let outcome = if result.success {
            EnrollOutcome::Success
        } else {
            EnrollOutcome::Failed
        };
        self.state = EnrollmentState::Finished(outcome);
        Ok(outcome)
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
}
