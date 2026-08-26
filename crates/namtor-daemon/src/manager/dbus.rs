// SPDX-License-Identifier: GPL-3.0-only
//! v2 `org.namtor.Manager2` D-Bus service (spec §9.1, plan task 11).
//!
//! PAM service inventory and toggle operations over the system bus.
//! Polkit `org.namtor.configure-pam` gates every mutating operation.
//! Forged service IDs are rejected by the transactional editor's
//! `validate_service_id`. Vanished senders fail closed (empty bus_name
//! -> Denied). No biometric material crosses this surface — asserted by
//! the privacy scan in [`crate::manager::contract`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use namtor_pam_config::{
    classify_service, disable_service, enable_service, scan_pamd, EditError, EditOptions,
    PamServiceCompatibility as PcmCompat, PamServiceInfo as PcmInfo,
};

use std::collections::HashMap;

use crate::dbus::{action, AuthzDecision, PeerIdentity, PolkitAuthority};
use crate::manager::contract::{
    EnrollmentProgressV3, EnrollmentReviewV3, PamServiceCompatibility, PamServiceInfo,
    PollProgressReply, ProtectionStatus,
};
use crate::manager::observation::GuidanceProgress;
use crate::manager::session::{EnrollmentCapture, ReviewSnapshot};
use crate::manager::store::ProfileStore;
use crate::manager::worker::{
    EventReceiver, SystemWorkerClock, WorkerCommand, WorkerEvent, WorkerRegistry,
};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// v2 API-level error. Maps to [`Manager2DBusError`] at the D-Bus boundary.
#[derive(Debug, thiserror::Error)]
pub enum PamApiError {
    #[error("denied: {action}")]
    Denied { action: String },
    #[error("challenge: {0}")]
    Challenge(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("rollback: {0}")]
    Rollback(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("state: {0}")]
    State(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// v2 D-Bus wire error. Variants map to frozen `org.namtor.Error.*` names
/// (see [`contract::error`] and [`contract::error_v2`]).
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.namtor.Error")]
pub enum Manager2DBusError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Denied(String),
    Challenge(String),
    NotFound(String),
    State(String),
    Unsupported(String),
    Rollback(String),
    Conflict(String),
}

impl From<PamApiError> for Manager2DBusError {
    fn from(e: PamApiError) -> Self {
        match e {
            PamApiError::Denied { action } => Self::Denied(action),
            PamApiError::Challenge(a) => Self::Challenge(a),
            PamApiError::NotFound(s) => Self::NotFound(s),
            PamApiError::Unsupported(s) => Self::Unsupported(s),
            PamApiError::Rollback(s) => Self::Rollback(s),
            PamApiError::Conflict(s) => Self::Conflict(s),
            PamApiError::State(s) => Self::State(s),
            PamApiError::Io(e) => Self::State(e.to_string()),
        }
    }
}

impl From<crate::dbus::ManagerDBusError> for Manager2DBusError {
    fn from(e: crate::dbus::ManagerDBusError) -> Self {
        match e {
            crate::dbus::ManagerDBusError::ZBus(e) => Self::ZBus(e),
            crate::dbus::ManagerDBusError::Denied(s) => Self::Denied(s),
            crate::dbus::ManagerDBusError::Challenge(s) => Self::Challenge(s),
            crate::dbus::ManagerDBusError::NotFound(s) => Self::NotFound(s),
            crate::dbus::ManagerDBusError::State(s) => Self::State(s),
            other => Self::State(other.to_string()),
        }
    }
}

/// v2 signals emitted by the `org.namtor.Manager2` interface.
#[derive(Debug, Clone, PartialEq)]
pub enum PamSignal {
    PamServicesChanged(Vec<PamServiceInfo>),
    ProtectionStatusChanged(ProtectionStatus),
}

/// v2 D-Bus-exposed PAM management API. Method names mirror the frozen
/// v2 contract in [`contract::method_v2`] 1:1.
pub trait PamApi {
    fn list_pam_services(&self, peer: &PeerIdentity) -> Result<Vec<PamServiceInfo>, PamApiError>;
    fn set_pam_service_enabled(
        &self,
        peer: &PeerIdentity,
        service_id: &str,
        enabled: bool,
    ) -> Result<bool, PamApiError>;
    fn get_protection_status(&self, peer: &PeerIdentity) -> Result<ProtectionStatus, PamApiError>;
}

/// Reference `org.namtor.Manager2` service backed by the transactional
/// PAM editor and the encrypted template store.
pub struct Manager2Service<P: PolkitAuthority> {
    polkit: P,
    store: Arc<dyn ProfileStore>,
    pamd_root: PathBuf,
    registry_root: PathBuf,
    signals: Mutex<Vec<PamSignal>>,
}

impl<P: PolkitAuthority> Manager2Service<P> {
    pub fn new(
        polkit: P,
        store: Arc<dyn ProfileStore>,
        pamd_root: PathBuf,
        registry_root: PathBuf,
    ) -> Self {
        Self {
            polkit,
            store,
            pamd_root,
            registry_root,
            signals: Mutex::new(Vec::new()),
        }
    }

    fn authz(&self, action_id: &str, peer: &PeerIdentity) -> Result<(), PamApiError> {
        match self.polkit.check(action_id, peer, 0) {
            AuthzDecision::Allowed => Ok(()),
            AuthzDecision::Challenge => Err(PamApiError::Challenge(action_id.into())),
            AuthzDecision::Denied => Err(PamApiError::Denied {
                action: action_id.into(),
            }),
        }
    }

    pub fn drain_signals(&self) -> Vec<PamSignal> {
        std::mem::take(&mut *lock(&self.signals))
    }

    fn edit_options(&self) -> EditOptions {
        EditOptions::new(self.pamd_root.clone(), self.registry_root.clone())
    }

    fn push_pam_services_changed(&self) {
        let services = self.scan_services();
        lock(&self.signals).push(PamSignal::PamServicesChanged(services));
    }

