// SPDX-License-Identifier: GPL-3.0-only
//! D-Bus profile API: `org.namtor.Manager` surface (spec §9.1).
//!
//! Reference in-memory implementation of the profile management API exposed
//! over D-Bus. The frozen wire contract — method/signal/property names and
//! argument signatures — lives in [`crate::manager::contract`] and is kept
//! byte-identical to `docs/evidence/dbus-contract.xml`. This module owns the
//! runtime types and an in-memory reference service; the bus service itself
//! (zbus wiring) is task 9.
//!
//! Polkit-backed authorisation gates every mutating operation; enrollment
//! sessions track emitter arming so a mid-enrollment disconnect disarms the
//! emitter without touching the template store. No biometric payload
//! (embedding, frame, template, score) ever crosses this surface — asserted
//! by the privacy scan in [`crate::manager::contract`] and the
//! `no_biometrics_in_api_structs` test here.
//
// allow: SIZE_OK — single cohesive D-Bus API surface (types + trait + in-memory
// reference impl). The frozen contract itself lives in `manager::contract`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
use crate::acl::PeerCreds;
use crate::daemon::CaptureFactory;
use crate::manager::contract;
use crate::manager::session::{EmbedSource, EnrollmentCapture, DEFAULT_MAX_RESERVOIR};
use crate::manager::store::{map_store_error, NullProfileStore, ProfileStore};
use crate::DaemonError;
use namtor_capture::source::CaptureSource;
use namtor_store::template::{ProfileEntry, ProfileId, ProfileRecord};
use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

/// API version of the `org.namtor.Manager` surface. Re-exported from the
/// frozen contract so there is a single source of truth.
pub use crate::manager::contract::API_VERSION;

/// Polkit action identifiers (spec §9.1).
pub mod action {
    pub const ENROLL_SELF: &str = "org.namtor.enroll-self";
    pub const ENROLL_OTHER: &str = "org.namtor.enroll-other";
    pub const DELETE: &str = "org.namtor.delete";
    pub const CONFIGURE: &str = "org.namtor.configure";
}

/// Model identifier stamped into every profile record at commit. The daemon
/// serves embeddings from this model; the store AAD binds uid + model_id so
/// a template enrolled under one model cannot be matched under another.
const PROFILE_MODEL_ID: &str = "sface_2021dec";

/// Coarse quality bucket derived from a scalar coverage/quality metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum QualityLabel {
    Low,
    Medium,
    High,
}

impl QualityLabel {
    pub fn from_scalar(s: f32) -> Self {
        if s < 0.4 {
            Self::Low
        } else if s < 0.75 {
            Self::Medium
        } else {
            Self::High
        }
    }
}

/// Public, biometric-free profile descriptor returned by `ListProfiles`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProfileInfo {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub created: u64,
    pub last_used: u64,
}

/// Live enrollment progress emitted as the `EnrollmentProgress` signal. Wire
/// signature `(susu)` (session_id, frames_captured, quality_label,
/// coverage_percent). `coverage_percent` is an integer 0-100; the UI must not
/// receive a fractional value it could mistake for a threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct EnrollmentProgress {
    pub session_id: String,
    pub frames_captured: u32,
    pub quality: QualityLabel,
    pub coverage_percent: u32,
}

/// Coarse outcome of `ReviewProfile` (wire `(sbs)`): the reviewed profile id,
/// a quality label, and whether re-enrollment is recommended. No score, no
/// threshold, no embedding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ReviewOutcome {
    pub profile_id: String,
    pub needs_reenroll: bool,
    pub quality: QualityLabel,
}

/// Identity of a D-Bus peer, resolved from `SO_PEERCRED` + `/proc/<pid>/cmdline`.
///
/// `bus_name` is the unique D-Bus connection name (e.g. `:1.42`) from the
/// message header — set by the bus daemon, not forgeable by the peer. It is
/// required as the polkit subject when checking authorization against the real
/// `org.freedesktop.PolicyKit1.Authority`. An empty `bus_name` means the
/// sender vanished (or the identity was built from raw socket credentials
/// without a bus context) and polkit checks fail closed (`Denied`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub uid: u32,
    pub pid: i32,
    pub cmdline: String,
    pub bus_name: String,
}

impl PeerIdentity {
    /// Build a peer identity from socket credentials. Reads `/proc/<pid>/cmdline`
    /// on a best-effort basis (Linux only); a missing or unreadable cmdline is
    /// represented as an empty string rather than an error. `bus_name` is empty
    /// — this constructor is for the PAM socket path where there is no D-Bus
    /// sender. The bus service (task 9) populates `bus_name` from the message
    /// header.
    pub fn from_creds(c: &PeerCreds) -> Self {
        let cmdline = std::fs::read(format!("/proc/{}/cmdline", c.pid))
            .ok()
            .map(|b| {
                b.split(|&x| x == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| std::str::from_utf8(s).unwrap_or("").to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        Self {
            uid: c.uid,
            pid: c.pid,
            cmdline,
            bus_name: String::new(),
        }
    }
}

/// Polkit authorisation decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthzDecision {
    Allowed,
    Challenge,
    Denied,
}

/// Abstraction over polkit so the reference impl can run under a stub in tests.
pub trait PolkitAuthority {
    fn check(&self, action: &str, peer: &PeerIdentity, target_uid: u32) -> AuthzDecision;
}

/// Real polkit authority backed by `org.freedesktop.PolicyKit1.Authority` on the
/// system bus. See [`crate::manager::polkit`] for the full implementation.
pub use crate::manager::polkit::SystemPolkit;

/// Stub polkit authority for tests that exercise D-Bus mechanics and profile
/// persistence, not real authorization. See [`crate::manager::polkit`].
pub use crate::manager::polkit::StubPolkit;

/// API-level error, mapped to `DaemonError` at the boundary.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("denied: {action}")]
    Denied { action: String },
    #[error("challenge: {0}")]
    Challenge(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("exists: {0}")]
    Exists(String),
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("uid {0} has active session")]
    SessionBusy(u32),
    #[error("state: {0}")]
    State(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<ApiError> for DaemonError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Denied { .. } | ApiError::Challenge(_) => DaemonError::Acl(e.to_string()),
            ApiError::Io(e) => DaemonError::Io(e),
            _ => DaemonError::State(e.to_string()),
        }
    }
}

/// D-Bus wire error. Variants map 1:1 to frozen `org.namtor.Error.*` names
/// (see [`contract::error`]).
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.namtor.Error")]
pub enum ManagerDBusError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Denied(String),
    Challenge(String),
    NotFound(String),
    Exists(String),
    SessionNotFound(String),
    SessionBusy(String),
    State(String),
    IncompatibleVersion(String),
}

impl From<ApiError> for ManagerDBusError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Denied { action } => Self::Denied(action),
            ApiError::Challenge(a) => Self::Challenge(a),
            ApiError::NotFound(s) => Self::NotFound(s),
            ApiError::Exists(s) => Self::Exists(s),
            ApiError::SessionNotFound(s) => Self::SessionNotFound(s),
            ApiError::SessionBusy(uid) => {
                Self::SessionBusy(format!("uid {uid} has active session"))
            }
            ApiError::State(s) => Self::State(s),
            ApiError::Io(e) => Self::State(e.to_string()),
        }
    }
}

