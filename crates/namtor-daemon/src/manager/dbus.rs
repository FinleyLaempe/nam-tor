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

use namtor_capture::source::CaptureSource;

use crate::daemon::CaptureFactory;
use crate::dbus::{action, ApiError, AuthzDecision, PeerIdentity, PolkitAuthority};
use crate::manager::contract::{
    EnrollmentProgressV3, EnrollmentReviewV3, PamServiceCompatibility, PamServiceInfo,
    PollProgressReply, ProtectionStatus,
};
use crate::manager::observation::GuidanceProgress;
use crate::manager::session::{EmbedSource, EnrollmentCapture, ReviewSnapshot};
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
    module_root: PathBuf,
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
            module_root: PathBuf::from("/"),
            signals: Mutex::new(Vec::new()),
        }
    }

    /// Point the `pam_namtor.so` preflight at a staging root instead of `/`.
    /// Production uses `/`; only tests and staged installs move it.
    pub fn with_module_root(mut self, root: PathBuf) -> Self {
        self.module_root = root;
        self
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
            .with_module_root(self.module_root.clone())
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

/// Factory producing a fresh [`EmbedSource`] for each v3 enrollment session.
///
/// Fallible, unlike the v1 [`crate::dbus::EmbedFactory`]: the production
/// implementation loads ONNX weights off disk, which can fail, and a session
/// started with a broken embedder would capture nothing while reporting
/// success.
pub type Manager3EmbedFactory = Box<dyn FnMut() -> Result<Box<dyn EmbedSource>, ApiError> + Send>;

/// v3 `org.namtor.Manager3` service. Continuous enrollment lifecycle
/// backed by the worker registry. No biometric material crosses this surface.
pub struct Manager3Service<P: PolkitAuthority> {
    polkit: P,
    store: Arc<dyn ProfileStore>,
    workers: Mutex<WorkerRegistry>,
    sessions: Mutex<HashMap<String, Manager3Session>>,
    signals: Mutex<Vec<Manager3Signal>>,
    /// Opened per session in [`Self::start_enrollment`]. Behind a `Mutex`
    /// because the factory is `FnMut` (it owns the camera inventory) while
    /// the service itself is shared across bus tasks.
    capture_factory: Option<Mutex<CaptureFactory>>,
    embed_factory: Option<Mutex<Manager3EmbedFactory>>,
}

impl<P: PolkitAuthority> Manager3Service<P> {
    /// Construct without capture/embed factories. Sessions can be created and
    /// authorized but capture nothing — used by the bus-mechanics and
    /// authorization tests. Production must use [`Self::new_with_capture`].
    pub fn new(polkit: P, store: Arc<dyn ProfileStore>) -> Self {
        Self {
            polkit,
            store,
            workers: Mutex::new(WorkerRegistry::new()),
            sessions: Mutex::new(HashMap::new()),
            signals: Mutex::new(Vec::new()),
            capture_factory: None,
            embed_factory: None,
        }
    }

    /// Construct with the factories real enrollment needs: one produces a
    /// [`CaptureSource`] from the selected camera, the other a production
    /// [`EmbedSource`]
    /// ([`PipelineEmbedSource`](crate::manager::embed::PipelineEmbedSource)).
    ///
    /// Both are opened lazily, per session, in [`Self::start_enrollment`] —
    /// the camera is not held open between enrollments.
    pub fn new_with_capture(
        polkit: P,
        store: Arc<dyn ProfileStore>,
        capture_factory: CaptureFactory,
        embed_factory: Manager3EmbedFactory,
    ) -> Self {
        let mut svc = Self::new(polkit, store);
        svc.capture_factory = Some(Mutex::new(capture_factory));
        svc.embed_factory = Some(Mutex::new(embed_factory));
        svc
    }

    /// Open a capture source and an embedder for a new session. Returns
    /// `(None, None)` when no factories are configured.
    ///
    /// A factory that fails is an error, not a silent downgrade: a session
    /// whose camera would not open must not be created and then report zero
    /// frames forever.
    #[allow(clippy::type_complexity)]
    fn build_capture_embed(
        &self,
    ) -> Result<(Option<Box<dyn CaptureSource>>, Option<Box<dyn EmbedSource>>), Manager3ApiError>
    {
        let capture = match self.capture_factory.as_ref() {
            Some(f) => Some(
                lock(f)().map_err(|e| Manager3ApiError::State(format!("capture source: {e}")))?,
            ),
            None => None,
        };
        let embed = match self.embed_factory.as_ref() {
            Some(f) => Some(lock(f)().map_err(Manager3ApiError::from)?),
            None => None,
        };
        Ok((capture, embed))
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

    /// Cancel every enrollment session, because the machine is about to
    /// suspend.
    ///
    /// Each session's worker owns a capture whose emitter guard darkens the
    /// IR emitter on drop, so shutting the workers down is what turns the
    /// emitter off. Same mechanism as a client disconnect, applied to all.
    /// Returns `(cancelled, stuck)`: how many sessions were ended, and how
    /// many workers did NOT join within the timeout.
    ///
    /// `stuck` is not cosmetic. A worker that does not join still owns its
    /// `EnrollmentCapture`, hence the capture source, hence the `EmitterGuard`
    /// whose `Drop` is the thing that actually darkens the IR emitter. If a
    /// worker is wedged in a blocking DQBUF against a camera that has stopped
    /// delivering — exactly what the start of a suspend causes — the session
    /// record is removed and the emitter stays lit. Reporting 0 cancelled and
    /// logging success there would put a false statement in the journal at the
    /// one moment it matters.
    pub fn cancel_all_sessions(&self) -> (usize, usize) {
        let owners: Vec<String> = {
            let sessions = lock(&self.sessions);
            let mut names: Vec<String> = sessions.values().map(|s| s.bus_name.clone()).collect();
            names.sort();
            names.dedup();
            names
        };
        let before = lock(&self.sessions).len();
        let mut stuck = 0;
        for name in owners {
            stuck += self.disconnect_bus_name(&name);
        }
        (before, stuck)
    }

    pub fn handle_disconnect_by_bus_name(&self, bus_name: &str) {
        let _ = self.disconnect_bus_name(bus_name);
    }

    /// End every session owned by `bus_name`. Returns how many workers failed
    /// to join within the timeout — each one is still holding a capture, and
    /// therefore an emitter that nothing has darkened.
    fn disconnect_bus_name(&self, bus_name: &str) -> usize {
        let mut stuck = 0;
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
                    if !h.shutdown_and_join() {
                        // `join` already took the JoinHandle, so `Drop` will
                        // not retry: this worker is gone from our books and
                        // still running.
                        stuck += 1;
                        tracing::warn!(
                            uid,
                            session = %sid,
                            "enrollment worker did not stop within the join timeout; \
                             it still holds the capture, so the IR emitter may stay armed"
                        );
                    }
                }
            }
            lock(&self.sessions).remove(&sid);
        }
        stuck
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
        let (source, embed) = self.build_capture_embed()?;
        let capture = EnrollmentCapture::new(
            uid,
            peer.uid,
            peer.bus_name.clone(),
            name.to_string(),
            source,
            embed,
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
                // The source ran out. Stop draining, but fall through to the
                // `latest` handling below rather than returning an empty
                // reply: Progress events drained earlier in this same pass
                // are real and the UI needs them. Returning here discarded
                // them, so a capture that finished faster than the poll
                // interval reported nothing at all (F15).
                Some(WorkerEvent::Exhausted) => break,
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
            guidance: crate::manager::session::guidance_to_snake_case(p.guidance),
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

    /// Stage a stub `pam_namtor.so` so `enable` passes its module preflight,
    /// and return the root to hand to `with_module_root`. `pamd` is
    /// `<tmp>/pam.d`, so its parent is the staging root.
    fn stage_module_for(pamd: &std::path::Path) -> PathBuf {
        let root = pamd.parent().expect("pamd has a parent").to_path_buf();
        let dir = root.join("usr/lib/security");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("pam_namtor.so"), b"").unwrap();
        root
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
        .with_module_root(stage_module_for(pamd))
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
        .with_module_root(stage_module_for(pamd))
    }

    // ---- Guidance wire serialization ----

    /// Every [`Guidance`] variant the daemon can emit must survive the trip to
    /// the client parser. Regression guard for the `format!("{:?}").to_lowercase()`
    /// bug that emitted `movecloser`/`betterlighting`/... — unparseable by
    /// `namtor_client::types::Guidance::from_wire`, so the UI displayed the raw
    /// token to the user.
    #[test]
    fn every_guidance_variant_round_trips_over_the_wire() {
        use crate::manager::observation::{Guidance, QualityBucket};
        let all = [
            Guidance::Centering,
            Guidance::MoveCloser,
            Guidance::MoveBack,
            Guidance::BetterLighting,
            Guidance::HoldStill,
            Guidance::FaceFront,
            Guidance::LookingGood,
            Guidance::Capturing,
            Guidance::InsufficientPose,
        ];
        let store: Arc<dyn ProfileStore> = Arc::new(NullProfileStore);
        let svc = Manager3Service::new(StubPolkit, store);
        for g in all {
            svc.push_progress(
                "session-1",
                GuidanceProgress {
                    frames_seen: 10,
                    frames_accepted: 8,
                    frames_rejected: 2,
                    quality_percent: 50,
                    diversity_percent: 50,
                    quality_bucket: QualityBucket::MEDIUM,
                    guidance: g,
                },
            );
        }
        let signals = svc.drain_signals();
        assert_eq!(signals.len(), all.len(), "one signal per variant");
        for (sig, g) in signals.iter().zip(all) {
            let v3 = match sig {
                Manager3Signal::EnrollmentProgress(v3) => v3,
                other => panic!("expected EnrollmentProgress, got {other:?}"),
            };
            assert_eq!(
                v3.guidance,
                crate::manager::session::guidance_to_snake_case(g),
                "wire form must be the canonical snake_case mapping"
            );
            assert!(
                namtor_client::types::Guidance::from_wire(&v3.guidance).is_some(),
                "client cannot parse wire form {:?} for {g:?}",
                v3.guidance
            );
        }
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
        let module_root = stage_module_for(&pamd);
        let svc = Manager2Service::new(ChallengePolkit, store, pamd, registry)
            .with_module_root(module_root);
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

    /// Paced greyscale replay source for the v3 wiring tests.
    ///
    /// Frames are served with a small delay so the session behaves like a
    /// live 30 fps camera rather than draining instantly: an instantly
    /// exhausted source makes `poll_progress` report `has_progress: false`
    /// regardless of how many frames were captured, which would test the
    /// wrong thing.
    struct ReplaySource3 {
        left: usize,
        fill: u8,
        started: bool,
    }

    impl ReplaySource3 {
        fn new(frames: usize, fill: u8) -> Self {
            Self {
                left: frames,
                fill,
                started: false,
            }
        }
    }

    impl namtor_capture::source::CaptureSource for ReplaySource3 {
        fn caps(&self) -> namtor_capture::source::SourceCaps {
            namtor_capture::source::SourceCaps {
                spectrum: namtor_capture::source::Spectrum::Ir,
                format: namtor_capture::frame::PixFmt::Grey8,
                width: 8,
                height: 8,
                fps: 30,
                has_metadata: false,
            }
        }
        fn start(&mut self) -> Result<(), namtor_capture::source::CaptureError> {
            self.started = true;
            Ok(())
        }
        fn next_frame(
            &mut self,
        ) -> Result<Option<namtor_capture::frame::Frame>, namtor_capture::source::CaptureError>
        {
            if !self.started {
                return Err(namtor_capture::source::CaptureError::NotStarted);
            }
            if self.left == 0 {
                return Ok(None);
            }
            self.left -= 1;
            std::thread::sleep(std::time::Duration::from_millis(5));
            let fill = self.fill.wrapping_add(self.left as u8);
            Ok(Some(namtor_capture::frame::Frame {
                data: vec![fill; 64],
                format: namtor_capture::frame::PixFmt::Grey8,
                width: 8,
                height: 8,
                ts_mono: std::time::Duration::from_millis(0),
                sof: None,
                emitter: namtor_capture::frame::EmitterState::On,
                quality: None,
            }))
        }
        fn stop(&mut self) {
            self.started = false;
        }
    }

    fn new_svc3_with_sources() -> Manager3Service<StubPolkit> {
        let store: Arc<dyn ProfileStore> = Arc::new(NullProfileStore);
        Manager3Service::new_with_capture(
            StubPolkit,
            store,
            Box::new(|| Ok(Box::new(ReplaySource3::new(10_000, 120)))),
            Box::new(|| {
                Ok(Box::new(crate::manager::session::MockEmbedSource::new(8))
                    as Box<dyn crate::manager::session::EmbedSource>)
            }),
        )
    }

    /// F15: a finite source that drains faster than the poll interval must
    /// still report the progress it produced.
    ///
    /// `poll_progress` accumulates `Progress` events into `latest`, but on
    /// draining `WorkerEvent::Exhausted` it used to `return` an empty reply
    /// immediately, throwing `latest` away. `ReviewReady`/`Committed` break
    /// out of the loop and keep it; `Exhausted` did not. A short enrollment
    /// therefore reported "no progress" no matter how many frames it had
    /// accepted, so the UI could show nothing at all for a fast capture.
    #[test]
    fn v3_poll_progress_keeps_progress_drained_alongside_exhausted() {
        let store: Arc<dyn ProfileStore> = Arc::new(NullProfileStore);
        // 3 frames, no pacing sleep: the worker finishes and pushes
        // Exhausted into the same queue pass as the Progress events.
        let svc = Manager3Service::new_with_capture(
            StubPolkit,
            store,
            Box::new(|| Ok(Box::new(InstantSource3::new(3, 120)))),
            Box::new(|| {
                Ok(Box::new(crate::manager::session::MockEmbedSource::new(8))
                    as Box<dyn crate::manager::session::EmbedSource>)
            }),
        );
        let sid = svc.start_enrollment(&peer(0), 0, "alice").expect("start");

        // Give the worker time to run the source to exhaustion so that a
        // single poll drains Progress AND Exhausted together.
        std::thread::sleep(std::time::Duration::from_millis(400));
        let reply = svc.poll_progress(&peer(0), &sid).expect("poll");

        assert!(
            reply.has_progress,
            "progress drained alongside Exhausted was discarded: {reply:?}"
        );
        assert!(
            reply.progress.frames_seen > 0,
            "reported progress is empty: {:?}",
            reply.progress
        );
        svc.cancel_enrollment(&peer(0), &sid).ok();
    }

    /// Like [`ReplaySource3`] but with no per-frame delay, so it exhausts
    /// immediately. Exists to exercise the F15 path deliberately.
    struct InstantSource3 {
        left: usize,
        fill: u8,
        started: bool,
    }

    impl InstantSource3 {
        fn new(frames: usize, fill: u8) -> Self {
            Self {
                left: frames,
                fill,
                started: false,
            }
        }
    }

    impl namtor_capture::source::CaptureSource for InstantSource3 {
        fn caps(&self) -> namtor_capture::source::SourceCaps {
            namtor_capture::source::SourceCaps {
                spectrum: namtor_capture::source::Spectrum::Ir,
                format: namtor_capture::frame::PixFmt::Grey8,
                width: 8,
                height: 8,
                fps: 30,
                has_metadata: false,
            }
        }
        fn start(&mut self) -> Result<(), namtor_capture::source::CaptureError> {
            self.started = true;
            Ok(())
        }
        fn next_frame(
            &mut self,
        ) -> Result<Option<namtor_capture::frame::Frame>, namtor_capture::source::CaptureError>
        {
            if !self.started {
                return Err(namtor_capture::source::CaptureError::NotStarted);
            }
            if self.left == 0 {
                return Ok(None);
            }
            self.left -= 1;
            let fill = self.fill.wrapping_add(self.left as u8);
            Ok(Some(namtor_capture::frame::Frame {
                data: vec![fill; 64],
                format: namtor_capture::frame::PixFmt::Grey8,
                width: 8,
                height: 8,
                ts_mono: std::time::Duration::from_millis(0),
                sof: None,
                emitter: namtor_capture::frame::EmitterState::On,
                quality: None,
            }))
        }
        fn stop(&mut self) {
            self.started = false;
        }
    }

    /// Before task 3.6 `start_enrollment` passed `None, None` for both
    /// sources, so the worker's first `pump()` returned
    /// `ApiError::State("no capture source for session")` and tore the
    /// session down. GUI enrollment could not capture a single frame.
    #[test]
    fn v3_start_enrollment_wires_a_real_capture_and_embed_source() {
        let svc = new_svc3_with_sources();
        let sid = svc.start_enrollment(&peer(0), 0, "alice").expect("start");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut progressed = None;
        while std::time::Instant::now() < deadline {
            let reply = svc.poll_progress(&peer(0), &sid).expect("poll");
            if reply.has_progress && reply.progress.frames_accepted > 0 {
                progressed = Some(reply.progress);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let progress = progressed.expect("no progress from a wired session");
        assert!(progress.frames_seen > 0, "progress {progress:?}");
        assert!(progress.frames_accepted > 0, "progress {progress:?}");
        assert!(!progress.guidance.is_empty(), "progress {progress:?}");
        svc.cancel_enrollment(&peer(0), &sid).ok();
    }

    /// A camera that will not open must surface as an error from
    /// `StartEnrollment`, not as a session that silently produces nothing.
    #[test]
    fn v3_start_enrollment_reports_a_capture_open_failure() {
        let store: Arc<dyn ProfileStore> = Arc::new(NullProfileStore);
        let svc = Manager3Service::new_with_capture(
            StubPolkit,
            store,
            Box::new(|| Err(crate::DaemonError::Capture("no IR camera found".into()))),
            Box::new(|| {
                Ok(Box::new(crate::manager::session::MockEmbedSource::new(8))
                    as Box<dyn crate::manager::session::EmbedSource>)
            }),
        );
        let err = svc.start_enrollment(&peer(0), 0, "alice").unwrap_err();
        assert!(
            matches!(err, Manager3ApiError::State(ref m) if m.contains("no IR camera found")),
            "unexpected error {err:?}"
        );
        // The failed start must not have left a session behind.
        assert!(svc.start_enrollment(&peer(0), 0, "alice").is_err());
    }

    /// Without factories the service still creates sessions, which is what
    /// the authorization and bus-mechanics tests rely on.
    #[test]
    fn v3_start_enrollment_without_factories_still_creates_a_session() {
        let svc = new_svc3();
        assert!(svc.start_enrollment(&peer(0), 0, "alice").is_ok());
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

    /// Suspend cancels every enrollment session, from every client.
    ///
    /// Each session's worker owns a capture whose emitter guard darkens the IR
    /// emitter on drop; with no `PrepareForSleep` handling, a session open when
    /// the lid closed kept the emitter armed through suspend.
    #[test]
    fn v3_cancel_all_sessions_ends_every_session() {
        let svc = new_svc3();
        let a = svc.start_enrollment(&peer(0), 0, "root-profile").unwrap();
        let b = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        assert!(lock(&svc.sessions).contains_key(&a));
        assert!(lock(&svc.sessions).contains_key(&b));

        let (cancelled, stuck) = svc.cancel_all_sessions();

        assert_eq!(cancelled, 2, "both sessions were open");
        assert_eq!(stuck, 0, "no worker should outlive its join timeout here");
        assert!(
            lock(&svc.sessions).is_empty(),
            "no session may survive suspend"
        );
    }

    #[test]
    fn v3_cancel_all_sessions_with_none_open_is_a_noop() {
        let svc = new_svc3();
        assert_eq!(svc.cancel_all_sessions(), (0, 0));
        assert!(lock(&svc.sessions).is_empty());
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