    fn scan_services(&self) -> Vec<PamServiceInfo> {
        let scanned = match scan_pamd(&self.pamd_root) {
            Ok(files) => files,
            Err(_) => return Vec::new(),
        };
        scanned
            .into_iter()
            .filter_map(|f| {
                classify_service(&f.id, &f.path, &self.pamd_root)
                    .ok()
                    .map(pcm_to_contract_info)
            })
            .collect()
    }
}

impl<P: PolkitAuthority> PamApi for Manager2Service<P> {
    fn list_pam_services(&self, _peer: &PeerIdentity) -> Result<Vec<PamServiceInfo>, PamApiError> {
        Ok(self.scan_services())
    }

    fn set_pam_service_enabled(
        &self,
        peer: &PeerIdentity,
        service_id: &str,
        enabled: bool,
    ) -> Result<bool, PamApiError> {
        self.authz(action::CONFIGURE_PAM, peer)?;
        let opts = self.edit_options();
        let outcome = if enabled {
            enable_service(service_id, &opts)
        } else {
            disable_service(service_id, &opts)
        };
        match outcome {
            Ok(_) => {
                self.push_pam_services_changed();
                Ok(true)
            }
            Err(e) => Err(map_edit_error(e)),
        }
    }

    fn get_protection_status(&self, _peer: &PeerIdentity) -> Result<ProtectionStatus, PamApiError> {
        Ok(self.store.protection_status())
    }
}

fn pcm_to_contract_info(info: PcmInfo) -> PamServiceInfo {
    PamServiceInfo {
        id: info.id,
        display_name: info.label,
        enabled: info.enabled,
        compatibility: pcm_to_contract_compat(info.compatibility),
        description: info.reason,
    }
}

fn pcm_to_contract_compat(c: PcmCompat) -> PamServiceCompatibility {
    match c {
        PcmCompat::Safe => PamServiceCompatibility::Safe,
        PcmCompat::Unsupported => PamServiceCompatibility::Unsupported,
        PcmCompat::Risky => PamServiceCompatibility::Risky,
    }
}

/// Map a transactional-editor [`EditError`] to a v2 [`PamApiError`].
/// Forged IDs -> `Unsupported`; stale external edits -> `Conflict`;
/// checksum/crash failures -> `Rollback`; unsafe stack shapes ->
/// `Unsupported`; I/O and registry failures -> `State`.
fn map_edit_error(e: EditError) -> PamApiError {
    match e {
        EditError::InvalidServiceId(s) => {
            PamApiError::Unsupported(format!("invalid service id: {s}"))
        }
        EditError::SymlinkRejected(s) => PamApiError::Unsupported(format!("symlink rejected: {s}")),
        EditError::TargetMissing(s) => PamApiError::NotFound(s),
        EditError::NoAuthLines(_)
        | EditError::NoPasswordFallback(_)
        | EditError::DenyBeforeBlock(_)
        | EditError::DenyBeforePassword(_)
        | EditError::UnsafeControlField(_)
        | EditError::ModuleNotFound => PamApiError::Unsupported(e.to_string()),
        EditError::BackupChecksumMismatch(s) => PamApiError::Rollback(s),
        EditError::StaleEdit(s) => PamApiError::Conflict(s),
        EditError::CrashInjected(s) => PamApiError::Rollback(s),
        EditError::Io(s) => PamApiError::State(format!("io: {s}")),
        EditError::Registry(s) => PamApiError::State(format!("registry: {s}")),
    }
}

// ---------------------------------------------------------------------------
// v3 interface: org.namtor.Manager3
// ---------------------------------------------------------------------------

/// v3 D-Bus wire error.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.namtor.Error")]
pub enum Manager3DBusError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Denied(String),
    Challenge(String),
    NotFound(String),
    State(String),
    SessionBusy(String),
    SessionNotFound(String),
    IncompatibleVersion(String),
}

/// v3 API-level error.
#[derive(Debug, thiserror::Error)]
pub enum Manager3ApiError {
    #[error("denied: {action}")]
    Denied { action: String },
    #[error("challenge: {0}")]
    Challenge(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("state: {0}")]
    State(String),
    #[error("session busy: uid {0}")]
    SessionBusy(u32),
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("incompatible version: requested {requested}, supported {supported}")]
    IncompatibleVersion { requested: u32, supported: u32 },
}

impl From<Manager3ApiError> for Manager3DBusError {
    fn from(e: Manager3ApiError) -> Self {
        match e {
            Manager3ApiError::Denied { action } => Self::Denied(action),
            Manager3ApiError::Challenge(a) => Self::Challenge(a),
            Manager3ApiError::NotFound(s) => Self::NotFound(s),
            Manager3ApiError::State(s) => Self::State(s),
            Manager3ApiError::SessionBusy(u) => Self::SessionBusy(format!("uid {u}")),
            Manager3ApiError::SessionNotFound(s) => Self::SessionNotFound(s),
            Manager3ApiError::IncompatibleVersion {
                requested,
                supported,
            } => Self::IncompatibleVersion(format!("requested {requested}, supported {supported}")),
        }
    }
}

impl From<crate::dbus::ApiError> for Manager3ApiError {
    fn from(e: crate::dbus::ApiError) -> Self {
        match e {
            crate::dbus::ApiError::Denied { action } => Self::Denied { action },
            crate::dbus::ApiError::Challenge(a) => Self::Challenge(a),
            crate::dbus::ApiError::NotFound(s) => Self::NotFound(s),
            crate::dbus::ApiError::SessionNotFound(s) => Self::SessionNotFound(s),
            crate::dbus::ApiError::SessionBusy(u) => Self::SessionBusy(u),
            crate::dbus::ApiError::State(s) => Self::State(s),
            other => Self::State(other.to_string()),
        }
    }
}

impl From<crate::dbus::ManagerDBusError> for Manager3DBusError {
    fn from(e: crate::dbus::ManagerDBusError) -> Self {
        match e {
            crate::dbus::ManagerDBusError::ZBus(e) => Self::ZBus(e),
            crate::dbus::ManagerDBusError::Denied(s) => Self::Denied(s),
            crate::dbus::ManagerDBusError::Challenge(s) => Self::Challenge(s),
            crate::dbus::ManagerDBusError::NotFound(s) => Self::NotFound(s),
            crate::dbus::ManagerDBusError::Exists(s) => Self::State(format!("exists: {s}")),
            crate::dbus::ManagerDBusError::SessionNotFound(s) => Self::SessionNotFound(s),
            crate::dbus::ManagerDBusError::SessionBusy(s) => Self::SessionBusy(s),
            crate::dbus::ManagerDBusError::State(s) => Self::State(s),
            crate::dbus::ManagerDBusError::IncompatibleVersion(s) => Self::IncompatibleVersion(s),
        }
    }
}