/// Signals emitted by the Manager interface. Mirrors the frozen signal set in
/// [`crate::manager::contract::signal`].
#[derive(Debug, Clone, PartialEq)]
pub enum ManagerSignal {
    /// `EnrollmentProgress(session, progress)` — live enrollment update.
    EnrollmentProgress(EnrollmentProgress),
    /// `EnrollmentCompleted(session, profile)` — `CommitProfile` succeeded.
    EnrollmentCompleted {
        session_id: String,
        profile_id: String,
    },
    /// `ProfileChanged(uid, id)` — a profile was created/renamed/enabled/
    /// deleted/merged/replaced/reviewed.
    ProfileChanged { uid: u32, id: String },
    /// `StatusChanged(status)` — daemon status (emitter arming, profile list)
    /// changed.
    StatusChanged(StatusReport),
}

/// Snapshot returned by `Status`.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct StatusReport {
    pub api_version: u32,
    pub profiles: Vec<ProfileInfo>,
    pub emitter_armed: bool,
}

/// Factory producing a fresh [`EmbedSource`] for each enrollment session.
/// Stateless (`Fn`, not `FnMut`) so it can be shared across sessions.
pub type EmbedFactory = Box<dyn Fn() -> Box<dyn EmbedSource> + Send>;

type CaptureEmbedPair = (Option<Box<dyn CaptureSource>>, Option<Box<dyn EmbedSource>>);

/// Reference `org.namtor.Manager` service backed by a durable [`ProfileStore`].
///
/// Profile data is persisted through the encrypted template store — there is
/// no in-memory profile map. A [`Mutex<()>`] serializes all store access so
/// concurrent D-Bus calls cannot interleave a read-mutate-write cycle on the
/// same uid's container. The UI has no direct store handle; every mutation
/// goes through the [`ProfileApi`] trait.
pub struct ManagerService<P: PolkitAuthority> {
    polkit: P,
    store: Arc<dyn ProfileStore>,
    store_lock: Mutex<()>,
    sessions: Mutex<HashMap<String, EnrollmentCapture>>,
    active_per_uid: Mutex<HashMap<u32, String>>,
    emitter_armed: Mutex<bool>,
    signals: Mutex<Vec<ManagerSignal>>,
    store_gen: Mutex<u64>,
    capture_factory: Mutex<Option<CaptureFactory>>,
    embed_factory: Mutex<Option<EmbedFactory>>,
}

impl<P: PolkitAuthority> ManagerService<P> {
    /// Construct with a null store. Used by bus-service tests that exercise
    /// D-Bus mechanics, not profile persistence. Production code must use
    /// [`Self::new_with_store`] or [`Self::new_with_capture`].
    pub fn new(polkit: P) -> Self {
        Self::new_with_store(polkit, Arc::new(NullProfileStore))
    }

    /// Construct with a durable profile store but no capture/embed factory.
    /// Enrollment sessions can be created (authorization/bus tests) but
    /// produce no templates on commit. Production must use
    /// [`Self::new_with_capture`] for real enrollment.
    pub fn new_with_store(polkit: P, store: Arc<dyn ProfileStore>) -> Self {
        Self {
            polkit,
            store,
            store_lock: Mutex::new(()),
            sessions: Mutex::new(HashMap::new()),
            active_per_uid: Mutex::new(HashMap::new()),
            emitter_armed: Mutex::new(false),
            signals: Mutex::new(Vec::new()),
            store_gen: Mutex::new(0),
            capture_factory: Mutex::new(None),
            embed_factory: Mutex::new(None),
        }
    }

    /// Construct with a durable store and capture/embed factories for real
    /// enrollment. The capture factory produces a fresh [`CaptureSource`] per
    /// session (selected-camera capture); the embed factory produces a fresh
    /// [`EmbedSource`] per session (recognition pipeline or mock).
    pub fn new_with_capture(
        polkit: P,
        store: Arc<dyn ProfileStore>,
        capture_factory: CaptureFactory,
        embed_factory: EmbedFactory,
    ) -> Self {
        let svc = Self::new_with_store(polkit, store);
        *lock(&svc.capture_factory) = Some(capture_factory);
        *lock(&svc.embed_factory) = Some(embed_factory);
        svc
    }

    fn authz(&self, action: &str, peer: &PeerIdentity, target_uid: u32) -> Result<(), ApiError> {
        match self.polkit.check(action, peer, target_uid) {
            AuthzDecision::Allowed => Ok(()),
            AuthzDecision::Challenge => Err(ApiError::Challenge(action.into())),
            AuthzDecision::Denied => Err(ApiError::Denied {
                action: action.into(),
            }),
        }
    }

    fn gen_session_id() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(1);
        format!("sess-{}", C.fetch_add(1, Ordering::Relaxed))
    }

    /// Open a fresh capture source + embedder for a new enrollment session.
    /// Returns `(None, None)` when no factories are configured (bus/authz
    /// tests); the session is created but produces no templates on commit.
    fn build_capture_embed(&self) -> Result<CaptureEmbedPair, ApiError> {
        let mut cf = lock(&self.capture_factory);
        let capture = match cf.as_mut() {
            Some(f) => Some(f().map_err(|e| ApiError::State(format!("capture: {e}")))?),
            None => None,
        };
        let embed = lock(&self.embed_factory).as_ref().map(|f| f());
        Ok((capture, embed))
    }

    /// Drive one capture→embed→reservoir step for a live session and queue an
    /// `EnrollmentProgress` signal. Returns `Ok(None)` when the capture source
    /// is exhausted. Not a wire method — internal plumbing for the daemon loop
    /// / tests; the frozen contract exposes progress only as a signal.
    pub fn pump_enrollment(
        &self,
        session_id: &str,
    ) -> Result<Option<EnrollmentProgress>, ApiError> {
        let snapshot = {
            let mut sessions = lock(&self.sessions);
            let s = sessions
                .get_mut(session_id)
                .ok_or_else(|| ApiError::SessionNotFound(session_id.into()))?;
            match s.pump()? {
                Some(snap) => snap,
                None => return Ok(None),
            }
        };
        let progress = EnrollmentProgress {
            session_id: session_id.to_string(),
            frames_captured: snapshot.frames_captured,
            quality: snapshot.quality,
            coverage_percent: snapshot.coverage_percent,
        };
        lock(&self.signals).push(ManagerSignal::EnrollmentProgress(progress.clone()));
        Ok(Some(progress))
    }

    /// Drain queued signals for the transport layer to emit on the bus. Not a
    /// wire method — internal plumbing for the reference impl.
    pub fn drain_signals(&self) -> Vec<ManagerSignal> {
        std::mem::take(&mut *lock(&self.signals))
    }

    /// Disarm any enrollment session owned by the disconnected peer and lower
    /// the emitter. Not a wire method — internal plumbing invoked by the
    /// transport layer on peer disconnect.
    pub fn handle_disconnect(&self, peer: &PeerIdentity) {
        let mut sessions = lock(&self.sessions);
        let sids: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| s.peer_uid() == peer.uid)
            .map(|(k, _)| k.clone())
            .collect();
        for sid in sids {
            if let Some(mut s) = sessions.remove(&sid) {
                s.cancel();
                lock(&self.active_per_uid).remove(&s.target_uid());
            }
        }
        let was_armed = *lock(&self.emitter_armed);
        *lock(&self.emitter_armed) = false;
        if was_armed {
            self.push_status_changed();
        }
    }

    fn push_status_changed(&self) {
        let st = self.snapshot_status();
        lock(&self.signals).push(ManagerSignal::StatusChanged(st));
    }

    fn snapshot_status(&self) -> StatusReport {
        self.snapshot_status_for(0)
    }

    fn snapshot_status_for(&self, uid: u32) -> StatusReport {
        let _guard = lock(&self.store_lock);
        let list = self
            .store
            .list_profiles(uid)
            .unwrap_or_default()
            .iter()
            .map(profile_entry_to_info)
            .collect();
        StatusReport {
            api_version: API_VERSION,
            profiles: list,
            emitter_armed: *lock(&self.emitter_armed),
        }
    }
}

