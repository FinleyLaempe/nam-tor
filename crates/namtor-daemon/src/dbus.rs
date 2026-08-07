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
use std::sync::{Mutex, MutexGuard};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
use crate::acl::PeerCreds;
use crate::manager::contract;
use crate::DaemonError;
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

// In-memory profile entry for the reference impl.
#[derive(Debug, Clone)]
struct InMemoryProfile {
    name: String,
    enabled: bool,
    created: u64,
}

// Live enrollment session.
#[derive(Debug)]
struct Session {
    target_uid: u32,
    peer_uid: u32,
    name: String,
}

/// Reference in-memory `org.namtor.Manager` service.
pub struct ManagerService<P: PolkitAuthority> {
    polkit: P,
    sessions: Mutex<HashMap<String, Session>>,
    active_per_uid: Mutex<HashMap<u32, String>>,
    profiles: Mutex<HashMap<(u32, String), InMemoryProfile>>,
    emitter_armed: Mutex<bool>,
    signals: Mutex<Vec<ManagerSignal>>,
    store_gen: Mutex<u64>,
}

impl<P: PolkitAuthority> ManagerService<P> {
    pub fn new(polkit: P) -> Self {
        Self {
            polkit,
            sessions: Mutex::new(HashMap::new()),
            active_per_uid: Mutex::new(HashMap::new()),
            profiles: Mutex::new(HashMap::new()),
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
        let profiles = lock(&self.profiles);
        let list: Vec<ProfileInfo> = profiles
            .iter()
            .map(|((_, id), p)| ProfileInfo {
                id: id.clone(),
                name: p.name.clone(),
                enabled: p.enabled,
                created: p.created,
                last_used: 0,
            })
            .collect();
        StatusReport {
            api_version: API_VERSION,
            profiles: list,
            emitter_armed: *lock(&self.emitter_armed),
        }
    }
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
        let profiles = lock(&self.profiles);
        Ok(profiles
            .iter()
            .filter(|((uid, _), _)| *uid == peer.uid)
            .map(|((_, id), p)| ProfileInfo {
                id: id.clone(),
                name: p.name.clone(),
                enabled: p.enabled,
                created: p.created,
                last_used: 0,
            })
            .collect())
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
        let profile_id = format!("profile-{}", session_id);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        lock(&self.profiles).insert(
            (s.target_uid, profile_id.clone()),
            InMemoryProfile {
                name: s.name,
                enabled: true,
                created: now,
            },
        );
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
        let profiles = lock(&self.profiles);
        let p = profiles
            .get(&(uid, id.to_string()))
            .ok_or_else(|| ApiError::NotFound(id.into()))?;
        let quality = if p.enabled {
            QualityLabel::High
        } else {
            QualityLabel::Low
        };
        let outcome = ReviewOutcome {
            profile_id: id.to_string(),
            needs_reenroll: !p.enabled,
            quality,
        };
        drop(profiles);
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
        let mut profiles = lock(&self.profiles);
        let p = profiles
            .get_mut(&(uid, id.to_string()))
            .ok_or_else(|| ApiError::NotFound(id.into()))?;
        p.name = new_name.to_string();
        *lock(&self.store_gen) += 1;
        drop(profiles);
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
        let mut profiles = lock(&self.profiles);
        let p = profiles
            .get_mut(&(uid, id.to_string()))
            .ok_or_else(|| ApiError::NotFound(id.into()))?;
        p.enabled = enabled;
        *lock(&self.store_gen) += 1;
        drop(profiles);
        lock(&self.signals).push(ManagerSignal::ProfileChanged {
            uid,
            id: id.to_string(),
        });
        Ok(())
    }

    fn delete_profile(&self, peer: &PeerIdentity, uid: u32, id: &str) -> Result<(), ApiError> {
        self.authz(action::DELETE, peer, uid)?;
        let mut profiles = lock(&self.profiles);
        if profiles.remove(&(uid, id.to_string())).is_none() {
            return Err(ApiError::NotFound(id.into()));
        }
        *lock(&self.store_gen) += 1;
        drop(profiles);
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
        let mut profiles = lock(&self.profiles);
        let src_p = profiles
            .remove(&(uid, src_id.to_string()))
            .ok_or_else(|| ApiError::NotFound(src_id.into()))?;
        let dst_p = profiles
            .get_mut(&(uid, dst_id.to_string()))
            .ok_or_else(|| ApiError::NotFound(dst_id.into()))?;
        dst_p.name = src_p.name;
        dst_p.enabled = src_p.enabled;
        dst_p.created = src_p.created;
        *lock(&self.store_gen) += 1;
        drop(profiles);
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
        let mut profiles = lock(&self.profiles);
        let p = profiles
            .get_mut(&(uid, id.to_string()))
            .ok_or_else(|| ApiError::NotFound(id.into()))?;
        p.name = new_name.to_string();
        *lock(&self.store_gen) += 1;
        drop(profiles);
        lock(&self.signals).push(ManagerSignal::ProfileChanged {
            uid,
            id: id.to_string(),
        });
        Ok(())
    }

    fn get_status(&self, _peer: &PeerIdentity) -> Result<StatusReport, ApiError> {
        Ok(self.snapshot_status())
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

    fn peer(uid: u32) -> PeerIdentity {
        PeerIdentity {
            uid,
            pid: 1,
            cmdline: "test".into(),
        }
    }

    #[test]
    fn self_enroll_allowed_for_own_uid() {
        let svc = ManagerService::new(SystemPolkit);
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        assert!(sid.starts_with("sess-"));
    }

    #[test]
    fn enroll_other_requires_admin() {
        let svc = ManagerService::new(SystemPolkit);
        assert!(matches!(
            svc.start_enrollment(&peer(1000), 1001, "bob"),
            Err(ApiError::Denied { .. })
        ));
    }

    #[test]
    fn delete_requires_admin() {
        let svc = ManagerService::new(SystemPolkit);
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
        let svc = ManagerService::new(SystemPolkit);
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
        let svc = ManagerService::new(SystemPolkit);
        let _sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        assert!(*svc.emitter_armed.lock().unwrap());
        svc.handle_disconnect(&peer(1000));
        assert!(!*svc.emitter_armed.lock().unwrap());
    }

    #[test]
    fn disconnect_mid_enrollment_leaves_store_unchanged() {
        let svc = ManagerService::new(SystemPolkit);
        let gen_before = *svc.store_gen.lock().unwrap();
        let _sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        svc.handle_disconnect(&peer(1000));
        let gen_after = *svc.store_gen.lock().unwrap();
        assert_eq!(gen_before, gen_after, "store generation unchanged");
        assert!(svc.profiles.lock().unwrap().is_empty());
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
        let svc = ManagerService::new(SystemPolkit);
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
        let svc = ManagerService::new(SystemPolkit);
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
        let svc = ManagerService::new(SystemPolkit);
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
        let svc = ManagerService::new(SystemPolkit);
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
        let svc = ManagerService::new(SystemPolkit);
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
        let svc = ManagerService::new(SystemPolkit);
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
        let svc = ManagerService::new(SystemPolkit);
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
}