/// v3 signals emitted by the `org.namtor.Manager3` interface.
#[derive(Debug, Clone)]
pub enum Manager3Signal {
    EnrollmentProgress(EnrollmentProgressV3),
    EnrollmentCompleted {
        session_id: String,
        profile_id: String,
    },
    ProfileChanged {
        uid: u32,
        id: String,
    },
    StatusChanged(crate::dbus::StatusReport),
}

/// Internal per-session state for Manager3.
struct Manager3Session {
    uid: u32,
    receiver: EventReceiver,
    bus_name: String,
    display_name: String,
}

/// v3 `org.namtor.Manager3` service. Continuous enrollment lifecycle
/// backed by the worker registry. No biometric material crosses this surface.
pub struct Manager3Service<P: PolkitAuthority> {
    polkit: P,
    store: Arc<dyn ProfileStore>,
    workers: Mutex<WorkerRegistry>,
    sessions: Mutex<HashMap<String, Manager3Session>>,
    signals: Mutex<Vec<Manager3Signal>>,
}

impl<P: PolkitAuthority> Manager3Service<P> {
    pub fn new(polkit: P, store: Arc<dyn ProfileStore>) -> Self {
        Self {
            polkit,
            store,
            workers: Mutex::new(WorkerRegistry::new()),
            sessions: Mutex::new(HashMap::new()),
            signals: Mutex::new(Vec::new()),
        }
    }

    pub fn drain_signals(&self) -> Vec<Manager3Signal> {
        std::mem::take(&mut *lock(&self.signals))
    }

    fn authz(
        &self,
        action: &str,
        peer: &PeerIdentity,
        target_uid: u32,
    ) -> Result<(), Manager3ApiError> {
        match self.polkit.check(action, peer, target_uid) {
            AuthzDecision::Allowed => Ok(()),
            AuthzDecision::Challenge => Err(Manager3ApiError::Challenge(action.into())),
            AuthzDecision::Denied => Err(Manager3ApiError::Denied {
                action: action.into(),
            }),
        }
    }