/// Map a stored [`ProfileEntry`] to the biometric-free [`ProfileInfo`] that
/// crosses the D-Bus surface. Microsecond timestamps are truncated to seconds.
fn profile_entry_to_info(entry: &ProfileEntry) -> ProfileInfo {
    ProfileInfo {
        id: entry.profile.profile_id.0.clone(),
        name: entry.profile.display_name.clone(),
        enabled: entry.profile.enabled,
        created: entry.profile.created_at_us / 1_000_000,
        last_used: entry.profile.last_used_us / 1_000_000,
    }
}

/// Derive a coarse [`QualityLabel`] from the stored templates' quality scores.
/// An empty gallery or disabled profile yields `Low`; otherwise the mean
/// template quality maps through [`QualityLabel::from_scalar`]. No score or
/// threshold crosses the surface — only the coarse bucket.
fn coarse_quality_for(entry: &ProfileEntry) -> QualityLabel {
    if !entry.profile.enabled || entry.templates.is_empty() {
        return QualityLabel::Low;
    }
    let avg: f32 =
        entry.templates.iter().map(|t| t.quality).sum::<f32>() / entry.templates.len() as f32;
    QualityLabel::from_scalar(avg)
}

fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// Public D-Bus-exposed profile API. Method names mirror the frozen contract
/// in [`crate::manager::contract::method`] 1:1. Methods take a `PeerIdentity`
/// so the transport layer (zbus) can resolve the caller once and pass it in.
pub trait ProfileApi {
    /// `ListProfiles` → `a(ssbtt)`. Returns only profiles owned by the caller.
    fn list_profiles(&self, peer: &PeerIdentity) -> Result<Vec<ProfileInfo>, ApiError>;
    /// `StartEnrollment(uid, name) → session`. Creates and arms a session.
    fn start_enrollment(
        &self,
        peer: &PeerIdentity,
        target_uid: u32,
        name: &str,
    ) -> Result<String, ApiError>;
    /// `CancelEnrollment(session)`. Disarms; no profile is written.
    fn cancel_enrollment(&self, peer: &PeerIdentity, session_id: &str) -> Result<(), ApiError>;
    /// `CommitProfile(session) → profile`. Finalises a session into a profile.
    fn commit_profile(&self, peer: &PeerIdentity, session_id: &str) -> Result<String, ApiError>;
    /// `ReviewProfile(uid, id) → (sbs)`. Coarse re-review of an existing
    /// profile; never returns a score or threshold.
    fn review_profile(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        id: &str,
    ) -> Result<ReviewOutcome, ApiError>;
    /// `RenameProfile(uid, id, name)`. Relabel a profile.
    fn rename_profile(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        id: &str,
        new_name: &str,
    ) -> Result<(), ApiError>;
    /// `EnableProfile(uid, id, enabled)`. Toggle a profile.
    fn enable_profile(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        id: &str,
        enabled: bool,
    ) -> Result<(), ApiError>;
    /// `DeleteProfile(uid, id)`. Remove a profile.
    fn delete_profile(&self, peer: &PeerIdentity, uid: u32, id: &str) -> Result<(), ApiError>;
    /// `MergeProfiles(uid, src, dst)`. Fold `src` into `dst`, remove `src`.
    fn merge_profiles(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        src_id: &str,
        dst_id: &str,
    ) -> Result<(), ApiError>;
    /// `ReplaceProfile(uid, id, name)`. Re-name an existing profile in place,
    /// re-running the store write path (distinct from a pure relabel).
    fn replace_profile(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        id: &str,
        new_name: &str,
    ) -> Result<(), ApiError>;
    /// `GetStatus → (ua(ssbtt)b)`. Daemon status snapshot.
    fn get_status(&self, peer: &PeerIdentity) -> Result<StatusReport, ApiError>;
}

impl<P: PolkitAuthority> ProfileApi for ManagerService<P> {
    fn list_profiles(&self, peer: &PeerIdentity) -> Result<Vec<ProfileInfo>, ApiError> {
        let _guard = lock(&self.store_lock);
        let entries = self
            .store
            .list_profiles(peer.uid)
            .map_err(map_store_error)?;
        Ok(entries.iter().map(profile_entry_to_info).collect())
    }

    fn start_enrollment(
        &self,
        peer: &PeerIdentity,
        target_uid: u32,
        name: &str,
    ) -> Result<String, ApiError> {
        let action = if peer.uid == target_uid {
            action::ENROLL_SELF
        } else {
            action::ENROLL_OTHER
        };
        self.authz(action, peer, target_uid)?;
        let mut active = lock(&self.active_per_uid);
        if active.contains_key(&target_uid) {
            return Err(ApiError::SessionBusy(target_uid));
        }
        let sid = Self::gen_session_id();
        let (capture, embed) = self.build_capture_embed()?;
        let session = EnrollmentCapture::new(
            target_uid,
            peer.uid,
            name.to_string(),
            capture,
            embed,
            DEFAULT_MAX_RESERVOIR,
        );
        active.insert(target_uid, sid.clone());
        lock(&self.sessions).insert(sid.clone(), session);
        let was_armed = *lock(&self.emitter_armed);
        *lock(&self.emitter_armed) = true;
        if !was_armed {
            self.push_status_changed();
        }
        Ok(sid)
    }

    fn cancel_enrollment(&self, _peer: &PeerIdentity, session_id: &str) -> Result<(), ApiError> {
        let disarmed = {
            let mut sessions = lock(&self.sessions);
            if let Some(mut s) = sessions.remove(session_id) {
                let uid = s.target_uid();
                s.cancel();
                lock(&self.active_per_uid).remove(&uid);
                true
            } else {
                false
            }
        };
        if disarmed {
            let was_armed = *lock(&self.emitter_armed);
            *lock(&self.emitter_armed) = false;
            if was_armed {
                self.push_status_changed();
            }
        }
        Ok(())
    }

