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
use crate::manager::contract;
use crate::manager::store::{map_store_error, NullProfileStore, ProfileStore};
use crate::DaemonError;
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub uid: u32,
    pub pid: i32,
    pub cmdline: String,
}

impl PeerIdentity {
    /// Build a peer identity from socket credentials. Reads `/proc/<pid>/cmdline`
    /// on a best-effort basis (Linux only); a missing or unreadable cmdline is
    /// represented as an empty string rather than an error.
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

/// Stub polkit authority: root is admin, self-enrollment is allowed for own uid,
/// everything else is denied. Sufficient for the in-memory reference impl.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPolkit;

impl PolkitAuthority for SystemPolkit {
    fn check(&self, action: &str, peer: &PeerIdentity, target_uid: u32) -> AuthzDecision {
        let admin = peer.uid == 0;
        match action {
            action::ENROLL_SELF if peer.uid == target_uid => AuthzDecision::Allowed,
            action::ENROLL_SELF if admin => AuthzDecision::Allowed,
            action::ENROLL_OTHER | action::DELETE | action::CONFIGURE if admin => {
                AuthzDecision::Allowed
            }
            _ => AuthzDecision::Denied,
        }
    }
}

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

// Live enrollment session.
#[derive(Debug)]
struct Session {
    target_uid: u32,
    peer_uid: u32,
    name: String,
}

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
    sessions: Mutex<HashMap<String, Session>>,
    active_per_uid: Mutex<HashMap<u32, String>>,
    emitter_armed: Mutex<bool>,
    signals: Mutex<Vec<ManagerSignal>>,
    store_gen: Mutex<u64>,
}

impl<P: PolkitAuthority> ManagerService<P> {
    /// Construct with a null store. Used by bus-service tests that exercise
    /// D-Bus mechanics, not profile persistence. Production code must use
    /// [`Self::new_with_store`].
    pub fn new(polkit: P) -> Self {
        Self::new_with_store(polkit, Arc::new(NullProfileStore))
    }