    fn gen_session_id() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(1);
        format!("sess3-{}", C.fetch_add(1, Ordering::Relaxed))
    }

    pub fn handle_disconnect_by_bus_name(&self, bus_name: &str) {
        let to_remove: Vec<String> = {
            let sessions = lock(&self.sessions);
            sessions
                .iter()
                .filter(|(_, s)| s.bus_name == bus_name)
                .map(|(k, _)| k.clone())
                .collect()
        };
        for sid in to_remove {
            let uid = {
                let sessions = lock(&self.sessions);
                sessions.get(&sid).map(|s| s.uid)
            };
            if let Some(uid) = uid {
                let mut workers = lock(&self.workers);
                let _ = workers.send_command(uid, WorkerCommand::Cancel);
                if let Some(mut h) = workers.remove(uid) {
                    h.shutdown_and_join();
                }
            }
            lock(&self.sessions).remove(&sid);
        }
    }

    pub fn start_enrollment(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        name: &str,
    ) -> Result<String, Manager3ApiError> {
        let action = if peer.uid == uid {
            action::ENROLL_SELF
        } else {
            action::ENROLL_OTHER
        };
        self.authz(action, peer, uid)?;
        let capture = EnrollmentCapture::new(
            uid,
            peer.uid,
            peer.bus_name.clone(),
            name.to_string(),
            None,
            None,
            100,
        );
        let session_id = Self::gen_session_id();
        let receiver = {
            let mut workers = lock(&self.workers);
            workers
                .start(uid, capture, SystemWorkerClock)
                .map_err(Manager3ApiError::from)?
        };
        lock(&self.sessions).insert(
            session_id.clone(),
            Manager3Session {
                uid,
                receiver,
                bus_name: peer.bus_name.clone(),
                display_name: name.to_string(),
            },
        );
        Ok(session_id)
    }

    pub fn stop_enrollment(
        &self,
        peer: &PeerIdentity,
        session: &str,
    ) -> Result<(), Manager3ApiError> {
        let uid = self.require_session(session)?;
        let action = if peer.uid == uid {
            action::ENROLL_SELF
        } else {
            action::ENROLL_OTHER
        };
        self.authz(action, peer, uid)?;
        lock(&self.workers)
            .send_command(uid, WorkerCommand::Stop)
            .map_err(Manager3ApiError::from)?;
        self.drain_until_review(session)?;
        Ok(())
    }

    pub fn resume_enrollment(
        &self,
        peer: &PeerIdentity,
        session: &str,
    ) -> Result<(), Manager3ApiError> {
        let uid = self.require_session(session)?;
        let action = if peer.uid == uid {
            action::ENROLL_SELF
        } else {
            action::ENROLL_OTHER
        };
        self.authz(action, peer, uid)?;
        lock(&self.workers)
            .send_command(uid, WorkerCommand::Resume)
            .map_err(Manager3ApiError::from)?;
        Ok(())
    }

    pub fn review_enrollment(
        &self,
        peer: &PeerIdentity,
        session: &str,
    ) -> Result<EnrollmentReviewV3, Manager3ApiError> {
        let uid = self.require_session(session)?;
        let action = if peer.uid == uid {
            action::ENROLL_SELF
        } else {
            action::ENROLL_OTHER
        };
        self.authz(action, peer, uid)?;
        lock(&self.workers)
            .send_command(uid, WorkerCommand::Review)
            .map_err(Manager3ApiError::from)?;
        let snap = self.drain_until_review(session)?;
        Ok(EnrollmentReviewV3 {
            session: session.to_string(),
            ready_to_save: snap.ready_to_save,
            quality_percent: snap.quality_percent,
            diversity_percent: snap.diversity_percent,
            frames_accepted: snap.frames_accepted,
            guidance: snap.guidance,
        })
    }

    pub fn poll_progress(
        &self,
        peer: &PeerIdentity,
        session: &str,
    ) -> Result<PollProgressReply, Manager3ApiError> {
        let uid = self.require_session(session)?;
        let action = if peer.uid == uid {
            action::ENROLL_SELF
        } else {
            action::ENROLL_OTHER
        };
        self.authz(action, peer, uid)?;
        let receiver = {
            let sessions = lock(&self.sessions);
            sessions
                .get(session)
                .map(|s| s.receiver.clone())
                .ok_or_else(|| Manager3ApiError::SessionNotFound(session.into()))?
        };
        let mut latest: Option<EnrollmentProgressV3> = None;
        loop {
            match receiver.try_recv() {
                Some(WorkerEvent::Progress(p)) => {
                    self.push_progress(session, p);
                    latest = {
                        let sigs = lock(&self.signals);
                        sigs.last().and_then(|s| {
                            if let Manager3Signal::EnrollmentProgress(v3) = s {
                                Some(v3.clone())
                            } else {
                                None
                            }
                        })
                    };
                }
                Some(WorkerEvent::Exhausted) => {
                    return Ok(PollProgressReply {
                        has_progress: false,
                        progress: empty_progress(session),
                    });
                }
                Some(WorkerEvent::Error(msg)) => {
                    self.remove_session(session, uid);
                    return Err(Manager3ApiError::State(msg));
                }
                Some(WorkerEvent::Cancelled) => {
                    self.remove_session(session, uid);
                    return Ok(PollProgressReply {
                        has_progress: false,
                        progress: empty_progress(session),
                    });
                }
                Some(WorkerEvent::ReviewReady(_)) | Some(WorkerEvent::Committed(_)) => break,
                None => break,
            }
        }
        Ok(match latest {
            Some(p) => PollProgressReply {
                has_progress: true,
                progress: p,
            },
            None => PollProgressReply {
                has_progress: false,
                progress: empty_progress(session),
            },
        })
    }

    pub fn commit_profile(
        &self,
        peer: &PeerIdentity,
        session: &str,
    ) -> Result<String, Manager3ApiError> {
        let uid = self.require_session(session)?;
        let action = if peer.uid == uid {
            action::ENROLL_SELF
        } else {
            action::ENROLL_OTHER
        };
        self.authz(action, peer, uid)?;
        lock(&self.workers)
            .send_command(uid, WorkerCommand::Commit)
            .map_err(Manager3ApiError::from)?;
        let templates = self.drain_until_committed(session)?;
        let display_name = {
            let sessions = lock(&self.sessions);
            sessions
                .get(session)
                .map(|s| s.display_name.clone())
                .unwrap_or_else(|| session.to_string())
        };
        let profile_id = format!("profile-{}", session);
        let record = namtor_store::ProfileRecord {
            profile_id: namtor_store::ProfileId(format!("profile-{}", session)),
            display_name,
            enabled: true,
            uid,
            model_id: "sface_2021dec".into(),
            created_at_us: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros() as u64)
                .unwrap_or(0),
            schema_version: 1,
            last_used_us: 0,
            extrinsics: None,
        };
        self.store
            .create_profile(uid, record, templates)
            .map_err(|e| Manager3ApiError::State(e.to_string()))?;
        lock(&self.signals).push(Manager3Signal::EnrollmentCompleted {
            session_id: session.to_string(),
            profile_id: profile_id.clone(),
        });
        self.remove_session(session, uid);
        Ok(profile_id)
    }

    pub fn cancel_enrollment(
        &self,
        peer: &PeerIdentity,
        session: &str,
    ) -> Result<(), Manager3ApiError> {
        let uid = self.require_session(session)?;
        let action = if peer.uid == uid {
            action::ENROLL_SELF
        } else {
            action::ENROLL_OTHER
        };
        self.authz(action, peer, uid)?;
        lock(&self.workers)
            .send_command(uid, WorkerCommand::Cancel)
            .map_err(Manager3ApiError::from)?;
        self.drain_until_cancelled(session)?;
        self.remove_session(session, uid);
        Ok(())
    }

    /// Look up the target uid for a live enrollment session. Returns `None`
    /// when the session does not exist. Used by the D-Bus `PollProgress`
    /// method to authorise the caller against the session owner before
    /// draining worker events (SEC-5).
    pub fn session_uid(&self, session: &str) -> Option<u32> {
        lock(&self.sessions).get(session).map(|s| s.uid)
    }

    fn require_session(&self, session: &str) -> Result<u32, Manager3ApiError> {
        lock(&self.sessions)
            .get(session)
            .map(|s| s.uid)
            .ok_or_else(|| Manager3ApiError::SessionNotFound(session.into()))
    }

    fn drain_until_review(&self, session: &str) -> Result<ReviewSnapshot, Manager3ApiError> {
        let receiver = {
            let sessions = lock(&self.sessions);
            sessions
                .get(session)
                .map(|s| s.receiver.clone())
                .ok_or_else(|| Manager3ApiError::SessionNotFound(session.into()))?
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut pushed_progress = false;
        loop {
            if let Some(ev) = receiver.recv_timeout(std::time::Duration::from_millis(100)) {
                match ev {
                    WorkerEvent::ReviewReady(snap) => return Ok(snap),
                    WorkerEvent::Progress(p) => {
                        if !pushed_progress {
                            self.push_progress(session, p);
                            pushed_progress = true;
                        }
                    }
                    WorkerEvent::Error(msg) => return Err(Manager3ApiError::State(msg)),
                    WorkerEvent::Cancelled => {
                        return Err(Manager3ApiError::State("cancelled".into()))
                    }
                    WorkerEvent::Exhausted => {}
                    WorkerEvent::Committed(_) => {
                        return Err(Manager3ApiError::State("unexpected committed".into()))
                    }
                }
            }
            if std::time::Instant::now() > deadline {
                return Err(Manager3ApiError::State("timeout waiting for review".into()));
            }
        }
    }

    fn drain_until_committed(
        &self,
        session: &str,
    ) -> Result<Vec<namtor_store::Template>, Manager3ApiError> {
        let receiver = {
            let sessions = lock(&self.sessions);
            sessions
                .get(session)
                .map(|s| s.receiver.clone())
                .ok_or_else(|| Manager3ApiError::SessionNotFound(session.into()))?
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(ev) = receiver.recv_timeout(std::time::Duration::from_millis(100)) {
                match ev {
                    WorkerEvent::Committed(t) => return Ok(t),
                    WorkerEvent::Progress(_) => continue,
                    WorkerEvent::Error(msg) => return Err(Manager3ApiError::State(msg)),
                    _ => return Err(Manager3ApiError::State("unexpected event".into())),
                }
            }
            if std::time::Instant::now() > deadline {
                return Err(Manager3ApiError::State("timeout".into()));
            }
        }
    }

    fn drain_until_cancelled(&self, session: &str) -> Result<(), Manager3ApiError> {
        let receiver = {
            let sessions = lock(&self.sessions);
            sessions
                .get(session)
                .map(|s| s.receiver.clone())
                .ok_or_else(|| Manager3ApiError::SessionNotFound(session.into()))?
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(ev) = receiver.recv_timeout(std::time::Duration::from_millis(100)) {
                match ev {
                    WorkerEvent::Cancelled => return Ok(()),
                    WorkerEvent::Progress(_) => continue,
                    WorkerEvent::Error(msg) => return Err(Manager3ApiError::State(msg)),
                    _ => return Err(Manager3ApiError::State("unexpected event".into())),
                }
            }
            if std::time::Instant::now() > deadline {
                return Err(Manager3ApiError::State("timeout".into()));
            }
        }
    }

    fn push_progress(&self, session: &str, p: GuidanceProgress) {
        let v3 = EnrollmentProgressV3 {
            session: session.to_string(),
            phase: "capturing".into(),
            quality_percent: p.quality_percent,
            diversity_percent: p.diversity_percent,
            frames_seen: p.frames_seen,
            frames_accepted: p.frames_accepted,
            frames_rejected: p.frames_rejected,
            quality_bucket: p.quality_bucket.0,
            guidance: format!("{:?}", p.guidance).to_lowercase(),
        };
        lock(&self.signals).push(Manager3Signal::EnrollmentProgress(v3));
    }

    fn remove_session(&self, session: &str, uid: u32) {
        lock(&self.sessions).remove(session);
        let mut workers = lock(&self.workers);
        if let Some(mut h) = workers.remove(uid) {
            h.shutdown_and_join();
        }
    }
}