    fn commit_profile(&self, _peer: &PeerIdentity, session_id: &str) -> Result<String, ApiError> {
        let (target_uid, name, finished) = {
            let mut sessions = lock(&self.sessions);
            let mut s = sessions
                .remove(session_id)
                .ok_or_else(|| ApiError::SessionNotFound(session_id.into()))?;
            let uid = s.target_uid();
            let name = s.name().to_string();
            // If the session has a capture source, drain it to completion so
            // start→commit without explicit pumps still produces templates.
            if let Err(e) = s.pump_until_exhausted() {
                s.cancel();
                lock(&self.active_per_uid).remove(&uid);
                let was_armed = *lock(&self.emitter_armed);
                *lock(&self.emitter_armed) = false;
                if was_armed {
                    self.push_status_changed();
                }
                return Err(e);
            }
            let finished = s.finish()?;
            (uid, name, finished)
        };
        let was_armed = *lock(&self.emitter_armed);
        *lock(&self.emitter_armed) = false;
        lock(&self.active_per_uid).remove(&target_uid);

        let profile_id = format!("profile-{}", session_id);
        let record = ProfileRecord {
            profile_id: ProfileId(profile_id.clone()),
            display_name: name,
            enabled: true,
            uid: target_uid,
            model_id: PROFILE_MODEL_ID.into(),
            created_at_us: now_us(),
            schema_version: 1,
            last_used_us: 0,
        };
        {
            let _guard = lock(&self.store_lock);
            self.store
                .create_profile(target_uid, record, finished.templates)
                .map_err(map_store_error)?;
        }
        *lock(&self.store_gen) += 1;
        let mut sigs = lock(&self.signals);
        sigs.push(ManagerSignal::EnrollmentCompleted {
            session_id: session_id.to_string(),
            profile_id: profile_id.clone(),
        });
        sigs.push(ManagerSignal::ProfileChanged {
            uid: target_uid,
            id: profile_id.clone(),
        });
        if was_armed {
            drop(sigs);
            self.push_status_changed();
        }
        Ok(profile_id)
    }

    fn review_profile(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        id: &str,
    ) -> Result<ReviewOutcome, ApiError> {
        self.authz(action::CONFIGURE, peer, uid)?;
        let pid = ProfileId(id.into());
        let _guard = lock(&self.store_lock);
        let entry = self.store.get_profile(uid, &pid).map_err(map_store_error)?;
        let quality = coarse_quality_for(&entry);
        let needs_reenroll =
            !entry.profile.enabled || entry.templates.is_empty() || quality == QualityLabel::Low;
        let outcome = ReviewOutcome {
            profile_id: id.to_string(),
            needs_reenroll,
            quality,
        };
        drop(_guard);
        lock(&self.signals).push(ManagerSignal::ProfileChanged {
            uid,
            id: id.to_string(),
        });
        *lock(&self.store_gen) += 1;
        Ok(outcome)
    }

    fn rename_profile(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        id: &str,
        new_name: &str,
    ) -> Result<(), ApiError> {
        self.authz(action::CONFIGURE, peer, uid)?;
        let pid = ProfileId(id.into());
        let _guard = lock(&self.store_lock);
        self.store
            .rename_profile(uid, &pid, new_name.to_string())
            .map_err(map_store_error)?;
        *lock(&self.store_gen) += 1;
        drop(_guard);
        lock(&self.signals).push(ManagerSignal::ProfileChanged {
            uid,
            id: id.to_string(),
        });
        Ok(())
    }

    fn enable_profile(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        id: &str,
        enabled: bool,
    ) -> Result<(), ApiError> {
        self.authz(action::CONFIGURE, peer, uid)?;
        let pid = ProfileId(id.into());
        let _guard = lock(&self.store_lock);
        self.store
            .enable_profile(uid, &pid, enabled)
            .map_err(map_store_error)?;
        *lock(&self.store_gen) += 1;
        drop(_guard);
        lock(&self.signals).push(ManagerSignal::ProfileChanged {
            uid,
            id: id.to_string(),
        });
        Ok(())
    }

    fn delete_profile(&self, peer: &PeerIdentity, uid: u32, id: &str) -> Result<(), ApiError> {
        self.authz(action::DELETE, peer, uid)?;
        let pid = ProfileId(id.into());
        let _guard = lock(&self.store_lock);
        self.store
            .delete_profile(uid, &pid)
            .map_err(map_store_error)?;
        *lock(&self.store_gen) += 1;
        drop(_guard);
        lock(&self.signals).push(ManagerSignal::ProfileChanged {
            uid,
            id: id.to_string(),
        });
        Ok(())
    }

    fn merge_profiles(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        src_id: &str,
        dst_id: &str,
    ) -> Result<(), ApiError> {
        self.authz(action::CONFIGURE, peer, uid)?;
        let src = ProfileId(src_id.into());
        let dst = ProfileId(dst_id.into());
        let _guard = lock(&self.store_lock);
        self.store
            .combine_profiles(uid, &src, &dst)
            .map_err(map_store_error)?;
        *lock(&self.store_gen) += 1;
        drop(_guard);
        lock(&self.signals).push(ManagerSignal::ProfileChanged {
            uid,
            id: dst_id.to_string(),
        });
        Ok(())
    }

    fn replace_profile(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        id: &str,
        new_name: &str,
    ) -> Result<(), ApiError> {
        self.authz(action::CONFIGURE, peer, uid)?;
        let pid = ProfileId(id.into());
        let _guard = lock(&self.store_lock);
        let entry = self.store.get_profile(uid, &pid).map_err(map_store_error)?;
        let mut updated = entry.profile;
        updated.display_name = new_name.to_string();
        self.store
            .merge_profile(uid, updated, entry.templates)
            .map_err(map_store_error)?;
        *lock(&self.store_gen) += 1;
        drop(_guard);
        lock(&self.signals).push(ManagerSignal::ProfileChanged {
            uid,
            id: id.to_string(),
        });
        Ok(())
    }

    fn get_status(&self, peer: &PeerIdentity) -> Result<StatusReport, ApiError> {
        Ok(self.snapshot_status_for(peer.uid))
    }
}