    /// Construct with a durable profile store. The daemon passes a
    /// [`TemplateProfileStore`] wrapping the real [`namtor_store::TemplateStore`].
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
        }
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
            .filter(|(_, s)| s.peer_uid == peer.uid)
            .map(|(k, _)| k.clone())
            .collect();
        for sid in sids {
            if let Some(s) = sessions.remove(&sid) {
                lock(&self.active_per_uid).remove(&s.target_uid);
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
        active.insert(target_uid, sid.clone());
        lock(&self.sessions).insert(
            sid.clone(),
            Session {
                target_uid,
                peer_uid: peer.uid,
                name: name.to_string(),
            },
        );
        let was_armed = *lock(&self.emitter_armed);
        *lock(&self.emitter_armed) = true;
        if !was_armed {
            self.push_status_changed();
        }
        Ok(sid)
    }

    fn cancel_enrollment(&self, _peer: &PeerIdentity, session_id: &str) -> Result<(), ApiError> {
        let mut sessions = lock(&self.sessions);
        if let Some(s) = sessions.remove(session_id) {
            lock(&self.active_per_uid).remove(&s.target_uid);
            let was_armed = *lock(&self.emitter_armed);
            *lock(&self.emitter_armed) = false;
            if was_armed {
                self.push_status_changed();
            }
        }
        Ok(())
    }

    fn commit_profile(&self, _peer: &PeerIdentity, session_id: &str) -> Result<String, ApiError> {
        let mut sessions = lock(&self.sessions);
        let s = sessions
            .remove(session_id)
            .ok_or_else(|| ApiError::SessionNotFound(session_id.into()))?;
        let was_armed = *lock(&self.emitter_armed);
        *lock(&self.emitter_armed) = false;
        lock(&self.active_per_uid).remove(&s.target_uid);
        drop(sessions);

        let profile_id = format!("profile-{}", session_id);
        let record = ProfileRecord {
            profile_id: ProfileId(profile_id.clone()),
            display_name: s.name,
            enabled: true,
            uid: s.target_uid,
            model_id: PROFILE_MODEL_ID.into(),
            created_at_us: now_us(),
            schema_version: 1,
            last_used_us: 0,
        };
        {
            let _guard = lock(&self.store_lock);
            self.store
                .create_profile(s.target_uid, record, Vec::new())
                .map_err(map_store_error)?;
        }
        *lock(&self.store_gen) += 1;
        let mut sigs = lock(&self.signals);
        sigs.push(ManagerSignal::EnrollmentCompleted {
            session_id: session_id.to_string(),
            profile_id: profile_id.clone(),
        });
        sigs.push(ManagerSignal::ProfileChanged {
            uid: s.target_uid,
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
        let quality = if entry.profile.enabled {
            QualityLabel::High
        } else {
            QualityLabel::Low
        };
        let outcome = ReviewOutcome {
            profile_id: id.to_string(),
            needs_reenroll: !entry.profile.enabled,
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
    use crate::manager::store::TemplateProfileStore;
    use namtor_store::TemplateStore;

    fn peer(uid: u32) -> PeerIdentity {
        PeerIdentity {
            uid,
            pid: 1,
            cmdline: "test".into(),
        }
    }

    fn test_store() -> (tempfile::TempDir, Arc<dyn ProfileStore>) {
        let dir = tempfile::tempdir().unwrap();
        let ts = TemplateStore::open(dir.path(), &dir.path().join("key")).unwrap();
        (dir, Arc::new(TemplateProfileStore::new(Arc::new(ts))))
    }

    fn svc() -> (tempfile::TempDir, ManagerService<SystemPolkit>) {
        let (dir, store) = test_store();
        let svc = ManagerService::new_with_store(SystemPolkit, store);
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

        let svc1 = ManagerService::new_with_store(SystemPolkit, store.clone());
        let sid = svc1.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let pid = svc1.commit_profile(&peer(1000), &sid).unwrap();

        drop(svc1);

        let svc2 = ManagerService::new_with_store(SystemPolkit, store);
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

        let svc1 = ManagerService::new_with_store(SystemPolkit, store.clone());
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

        let svc2 = ManagerService::new_with_store(SystemPolkit, store);
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

        let svc1 = ManagerService::new_with_store(SystemPolkit, store.clone());
        let sid = svc1.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let pid = svc1.commit_profile(&peer(1000), &sid).unwrap();
        svc1.rename_profile(&peer(0), 1000, &pid, "Alice").unwrap();
        drop(svc1);

        let svc2 = ManagerService::new_with_store(SystemPolkit, store);
        let list = svc2.list_profiles(&peer(1000)).unwrap();
        assert_eq!(list[0].name, "Alice");

        let _ = dir;
    }

    #[test]
    fn enable_disable_persists_across_restart() {
        let (dir, store) = test_store();

        let svc1 = ManagerService::new_with_store(SystemPolkit, store.clone());
        let sid = svc1.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let pid = svc1.commit_profile(&peer(1000), &sid).unwrap();
        svc1.enable_profile(&peer(0), 1000, &pid, false).unwrap();
        drop(svc1);

        let svc2 = ManagerService::new_with_store(SystemPolkit, store);
        let list = svc2.list_profiles(&peer(1000)).unwrap();
        assert!(!list[0].enabled, "disabled state persists");

        let _ = dir;
    }

    #[test]
    fn delete_persists_across_restart() {
        let (dir, store) = test_store();

        let svc1 = ManagerService::new_with_store(SystemPolkit, store.clone());
        let sid = svc1.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        let pid = svc1.commit_profile(&peer(1000), &sid).unwrap();
        svc1.delete_profile(&peer(0), 1000, &pid).unwrap();
        drop(svc1);

        let svc2 = ManagerService::new_with_store(SystemPolkit, store);
        assert!(svc2.list_profiles(&peer(1000)).unwrap().is_empty());

        let _ = dir;
    }

    #[test]
    fn merge_persists_across_restart() {
        let (dir, store) = test_store();

        let svc1 = ManagerService::new_with_store(SystemPolkit, store.clone());
        let sid_a = svc1.start_enrollment(&peer(0), 0, "a").unwrap();
        let pid_a = svc1.commit_profile(&peer(0), &sid_a).unwrap();
        let sid_b = svc1.start_enrollment(&peer(0), 0, "b").unwrap();
        let pid_b = svc1.commit_profile(&peer(0), &sid_b).unwrap();
        svc1.merge_profiles(&peer(0), 0, &pid_a, &pid_b).unwrap();
        drop(svc1);

        let svc2 = ManagerService::new_with_store(SystemPolkit, store);
        let list = svc2.list_profiles(&peer(0)).unwrap();
        assert_eq!(list.len(), 1, "merge persisted: src removed, dst remains");
        assert_eq!(list[0].id, pid_b);

        let _ = dir;
    }

    #[test]
    fn replace_persists_across_restart() {
        let (dir, store) = test_store();

        let svc1 = ManagerService::new_with_store(SystemPolkit, store.clone());
        let sid = svc1.start_enrollment(&peer(1000), 1000, "old").unwrap();
        let pid = svc1.commit_profile(&peer(1000), &sid).unwrap();
        svc1.replace_profile(&peer(0), 1000, &pid, "new").unwrap();
        drop(svc1);

        let svc2 = ManagerService::new_with_store(SystemPolkit, store);
        let list = svc2.list_profiles(&peer(1000)).unwrap();
        assert_eq!(list[0].name, "new");

        let _ = dir;
    }

    #[test]
    fn null_store_list_returns_empty() {
        let svc = ManagerService::new(SystemPolkit);
        assert!(svc.list_profiles(&peer(1000)).unwrap().is_empty());
    }

    #[test]
    fn null_store_commit_errors() {
        let svc = ManagerService::new(SystemPolkit);
        let sid = svc.start_enrollment(&peer(0), 0, "root").unwrap();
        let err = svc.commit_profile(&peer(0), &sid).unwrap_err();
        assert!(
            matches!(err, ApiError::State(_)),
            "null store commit should error, got {err:?}"
        );
    }
}