fn empty_progress(session: &str) -> EnrollmentProgressV3 {
    EnrollmentProgressV3 {
        session: session.to_string(),
        phase: "capturing".into(),
        quality_percent: 0,
        diversity_percent: 0,
        frames_seen: 0,
        frames_accepted: 0,
        frames_rejected: 0,
        quality_bucket: 0,
        guidance: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::contract;
    use crate::manager::polkit::StubPolkit;
    use crate::manager::store::NullProfileStore;
    use namtor_pam_config::Registry;
    use std::fs;
    use tempfile::TempDir;

    fn peer(uid: u32) -> PeerIdentity {
        PeerIdentity::for_testing(uid, format!(":1.{uid}"))
    }

    fn peer_no_bus(uid: u32) -> PeerIdentity {
        PeerIdentity::for_testing(uid, String::new())
    }

    fn write_service(dir: &std::path::Path, id: &str) {
        let content = "#%PAM-1.0\nauth required pam_unix.so\naccount required pam_unix.so\npassword required pam_unix.so\nsession required pam_unix.so\n";
        fs::write(dir.join(id), content).unwrap();
    }

    fn new_svc(pamd: &std::path::Path, registry: &std::path::Path) -> Manager2Service<StubPolkit> {
        let store: Arc<dyn ProfileStore> = Arc::new(NullProfileStore);
        Manager2Service::new(
            StubPolkit,
            store,
            pamd.to_path_buf(),
            registry.to_path_buf(),
        )
    }

    fn new_svc_fail_closed(
        pamd: &std::path::Path,
        registry: &std::path::Path,
    ) -> Manager2Service<crate::manager::polkit::SystemPolkit> {
        let store: Arc<dyn ProfileStore> = Arc::new(NullProfileStore);
        Manager2Service::new(
            crate::manager::polkit::SystemPolkit::for_testing(),
            store,
            pamd.to_path_buf(),
            registry.to_path_buf(),
        )
    }

    // ---- ListPamServices ----

    #[test]
    fn list_pam_services_returns_classified_inventory() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");
        write_service(&pamd, "sudo");

        let svc = new_svc(&pamd, &registry);
        let services = svc.list_pam_services(&peer(1000)).unwrap();
        assert_eq!(services.len(), 2);
        let gdm = services.iter().find(|s| s.id == "gdm-password").unwrap();
        assert!(!gdm.enabled);
        assert_eq!(gdm.compatibility, PamServiceCompatibility::Safe);
    }

    #[test]
    fn list_pam_services_empty_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();

        let svc = new_svc(&pamd, &registry);
        let services = svc.list_pam_services(&peer(1000)).unwrap();
        assert!(services.is_empty());
    }

    #[test]
    fn list_pam_services_no_polkit_required() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc = new_svc_fail_closed(&pamd, &registry);
        let services = svc.list_pam_services(&peer(1000)).unwrap();
        assert!(!services.is_empty());
    }

    // ---- SetPamServiceEnabled: authorization ----

    #[test]
    fn set_pam_service_enabled_denied_for_non_admin() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc = new_svc(&pamd, &registry);
        let err = svc
            .set_pam_service_enabled(&peer(1000), "gdm-password", true)
            .unwrap_err();
        assert!(matches!(err, PamApiError::Denied { .. }));
    }

    #[test]
    fn set_pam_service_enabled_challenged_when_polkit_says_challenge() {
        struct ChallengePolkit;
        impl PolkitAuthority for ChallengePolkit {
            fn check(&self, _a: &str, _p: &PeerIdentity, _t: u32) -> AuthzDecision {
                AuthzDecision::Challenge
            }
        }
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let store: Arc<dyn ProfileStore> = Arc::new(NullProfileStore);
        let svc = Manager2Service::new(ChallengePolkit, store, pamd, registry);
        let err = svc
            .set_pam_service_enabled(&peer(1000), "gdm-password", true)
            .unwrap_err();
        assert!(matches!(err, PamApiError::Challenge(_)));
    }

    #[test]
    fn set_pam_service_enabled_allowed_for_root() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc = new_svc(&pamd, &registry);
        let ok = svc
            .set_pam_service_enabled(&peer(0), "gdm-password", true)
            .unwrap();
        assert!(ok);
    }

    // ---- SetPamServiceEnabled: vanished sender fails closed ----

    #[test]
    fn set_pam_service_enabled_vanished_sender_denied() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc = new_svc_fail_closed(&pamd, &registry);
        let err = svc
            .set_pam_service_enabled(&peer_no_bus(0), "gdm-password", true)
            .unwrap_err();
        assert!(
            matches!(err, PamApiError::Denied { .. }),
            "vanished sender must deny even for root, got {err:?}"
        );
    }

    #[test]
    fn set_pam_service_enabled_missing_polkitd_denied_even_for_root() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc = new_svc_fail_closed(&pamd, &registry);
        let err = svc
            .set_pam_service_enabled(&peer(0), "gdm-password", true)
            .unwrap_err();
        assert!(
            matches!(err, PamApiError::Denied { .. }),
            "missing polkitd must deny even for root, got {err:?}"
        );
    }

    // ---- SetPamServiceEnabled: forged IDs rejected ----

    #[test]
    fn set_pam_service_enabled_forged_traversal_id_rejected() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc = new_svc(&pamd, &registry);
        let err = svc
            .set_pam_service_enabled(&peer(0), "../etc/passwd", true)
            .unwrap_err();
        assert!(
            matches!(err, PamApiError::Unsupported(ref s) if s.contains("invalid service id")),
            "traversal ID must be Unsupported, got {err:?}"
        );
    }

    #[test]
    fn set_pam_service_enabled_forged_dotfile_id_rejected() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc = new_svc(&pamd, &registry);
        let err = svc
            .set_pam_service_enabled(&peer(0), ".hidden", true)
            .unwrap_err();
        assert!(matches!(err, PamApiError::Unsupported(_)));
    }

    #[test]
    fn set_pam_service_enabled_forged_slash_id_rejected() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc = new_svc(&pamd, &registry);
        let err = svc
            .set_pam_service_enabled(&peer(0), "foo/bar", true)
            .unwrap_err();
        assert!(matches!(err, PamApiError::Unsupported(_)));
    }

    #[test]
    fn set_pam_service_enabled_empty_id_rejected() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc = new_svc(&pamd, &registry);
        let err = svc.set_pam_service_enabled(&peer(0), "", true).unwrap_err();
        assert!(matches!(err, PamApiError::Unsupported(_)));
    }

    #[test]
    fn set_pam_service_enabled_nonexistent_service_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc = new_svc(&pamd, &registry);
        let err = svc
            .set_pam_service_enabled(&peer(0), "nonexistent", true)
            .unwrap_err();
        assert!(matches!(err, PamApiError::NotFound(_)));
    }

    // ---- SetPamServiceEnabled: enable/disable round-trip ----

    #[test]
    fn enable_then_disable_round_trips() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc = new_svc(&pamd, &registry);
        let root = peer(0);

        let ok = svc
            .set_pam_service_enabled(&root, "gdm-password", true)
            .unwrap();
        assert!(ok);

        let services = svc.list_pam_services(&root).unwrap();
        let gdm = services.iter().find(|s| s.id == "gdm-password").unwrap();
        assert!(gdm.enabled);

        let ok = svc
            .set_pam_service_enabled(&root, "gdm-password", false)
            .unwrap();
        assert!(ok);

        let services = svc.list_pam_services(&root).unwrap();
        let gdm = services.iter().find(|s| s.id == "gdm-password").unwrap();
        assert!(!gdm.enabled);
    }

    #[test]
    fn kde_face_enable_disable_cycle() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "kde-face");

        let svc = new_svc(&pamd, &registry);
        let root = peer(0);

        let services = svc.list_pam_services(&root).unwrap();
        let kde = services.iter().find(|s| s.id == "kde-face").unwrap();
        assert!(!kde.enabled);
        assert_eq!(kde.compatibility, PamServiceCompatibility::Safe);

        assert!(svc
            .set_pam_service_enabled(&root, "kde-face", true)
            .unwrap());
        let services = svc.list_pam_services(&root).unwrap();
        let kde = services.iter().find(|s| s.id == "kde-face").unwrap();
        assert!(kde.enabled);

        assert!(svc
            .set_pam_service_enabled(&root, "kde-face", false)
            .unwrap());
        let services = svc.list_pam_services(&root).unwrap();
        let kde = services.iter().find(|s| s.id == "kde-face").unwrap();
        assert!(!kde.enabled);
    }

    // ---- PamServicesChanged signal ----

    #[test]
    fn set_pam_service_enabled_emits_pam_services_changed() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc = new_svc(&pamd, &registry);
        svc.set_pam_service_enabled(&peer(0), "gdm-password", true)
            .unwrap();

        let signals = svc.drain_signals();
        assert_eq!(signals.len(), 1);
        match &signals[0] {
            PamSignal::PamServicesChanged(services) => {
                let gdm = services.iter().find(|s| s.id == "gdm-password").unwrap();
                assert!(gdm.enabled);
            }
            other => panic!("expected PamServicesChanged, got {other:?}"),
        }
    }

    #[test]
    fn list_pam_services_does_not_emit_signal() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc = new_svc(&pamd, &registry);
        let _ = svc.list_pam_services(&peer(1000)).unwrap();
        assert!(svc.drain_signals().is_empty());
    }

    // ---- Restart preserves registry ----

    #[test]
    fn restart_preserves_registry() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();
        write_service(&pamd, "gdm-password");

        let svc1 = new_svc(&pamd, &registry);
        svc1.set_pam_service_enabled(&peer(0), "gdm-password", true)
            .unwrap();

        let reg = Registry::load(&registry).unwrap();
        assert!(reg.is_enabled("gdm-password"));

        let svc2 = new_svc(&pamd, &registry);
        let services = svc2.list_pam_services(&peer(0)).unwrap();
        let gdm = services.iter().find(|s| s.id == "gdm-password").unwrap();
        assert!(gdm.enabled, "registry must persist across restart");
    }

    // ---- GetProtectionStatus ----

    #[test]
    fn get_protection_status_returns_store_status() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        let registry = tmp.path().join("registry");
        fs::create_dir_all(&pamd).unwrap();

        let svc = new_svc(&pamd, &registry);
        let status = svc.get_protection_status(&peer(1000)).unwrap();
        assert_eq!(status.sealing, contract::ProtectionKeyKind::Software);
        assert!(!status.warning.is_empty());
    }

    // ---- v1 interface unchanged ----

    #[test]
    fn v1_contract_constants_unchanged() {
        assert_eq!(contract::API_VERSION, 1);
        assert_eq!(contract::INTERFACE, "org.namtor.Manager");
        assert_eq!(contract::BUS_NAME, "org.namtor.Manager");
        assert_eq!(contract::OBJECT_PATH, "/org/namtor/Manager");
    }

    #[test]
    fn v2_contract_constants_unchanged() {
        assert_eq!(contract::API_VERSION_V2, 2);
        assert_eq!(contract::INTERFACE_V2, "org.namtor.Manager2");
    }

    #[test]
    fn error_mapping_covers_all_edit_errors() {
        assert!(matches!(
            map_edit_error(EditError::InvalidServiceId("x".into())),
            PamApiError::Unsupported(_)
        ));
        assert!(matches!(
            map_edit_error(EditError::SymlinkRejected("x".into())),
            PamApiError::Unsupported(_)
        ));
        assert!(matches!(
            map_edit_error(EditError::TargetMissing("x".into())),
            PamApiError::NotFound(_)
        ));
        assert!(matches!(
            map_edit_error(EditError::NoAuthLines("x".into())),
            PamApiError::Unsupported(_)
        ));
        assert!(matches!(
            map_edit_error(EditError::BackupChecksumMismatch("x".into())),
            PamApiError::Rollback(_)
        ));
        assert!(matches!(
            map_edit_error(EditError::StaleEdit("x".into())),
            PamApiError::Conflict(_)
        ));
        assert!(matches!(
            map_edit_error(EditError::CrashInjected("x".into())),
            PamApiError::Rollback(_)
        ));
        assert!(matches!(
            map_edit_error(EditError::Io("x".into())),
            PamApiError::State(_)
        ));
        assert!(matches!(
            map_edit_error(EditError::Registry("x".into())),
            PamApiError::State(_)
        ));
        assert!(matches!(
            map_edit_error(EditError::ModuleNotFound),
            PamApiError::Unsupported(_)
        ));
    }

    #[test]
    fn pcm_to_contract_info_maps_fields() {
        let pcm = PcmInfo {
            id: "gdm-password".into(),
            label: "GDM Password Login".into(),
            enabled: true,
            compatibility: PcmCompat::Safe,
            reason: "safe for insertion".into(),
        };
        let c = pcm_to_contract_info(pcm);
        assert_eq!(c.id, "gdm-password");
        assert_eq!(c.display_name, "GDM Password Login");
        assert!(c.enabled);
        assert_eq!(c.compatibility, PamServiceCompatibility::Safe);
        assert_eq!(c.description, "safe for insertion");
    }

    #[test]
    fn pcm_to_contract_compat_exhaustive() {
        assert_eq!(
            pcm_to_contract_compat(PcmCompat::Safe),
            PamServiceCompatibility::Safe
        );
        assert_eq!(
            pcm_to_contract_compat(PcmCompat::Unsupported),
            PamServiceCompatibility::Unsupported
        );
        assert_eq!(
            pcm_to_contract_compat(PcmCompat::Risky),
            PamServiceCompatibility::Risky
        );
    }

    #[test]
    fn pam_api_error_to_dbus_error_exhaustive() {
        assert!(matches!(
            Manager2DBusError::from(PamApiError::Denied { action: "x".into() }),
            Manager2DBusError::Denied(_)
        ));
        assert!(matches!(
            Manager2DBusError::from(PamApiError::Challenge("x".into())),
            Manager2DBusError::Challenge(_)
        ));
        assert!(matches!(
            Manager2DBusError::from(PamApiError::NotFound("x".into())),
            Manager2DBusError::NotFound(_)
        ));
        assert!(matches!(
            Manager2DBusError::from(PamApiError::Unsupported("x".into())),
            Manager2DBusError::Unsupported(_)
        ));
        assert!(matches!(
            Manager2DBusError::from(PamApiError::Rollback("x".into())),
            Manager2DBusError::Rollback(_)
        ));
        assert!(matches!(
            Manager2DBusError::from(PamApiError::Conflict("x".into())),
            Manager2DBusError::Conflict(_)
        ));
        assert!(matches!(
            Manager2DBusError::from(PamApiError::State("x".into())),
            Manager2DBusError::State(_)
        ));
        assert!(matches!(
            Manager2DBusError::from(PamApiError::Io(std::io::Error::other("x"))),
            Manager2DBusError::State(_)
        ));
    }

    // -----------------------------------------------------------------------
    // v3 Manager3Service tests
    // -----------------------------------------------------------------------

    fn new_svc3() -> Manager3Service<StubPolkit> {
        let store: Arc<dyn ProfileStore> = Arc::new(NullProfileStore);
        Manager3Service::new(StubPolkit, store)
    }

    #[test]
    fn v3_start_enrollment_returns_session_id() {
        let svc = new_svc3();
        let sid = svc.start_enrollment(&peer(0), 0, "test").unwrap();
        assert!(sid.starts_with("sess3-"));
    }

    #[test]
    fn v3_duplicate_session_rejected() {
        let svc = new_svc3();
        let _sid1 = svc.start_enrollment(&peer(0), 0, "test").unwrap();
        let err = svc.start_enrollment(&peer(0), 0, "test2").unwrap_err();
        assert!(matches!(err, Manager3ApiError::SessionBusy(0)));
    }

    #[test]
    fn v3_unauthorized_uid_denied() {
        let svc = new_svc3();
        // StubPolkit denies cross-user enrollment
        let err = svc.start_enrollment(&peer(1000), 1001, "test").unwrap_err();
        assert!(matches!(err, Manager3ApiError::Denied { .. }));
    }

    #[test]
    fn v3_review_unknown_session() {
        let svc = new_svc3();
        let err = svc.review_enrollment(&peer(0), "unknown").unwrap_err();
        assert!(matches!(err, Manager3ApiError::SessionNotFound(_)));
    }

    #[test]
    fn v3_cancel_unknown_session() {
        let svc = new_svc3();
        let err = svc.cancel_enrollment(&peer(0), "unknown").unwrap_err();
        assert!(matches!(err, Manager3ApiError::SessionNotFound(_)));
    }

    #[test]
    fn v3_commit_unknown_session() {
        let svc = new_svc3();
        let err = svc.commit_profile(&peer(0), "unknown").unwrap_err();
        assert!(matches!(err, Manager3ApiError::SessionNotFound(_)));
    }

    #[test]
    fn v3_stop_unknown_session() {
        let svc = new_svc3();
        let err = svc.stop_enrollment(&peer(0), "unknown").unwrap_err();
        assert!(matches!(err, Manager3ApiError::SessionNotFound(_)));
    }

    #[test]
    fn v3_resume_unknown_session() {
        let svc = new_svc3();
        let err = svc.resume_enrollment(&peer(0), "unknown").unwrap_err();
        assert!(matches!(err, Manager3ApiError::SessionNotFound(_)));
    }

    #[test]
    fn v3_negotiate_version_mismatch() {
        use crate::manager::contract::negotiate_version_v3;
        let err = negotiate_version_v3(2).unwrap_err();
        assert_eq!(err.requested, 2);
        assert_eq!(err.supported, 3);
    }

    #[test]
    fn v3_api_version_is_frozen() {
        use crate::manager::contract::API_VERSION_V3;
        assert_eq!(API_VERSION_V3, 3);
    }

    #[test]
    fn v3_handle_disconnect_by_bus_name() {
        let svc = new_svc3();
        let sid = svc.start_enrollment(&peer(0), 0, "test").unwrap();
        assert!(lock(&svc.sessions).contains_key(&sid));
        let bus_name = peer(0).bus_name;
        svc.handle_disconnect_by_bus_name(&bus_name);
        assert!(!lock(&svc.sessions).contains_key(&sid));
    }

    #[test]
    fn v3_drain_signals() {
        let svc = new_svc3();
        let _ = svc.start_enrollment(&peer(0), 0, "test");
        let signals = svc.drain_signals();
        // With no capture source, no progress signals are generated
        assert!(signals.is_empty());
    }

    // ---- session_uid lookup (SEC-5 task 5) ----

    #[test]
    fn v3_session_uid_returns_uid_for_known_session() {
        let svc = new_svc3();
        let sid = svc.start_enrollment(&peer(0), 1000, "alice").unwrap();
        assert_eq!(svc.session_uid(&sid), Some(1000));
    }

    #[test]
    fn v3_session_uid_returns_none_for_unknown_session() {
        let svc = new_svc3();
        assert_eq!(svc.session_uid("nonexistent"), None);
    }

    // ---- PollProgress peer authz (SEC-5 tasks 6-7) ----

    #[test]
    fn v3_poll_progress_own_user_passes_authz() {
        let svc = new_svc3();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let result = svc.poll_progress(&peer(1000), &sid);
        assert!(
            !matches!(result, Err(Manager3ApiError::Denied { .. })),
            "own-user poll must pass authz (not Denied), got {result:?}"
        );
    }

    #[test]
    fn v3_poll_progress_cross_user_denied() {
        let svc = new_svc3();
        let sid = svc.start_enrollment(&peer(0), 1000, "alice").unwrap();
        let err = svc.poll_progress(&peer(2000), &sid).unwrap_err();
        assert!(
            matches!(err, Manager3ApiError::Denied { .. }),
            "cross-user poll must be denied, got {err:?}"
        );
    }

    #[test]
    fn v3_poll_progress_unknown_session_not_found() {
        let svc = new_svc3();
        let err = svc.poll_progress(&peer(0), "nonexistent").unwrap_err();
        assert!(matches!(err, Manager3ApiError::SessionNotFound(_)));
    }

    #[test]
    fn v3_poll_progress_root_admin_allowed_for_other_uid() {
        let svc = new_svc3();
        let sid = svc.start_enrollment(&peer(0), 1000, "alice").unwrap();
        // Root admin auth passes (not Denied); poll returns State error
        // because new_svc3() has no capture source — the test verifies
        // auth, not capture.
        let result = svc.poll_progress(&peer(0), &sid);
        assert!(
            !matches!(result, Err(Manager3ApiError::Denied { .. })),
            "root admin should not be denied"
        );
    }

    #[test]
    fn v3_api_error_to_dbus_error_exhaustive() {
        assert!(matches!(
            Manager3DBusError::from(Manager3ApiError::Denied { action: "x".into() }),
            Manager3DBusError::Denied(_)
        ));
        assert!(matches!(
            Manager3DBusError::from(Manager3ApiError::Challenge("x".into())),
            Manager3DBusError::Challenge(_)
        ));
        assert!(matches!(
            Manager3DBusError::from(Manager3ApiError::NotFound("x".into())),
            Manager3DBusError::NotFound(_)
        ));
        assert!(matches!(
            Manager3DBusError::from(Manager3ApiError::State("x".into())),
            Manager3DBusError::State(_)
        ));
        assert!(matches!(
            Manager3DBusError::from(Manager3ApiError::SessionBusy(42)),
            Manager3DBusError::SessionBusy(_)
        ));
        assert!(matches!(
            Manager3DBusError::from(Manager3ApiError::SessionNotFound("s".into())),
            Manager3DBusError::SessionNotFound(_)
        ));
        assert!(matches!(
            Manager3DBusError::from(Manager3ApiError::IncompatibleVersion {
                requested: 2,
                supported: 3
            }),
            Manager3DBusError::IncompatibleVersion(_)
        ));
    }
}