/// Static D-Bus introspection XML for the `org.namtor.Manager` interface.
/// Delegates to the frozen contract so there is a single source of truth.
pub fn introspect_xml() -> &'static str {
    contract::INTROSPECTION_XML
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::session::MockEmbedSource;
    use crate::manager::store::TemplateProfileStore;
    use namtor_capture::frame::{EmitterState, Frame, PixFmt};
    use namtor_capture::source::{CaptureError, CaptureSource, SourceCaps, Spectrum};
    use namtor_store::TemplateStore;
    use std::time::Duration;

    fn peer(uid: u32) -> PeerIdentity {
        PeerIdentity {
            uid,
            pid: 1,
            cmdline: "test".into(),
            bus_name: format!(":1.{}", uid),
        }
    }

    fn test_store() -> (tempfile::TempDir, Arc<dyn ProfileStore>) {
        let dir = tempfile::tempdir().unwrap();
        let ts = TemplateStore::open(dir.path(), &dir.path().join("key")).unwrap();
        (dir, Arc::new(TemplateProfileStore::new(Arc::new(ts))))
    }

    /// Replay capture source: serves `n` grey frames filled with `byte`, then
    /// returns `Ok(None)`. Used to drive enrollment sessions deterministically.
    struct ReplaySource {
        caps: SourceCaps,
        frames: Vec<Frame>,
        idx: usize,
        started: bool,
    }

    impl ReplaySource {
        fn new(n: usize, byte: u8) -> Self {
            let frame = Frame {
                data: vec![byte; 64],
                format: PixFmt::Grey8,
                width: 8,
                height: 8,
                ts_mono: Duration::from_millis(0),
                sof: None,
                emitter: EmitterState::On,
                quality: None,
            };
            Self {
                caps: SourceCaps {
                    spectrum: Spectrum::Ir,
                    format: PixFmt::Grey8,
                    width: 8,
                    height: 8,
                    fps: 30,
                    has_metadata: false,
                },
                frames: vec![frame; n],
                idx: 0,
                started: false,
            }
        }
    }

    impl CaptureSource for ReplaySource {
        fn caps(&self) -> SourceCaps {
            self.caps
        }
        fn start(&mut self) -> Result<(), CaptureError> {
            self.started = true;
            Ok(())
        }
        fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError> {
            if !self.started {
                return Err(CaptureError::NotStarted);
            }
            if self.idx >= self.frames.len() {
                return Ok(None);
            }
            let f = self.frames[self.idx].clone();
            self.idx += 1;
            Ok(Some(f))
        }
        fn stop(&mut self) {
            self.started = false;
        }
    }

    fn mock_capture_factory() -> CaptureFactory {
        Box::new(|| Ok(Box::new(ReplaySource::new(12, 130))))
    }

    fn mock_embed_factory() -> EmbedFactory {
        Box::new(|| Box::new(MockEmbedSource::new(8)))
    }

    fn svc() -> (tempfile::TempDir, ManagerService<StubPolkit>) {
        let (dir, store) = test_store();
        let svc = ManagerService::new_with_capture(
            StubPolkit,
            store,
            mock_capture_factory(),
            mock_embed_factory(),
        );
        (dir, svc)
    }

    #[test]
    fn self_enroll_allowed_for_own_uid() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        assert!(sid.starts_with("sess-"));
    }

    #[test]
    fn enroll_other_requires_admin() {
        let (_dir, svc) = svc();
        assert!(matches!(
            svc.start_enrollment(&peer(1000), 1001, "bob"),
            Err(ApiError::Denied { .. })
        ));
    }

    #[test]
    fn delete_requires_admin() {
        let (_dir, svc) = svc();
        assert!(matches!(
            svc.delete_profile(&peer(1000), 1000, "x"),
            Err(ApiError::Denied { .. })
        ));
        let sid = svc.start_enrollment(&peer(0), 0, "root").unwrap();
        let pid = svc.commit_profile(&peer(0), &sid).unwrap();
        svc.delete_profile(&peer(0), 0, &pid).unwrap();
    }

    #[test]
    fn configure_requires_admin() {
        let (_dir, svc) = svc();
        assert!(matches!(
            svc.rename_profile(&peer(1000), 1000, "x", "y"),
            Err(ApiError::Denied { .. })
        ));
        assert!(matches!(
            svc.enable_profile(&peer(1000), 1000, "x", false),
            Err(ApiError::Denied { .. })
        ));
        assert!(matches!(
            svc.review_profile(&peer(1000), 1000, "x"),
            Err(ApiError::Denied { .. })
        ));
    }

    #[test]
    fn disconnect_mid_enrollment_cleans_up_emitter() {
        let (_dir, svc) = svc();
        let _sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        assert!(*svc.emitter_armed.lock().unwrap());
        svc.handle_disconnect(&peer(1000));
        assert!(!*svc.emitter_armed.lock().unwrap());
    }

    #[test]
    fn disconnect_mid_enrollment_leaves_store_unchanged() {
        let (_dir, svc) = svc();
        let gen_before = *svc.store_gen.lock().unwrap();
        let _sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        svc.handle_disconnect(&peer(1000));
        let gen_after = *svc.store_gen.lock().unwrap();
        assert_eq!(gen_before, gen_after, "store generation unchanged");
        assert!(svc.list_profiles(&peer(1000)).unwrap().is_empty());
    }

    #[test]
    fn no_biometrics_in_api_structs() {
        let pi = ProfileInfo {
            id: "p".into(),
            name: "n".into(),
            enabled: true,
            created: 0,
            last_used: 0,
        };
        let ep = EnrollmentProgress {
            session_id: "s".into(),
            frames_captured: 0,
            quality: QualityLabel::High,
            coverage_percent: 100,
        };
        let ro = ReviewOutcome {
            profile_id: "p".into(),
            needs_reenroll: false,
            quality: QualityLabel::High,
        };
        let sr = StatusReport {
            api_version: 1,
            profiles: vec![pi],
            emitter_armed: false,
        };
        for (name, json) in [
            ("ep", serde_json::to_string(&ep).unwrap()),
            ("sr", serde_json::to_string(&sr).unwrap()),
            ("ro", serde_json::to_string(&ro).unwrap()),
        ] {
            for token in [
                "embedding",
                "template",
                "pixel",
                "score",
                "0x",
                "nonce",
                "key",
                "threshold",
            ] {
                assert!(
                    !json.to_lowercase().contains(token),
                    "{name} leaked `{token}`: {json}"
                );
            }
        }
        assert!(serde_json::to_string(&ep)
            .unwrap()
            .contains("frames_captured"));
    }

    #[test]
    fn status_returns_profile_list_without_biometrics() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        svc.commit_profile(&peer(1000), &sid).unwrap();
        let st = svc.get_status(&peer(1000)).unwrap();
        assert_eq!(st.profiles.len(), 1);
        assert_eq!(st.profiles[0].name, "alice");
        let json = serde_json::to_string(&st).unwrap();
        assert!(!json.contains("embedding"));
        assert!(!json.contains("template"));
    }

    #[test]
    fn commit_emits_completed_and_profile_changed_signals() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        svc.commit_profile(&peer(1000), &sid).unwrap();
        let sigs = svc.drain_signals();
        assert!(
            sigs.iter()
                .any(|s| matches!(s, ManagerSignal::EnrollmentCompleted { .. })),
            "EnrollmentCompleted emitted on commit"
        );
        assert!(
            sigs.iter()
                .any(|s| matches!(s, ManagerSignal::ProfileChanged { uid: 1000, .. })),
            "ProfileChanged emitted on commit"
        );
    }

    #[test]
    fn mutations_emit_profile_changed() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(0), 0, "root").unwrap();
        let pid = svc.commit_profile(&peer(0), &sid).unwrap();
        svc.drain_signals();

        svc.rename_profile(&peer(0), 0, &pid, "root2").unwrap();
        svc.enable_profile(&peer(0), 0, &pid, false).unwrap();
        svc.replace_profile(&peer(0), 0, &pid, "root3").unwrap();
        svc.review_profile(&peer(0), 0, &pid).unwrap();
        let sigs = svc.drain_signals();
        let changed = sigs
            .iter()
            .filter(|s| matches!(s, ManagerSignal::ProfileChanged { .. }))
            .count();
        assert_eq!(
            changed, 4,
            "rename/enable/replace/review each emit ProfileChanged"
        );
        svc.delete_profile(&peer(0), 0, &pid).unwrap();
        assert!(svc
            .drain_signals()
            .iter()
            .any(|s| matches!(s, ManagerSignal::ProfileChanged { .. })));
    }

    #[test]
    fn merge_folds_src_into_dst_and_emits_profile_changed() {
        let (_dir, svc) = svc();
        let sid_a = svc.start_enrollment(&peer(0), 0, "a").unwrap();
        let pid_a = svc.commit_profile(&peer(0), &sid_a).unwrap();
        let sid_b = svc.start_enrollment(&peer(0), 0, "b").unwrap();
        let pid_b = svc.commit_profile(&peer(0), &sid_b).unwrap();
        svc.drain_signals();

        svc.merge_profiles(&peer(0), 0, &pid_a, &pid_b).unwrap();
        let list = svc.list_profiles(&peer(0)).unwrap();
        assert_eq!(list.len(), 1, "src removed, dst remains");
        assert_eq!(list[0].id, pid_b);
        assert!(svc
            .drain_signals()
            .iter()
            .any(|s| matches!(s, ManagerSignal::ProfileChanged { id, .. } if id == &pid_b)));
    }

    #[test]
    fn cancel_disarms_without_writing_or_emitting_completed() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        assert!(*svc.emitter_armed.lock().unwrap());
        let gen_before = *svc.store_gen.lock().unwrap();
        svc.cancel_enrollment(&peer(1000), &sid).unwrap();
        assert!(!*svc.emitter_armed.lock().unwrap());
        assert_eq!(*svc.store_gen.lock().unwrap(), gen_before);
        let sigs = svc.drain_signals();
        assert!(
            !sigs
                .iter()
                .any(|s| matches!(s, ManagerSignal::EnrollmentCompleted { .. })),
            "cancel must not emit EnrollmentCompleted"
        );
    }

    #[test]
    fn review_returns_coarse_label_and_reenroll_flag() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(0), 0, "root").unwrap();
        let pid = svc.commit_profile(&peer(0), &sid).unwrap();
        svc.drain_signals();

        let out = svc.review_profile(&peer(0), 0, &pid).unwrap();
        assert_eq!(out.profile_id, pid);
        assert!(
            !out.needs_reenroll,
            "enabled profile does not need reenroll"
        );
        assert_eq!(out.quality, QualityLabel::High);

        svc.enable_profile(&peer(0), 0, &pid, false).unwrap();
        let out = svc.review_profile(&peer(0), 0, &pid).unwrap();
        assert!(out.needs_reenroll, "disabled profile needs reenroll");
    }

    #[test]
    fn start_and_cancel_round_trip_via_trait() {
        let (_dir, svc) = svc();
        let api: &dyn ProfileApi = &svc;
        let sid = api.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        api.cancel_enrollment(&peer(1000), &sid).unwrap();
        assert!(!*svc.emitter_armed.lock().unwrap());
    }

    #[test]
    fn introspection_delegates_to_frozen_contract() {
        let xml = introspect_xml();
        assert_eq!(xml, contract::INTROSPECTION_XML);
        assert!(xml.contains("org.namtor.Manager"));
        assert!(xml.contains("ListProfiles"));
        assert!(xml.contains("CommitProfile"));
        assert!(xml.contains("ReviewProfile"));
        assert!(xml.contains("GetStatus"));
        assert!(xml.contains("EnrollmentCompleted"));
        assert!(xml.contains("ProfileChanged"));
        assert!(xml.contains("StatusChanged"));
    }

    #[test]
    fn api_error_maps_to_daemon_error() {
        let io_err = std::io::Error::other("boom");
        let api: ApiError = io_err.into();
        let daemon: DaemonError = api.into();
        assert!(matches!(daemon, DaemonError::Io(_)));

        let api = ApiError::Denied { action: "x".into() };
        let daemon: DaemonError = api.into();
        assert!(matches!(daemon, DaemonError::Acl(_)));

        let api = ApiError::NotFound("p".into());
        let daemon: DaemonError = api.into();
        assert!(matches!(daemon, DaemonError::State(_)));
    }

    #[test]
    fn quality_label_buckets() {
        assert_eq!(QualityLabel::from_scalar(0.1), QualityLabel::Low);
        assert_eq!(QualityLabel::from_scalar(0.5), QualityLabel::Medium);
        assert_eq!(QualityLabel::from_scalar(0.9), QualityLabel::High);
    }

    // ---- Durable store integration tests (task 10) ----

    #[test]
    fn restart_persistence_across_manager_instances() {
        let (dir, store) = test_store();

        let svc1 = ManagerService::new_with_store(StubPolkit, store.clone());
        let sid = svc1.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let pid = svc1.commit_profile(&peer(1000), &sid).unwrap();

        drop(svc1);

        let svc2 = ManagerService::new_with_store(StubPolkit, store);
        let list = svc2.list_profiles(&peer(1000)).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, pid);
        assert_eq!(list[0].name, "alice");
        assert!(list[0].enabled);

        let _ = dir;
    }

    #[test]
    fn duplicate_profile_id_rejected() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let pid = svc.commit_profile(&peer(1000), &sid).unwrap();
        assert!(svc.list_profiles(&peer(1000)).unwrap().len() == 1);

        // Manually insert a duplicate via the store to verify Exists error
        let record = ProfileRecord {
            profile_id: ProfileId(pid.clone()),
            display_name: "dup".into(),
            enabled: true,
            uid: 1000,
            model_id: PROFILE_MODEL_ID.into(),
            created_at_us: now_us(),
            schema_version: 1,
            last_used_us: 0,
        };
        let err = svc
            .store
            .create_profile(1000, record, Vec::new())
            .unwrap_err();
        assert!(matches!(
            namtor_store::store::StoreError::from(err),
            namtor_store::store::StoreError::ProfileExists(_)
        ));
    }

    #[test]
    fn wrong_uid_isolated() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        svc.commit_profile(&peer(1000), &sid).unwrap();

        let other = svc.list_profiles(&peer(2000)).unwrap();
        assert!(other.is_empty(), "uid 2000 sees no profiles from uid 1000");
    }

    #[test]
    fn corrupt_store_errors_gracefully() {
        let (dir, store) = test_store();

        let svc1 = ManagerService::new_with_store(StubPolkit, store.clone());
        let sid = svc1.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        svc1.commit_profile(&peer(1000), &sid).unwrap();
        drop(svc1);

        // Corrupt the sealed container file
        let path = dir.path().join("1000.sealed");
        let mut data = std::fs::read(&path).unwrap();
        if data.len() > 20 {
            let last = data.len() - 1;
            data[last] ^= 0xff;
        }
        std::fs::write(&path, &data).unwrap();

        let svc2 = ManagerService::new_with_store(StubPolkit, store);
        let err = svc2.list_profiles(&peer(1000)).unwrap_err();
        assert!(
            matches!(err, ApiError::State(_) | ApiError::Io(_)),
            "corruption maps to State/Io, got {err:?}"
        );
    }

    #[test]
    fn concurrent_mutation_serialized() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(0), 0, "root").unwrap();
        let pid = svc.commit_profile(&peer(0), &sid).unwrap();

        let svc = Arc::new(svc);
        let pid_a = pid.clone();
        let svc_a = svc.clone();
        let t1 = std::thread::spawn(move || {
            for i in 0..10 {
                svc_a
                    .rename_profile(&peer(0), 0, &pid_a, &format!("name-{i}"))
                    .unwrap();
            }
        });
        let pid_b = pid.clone();
        let svc_b = svc.clone();
        let t2 = std::thread::spawn(move || {
            for i in 0..10 {
                svc_b
                    .enable_profile(&peer(0), 0, &pid_b, i % 2 == 0)
                    .unwrap();
            }
        });
        t1.join().unwrap();
        t2.join().unwrap();

        let list = svc.list_profiles(&peer(0)).unwrap();
        assert_eq!(
            list.len(),
            1,
            "no duplicate profiles after concurrent writes"
        );
        assert!(list[0].name.starts_with("name-"));
    }

    #[test]
    fn merge_rollback_on_missing_dst() {
        let (_dir, svc) = svc();
        let sid_a = svc.start_enrollment(&peer(0), 0, "a").unwrap();
        let pid_a = svc.commit_profile(&peer(0), &sid_a).unwrap();
        svc.drain_signals();

        let err = svc
            .merge_profiles(&peer(0), 0, &pid_a, "nonexistent-dst")
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)));

        let list = svc.list_profiles(&peer(0)).unwrap();
        assert_eq!(list.len(), 1, "src preserved on missing dst");
        assert_eq!(list[0].id, pid_a);
    }

    #[test]
    fn merge_missing_src_errors() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(0), 0, "dst").unwrap();
        let pid = svc.commit_profile(&peer(0), &sid).unwrap();

        let err = svc
            .merge_profiles(&peer(0), 0, "nonexistent-src", &pid)
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)));
        assert_eq!(svc.list_profiles(&peer(0)).unwrap().len(), 1);
    }

    #[test]
    fn rename_persists_across_restart() {
        let (dir, store) = test_store();

        let svc1 = ManagerService::new_with_store(StubPolkit, store.clone());
        let sid = svc1.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let pid = svc1.commit_profile(&peer(1000), &sid).unwrap();
        svc1.rename_profile(&peer(0), 1000, &pid, "Alice").unwrap();
        drop(svc1);

        let svc2 = ManagerService::new_with_store(StubPolkit, store);
        let list = svc2.list_profiles(&peer(1000)).unwrap();
        assert_eq!(list[0].name, "Alice");

        let _ = dir;
    }

    #[test]
    fn enable_disable_persists_across_restart() {
        let (dir, store) = test_store();

        let svc1 = ManagerService::new_with_store(StubPolkit, store.clone());
        let sid = svc1.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let pid = svc1.commit_profile(&peer(1000), &sid).unwrap();
        svc1.enable_profile(&peer(0), 1000, &pid, false).unwrap();
        drop(svc1);

        let svc2 = ManagerService::new_with_store(StubPolkit, store);
        let list = svc2.list_profiles(&peer(1000)).unwrap();
        assert!(!list[0].enabled, "disabled state persists");

        let _ = dir;
    }

    #[test]
    fn delete_persists_across_restart() {
        let (dir, store) = test_store();

        let svc1 = ManagerService::new_with_store(StubPolkit, store.clone());
        let sid = svc1.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let pid = svc1.commit_profile(&peer(1000), &sid).unwrap();
        svc1.delete_profile(&peer(0), 1000, &pid).unwrap();
        drop(svc1);

        let svc2 = ManagerService::new_with_store(StubPolkit, store);
        assert!(svc2.list_profiles(&peer(1000)).unwrap().is_empty());

        let _ = dir;
    }

    #[test]
    fn merge_persists_across_restart() {
        let (dir, store) = test_store();

        let svc1 = ManagerService::new_with_store(StubPolkit, store.clone());
        let sid_a = svc1.start_enrollment(&peer(0), 0, "a").unwrap();
        let pid_a = svc1.commit_profile(&peer(0), &sid_a).unwrap();
        let sid_b = svc1.start_enrollment(&peer(0), 0, "b").unwrap();
        let pid_b = svc1.commit_profile(&peer(0), &sid_b).unwrap();
        svc1.merge_profiles(&peer(0), 0, &pid_a, &pid_b).unwrap();
        drop(svc1);

        let svc2 = ManagerService::new_with_store(StubPolkit, store);
        let list = svc2.list_profiles(&peer(0)).unwrap();
        assert_eq!(list.len(), 1, "merge persisted: src removed, dst remains");
        assert_eq!(list[0].id, pid_b);

        let _ = dir;
    }

    #[test]
    fn replace_persists_across_restart() {
        let (dir, store) = test_store();

        let svc1 = ManagerService::new_with_store(StubPolkit, store.clone());
        let sid = svc1.start_enrollment(&peer(1000), 1000, "old").unwrap();
        let pid = svc1.commit_profile(&peer(1000), &sid).unwrap();
        svc1.replace_profile(&peer(0), 1000, &pid, "new").unwrap();
        drop(svc1);

        let svc2 = ManagerService::new_with_store(StubPolkit, store);
        let list = svc2.list_profiles(&peer(1000)).unwrap();
        assert_eq!(list[0].name, "new");

        let _ = dir;
    }

    #[test]
    fn null_store_list_returns_empty() {
        let svc = ManagerService::new(StubPolkit);
        assert!(svc.list_profiles(&peer(1000)).unwrap().is_empty());
    }

    #[test]
    fn null_store_commit_errors() {
        let svc = ManagerService::new(StubPolkit);
        let sid = svc.start_enrollment(&peer(0), 0, "root").unwrap();
        let err = svc.commit_profile(&peer(0), &sid).unwrap_err();
        assert!(
            matches!(err, ApiError::State(_)),
            "null store commit should error, got {err:?}"
        );
    }

    // ---- Task 11: real enrollment capture wiring ----

    #[test]
    fn start_progress_review_finish_full_lifecycle() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        assert!(*svc.emitter_armed.lock().unwrap(), "emitter armed on start");

        let mut last_progress: Option<EnrollmentProgress> = None;
        for _ in 0..12 {
            match svc.pump_enrollment(&sid).unwrap() {
                Some(p) => last_progress = Some(p),
                None => break,
            }
        }
        let progress = last_progress.expect("at least one progress signal");
        assert_eq!(progress.session_id, sid);
        assert!(progress.frames_captured > 0, "frames were captured");
        assert!(progress.coverage_percent <= 100);

        let sigs = svc.drain_signals();
        assert!(
            sigs.iter()
                .any(|s| matches!(s, ManagerSignal::EnrollmentProgress(_))),
            "EnrollmentProgress signal emitted during capture"
        );

        let pid = svc.commit_profile(&peer(1000), &sid).unwrap();
        assert!(
            !*svc.emitter_armed.lock().unwrap(),
            "emitter disarmed on commit"
        );

        let list = svc.list_profiles(&peer(1000)).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, pid);

        let review = svc.review_profile(&peer(0), 1000, &pid).unwrap();
        assert_eq!(review.profile_id, pid);
        assert!(
            matches!(
                review.quality,
                QualityLabel::Low | QualityLabel::Medium | QualityLabel::High
            ),
            "review returns coarse label, not a score"
        );
    }

    #[test]
    fn commit_writes_real_templates_from_reservoir() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let pid = svc.commit_profile(&peer(1000), &sid).unwrap();

        let _guard = svc.store_lock.lock().unwrap_or_else(|e| e.into_inner());
        let entries = svc.store.list_profiles(1000).unwrap();
        assert_eq!(entries.len(), 1);
        let templates = &entries[0].templates;
        assert!(
            !templates.is_empty(),
            "commit populates templates from the reservoir"
        );
        assert!(templates.len() <= 8, "at most SELECT_N templates stored");
        assert!(templates[0].is_anchor, "first template is the anchor");
        for t in templates {
            assert!(!t.vector.is_empty(), "template vector is populated");
        }
    }

    #[test]
    fn busy_rejects_second_session_for_same_uid() {
        let (_dir, svc) = svc();
        let _sid1 = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let err = svc
            .start_enrollment(&peer(1000), 1000, "alice2")
            .unwrap_err();
        assert!(
            matches!(err, ApiError::SessionBusy(1000)),
            "second session for same uid rejected, got {err:?}"
        );
    }

    #[test]
    fn cancel_during_capturing_zeroizes_and_disarms() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        for _ in 0..3 {
            svc.pump_enrollment(&sid).unwrap();
        }
        assert!(*svc.emitter_armed.lock().unwrap());
        svc.cancel_enrollment(&peer(1000), &sid).unwrap();
        assert!(!*svc.emitter_armed.lock().unwrap(), "emitter off on cancel");
        assert!(
            svc.list_profiles(&peer(1000)).unwrap().is_empty(),
            "no profile written"
        );
        let gen = *svc.store_gen.lock().unwrap();
        assert_eq!(gen, 0, "store generation unchanged on cancel");
        let sigs = svc.drain_signals();
        assert!(
            !sigs
                .iter()
                .any(|s| matches!(s, ManagerSignal::EnrollmentCompleted { .. })),
            "cancel must not emit EnrollmentCompleted"
        );
    }

    #[test]
    fn cancel_after_finish_not_possible_via_wire() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let _pid = svc.commit_profile(&peer(1000), &sid).unwrap();
        let _ = svc.cancel_enrollment(&peer(1000), &sid);
        let list = svc.list_profiles(&peer(1000)).unwrap();
        assert_eq!(
            list.len(),
            1,
            "committed profile survives post-commit cancel"
        );
    }

    #[test]
    fn cancel_unknown_session_is_noop() {
        let (_dir, svc) = svc();
        svc.cancel_enrollment(&peer(1000), "sess-nonexistent")
            .unwrap();
        assert!(!*svc.emitter_armed.lock().unwrap());
    }

    #[test]
    fn disconnect_during_enrollment_cancels_and_disarms() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        for _ in 0..3 {
            svc.pump_enrollment(&sid).unwrap();
        }
        assert!(*svc.emitter_armed.lock().unwrap());
        svc.handle_disconnect(&peer(1000));
        assert!(
            !*svc.emitter_armed.lock().unwrap(),
            "emitter off on disconnect"
        );
        assert!(
            svc.list_profiles(&peer(1000)).unwrap().is_empty(),
            "no profile written on disconnect"
        );
        let err = svc.pump_enrollment(&sid).unwrap_err();
        assert!(
            matches!(err, ApiError::SessionNotFound(_)),
            "session removed on disconnect"
        );
    }

    #[test]
    fn removal_during_enrollment_preserves_old_profile() {
        let (_dir, svc) = svc();
        let sid1 = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let pid1 = svc.commit_profile(&peer(1000), &sid1).unwrap();

        let sid2 = svc.start_enrollment(&peer(1000), 1000, "alice2").unwrap();
        for _ in 0..3 {
            svc.pump_enrollment(&sid2).unwrap();
        }
        svc.cancel_enrollment(&peer(1000), &sid2).unwrap();

        let list = svc.list_profiles(&peer(1000)).unwrap();
        assert_eq!(
            list.len(),
            1,
            "old profile preserved after canceling re-enroll"
        );
        assert_eq!(list[0].id, pid1);
    }

    #[test]
    fn daemon_restart_preserves_committed_profile_loses_in_progress() {
        let (dir, store) = test_store();
        let cf = mock_capture_factory();
        let ef = mock_embed_factory();

        let svc1 = ManagerService::new_with_capture(StubPolkit, store.clone(), cf, ef);
        let sid1 = svc1.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let pid1 = svc1.commit_profile(&peer(1000), &sid1).unwrap();

        let sid2 = svc1.start_enrollment(&peer(1000), 1000, "alice2").unwrap();
        for _ in 0..5 {
            svc1.pump_enrollment(&sid2).unwrap();
        }
        drop(svc1);

        let cf2 = mock_capture_factory();
        let ef2 = mock_embed_factory();
        let svc2 = ManagerService::new_with_capture(StubPolkit, store, cf2, ef2);
        let list = svc2.list_profiles(&peer(1000)).unwrap();
        assert_eq!(list.len(), 1, "committed profile survives restart");
        assert_eq!(list[0].id, pid1);
        assert!(
            svc2.pump_enrollment(&sid2).is_err(),
            "in-progress session lost on restart"
        );

        let _ = dir;
    }

    #[test]
    fn emitter_off_on_cancel_with_capture() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        svc.pump_enrollment(&sid).unwrap();
        assert!(*svc.emitter_armed.lock().unwrap());
        svc.cancel_enrollment(&peer(1000), &sid).unwrap();
        assert!(!*svc.emitter_armed.lock().unwrap());
        let st = svc.get_status(&peer(1000)).unwrap();
        assert!(!st.emitter_armed, "status reflects disarmed emitter");
    }

    #[test]
    fn progress_signal_carries_no_biometrics() {
        let (_dir, svc) = svc();
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        svc.pump_enrollment(&sid).unwrap();
        let sigs = svc.drain_signals();
        for sig in &sigs {
            if let ManagerSignal::EnrollmentProgress(p) = sig {
                let json = serde_json::to_string(p).unwrap();
                for token in [
                    "embedding",
                    "template",
                    "pixel",
                    "score",
                    "0x",
                    "nonce",
                    "key",
                    "threshold",
                    "vec",
                ] {
                    assert!(
                        !json.to_lowercase().contains(token),
                        "progress signal leaked `{token}`: {json}"
                    );
                }
                assert!(json.contains("frames_captured"));
                assert!(json.contains("coverage_percent"));
            }
        }
    }

    #[test]
    fn review_reflects_template_quality_not_enabled_flag() {
        let (dir, store) = test_store();
        let cf: CaptureFactory = Box::new(|| Ok(Box::new(ReplaySource::new(10, 130))));
        let ef: EmbedFactory = Box::new(|| Box::new(MockEmbedSource::new(8)));
        let svc = ManagerService::new_with_capture(StubPolkit, store, cf, ef);
        let sid = svc.start_enrollment(&peer(0), 0, "root").unwrap();
        let pid = svc.commit_profile(&peer(0), &sid).unwrap();

        let review = svc.review_profile(&peer(0), 0, &pid).unwrap();
        assert_eq!(
            review.quality,
            QualityLabel::High,
            "high-quality templates → High label"
        );

        let _ = dir;
    }

    #[test]
    fn no_capture_factory_commit_writes_empty_templates() {
        let (dir, store) = test_store();
        let svc = ManagerService::new_with_store(StubPolkit, store);
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let pid = svc.commit_profile(&peer(1000), &sid).unwrap();
        assert_eq!(pid, format!("profile-{sid}"));

        let _guard = svc.store_lock.lock().unwrap_or_else(|e| e.into_inner());
        let entries = svc.store.list_profiles(1000).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].templates.is_empty(),
            "no capture → empty templates (backward compat)"
        );

        let _ = dir;
    }
}
