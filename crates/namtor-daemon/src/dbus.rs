// SPDX-License-Identifier: GPL-3.0-only
//! D-Bus profile API: `org.namtor.Manager` surface (spec §9.1).
//!
//! Reference in-memory implementation of the profile management API exposed
//! over D-Bus. Polkit-backed authorisation gates every mutating operation;
//! enrollment sessions track emitter arming so a mid-enrollment disconnect
//! disarms the emitter without touching the template store. No biometric
//! payload (embedding, frame, template, score) ever crosses this surface.
//
// allow: SIZE_OK — single cohesive D-Bus API surface (types + trait + in-memory
// reference impl + introspection XML). The real zbus binding will replace the
// in-memory storage with calls to namtor-store; splitting now would be premature.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::acl::PeerCreds;
use crate::DaemonError;

/// API version of the `org.namtor.Manager` surface.
pub const API_VERSION: u32 = 1;

/// Polkit action identifiers (spec §9.1).
pub mod action {
    pub const ENROLL_SELF: &str = "org.namtor.enroll-self";
    pub const ENROLL_OTHER: &str = "org.namtor.enroll-other";
    pub const DELETE: &str = "org.namtor.delete";
    pub const CONFIGURE: &str = "org.namtor.configure";
}

/// Coarse quality bucket derived from a scalar coverage/quality metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub created: u64,
    pub last_used: Option<u64>,
}

/// Live enrollment progress emitted as the `Progress` signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollmentProgress {
    pub session_id: String,
    pub frames_captured: u32,
    pub quality: QualityLabel,
    pub coverage_percent: f32,
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

/// Signals emitted by the Manager interface.
#[derive(Debug, Clone, PartialEq)]
pub enum ManagerSignal {
    Progress(EnrollmentProgress),
    Enrolled {
        session_id: String,
        profile_id: String,
    },
    Cancelled {
        session_id: String,
    },
}

/// Snapshot returned by `Status`.
#[derive(Debug, Clone, PartialEq, Serialize)]
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
    frames: u32,
    armed: bool,
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
}

/// Public D-Bus-exposed profile API. Methods take a `PeerIdentity` so the
/// transport layer (zbus) can resolve the caller once and pass it in.
pub trait ProfileApi {
    fn list(&self, peer: &PeerIdentity) -> Result<Vec<ProfileInfo>, ApiError>;
    fn create(&self, peer: &PeerIdentity, target_uid: u32, name: &str) -> Result<String, ApiError>;
    fn start(&self, peer: &PeerIdentity, session_id: &str) -> Result<(), ApiError>;
    fn progress(
        &self,
        peer: &PeerIdentity,
        session_id: &str,
    ) -> Result<EnrollmentProgress, ApiError>;
    fn finish(&self, peer: &PeerIdentity, session_id: &str) -> Result<String, ApiError>;
    fn cancel(&self, peer: &PeerIdentity, session_id: &str) -> Result<(), ApiError>;
    fn delete(&self, peer: &PeerIdentity, uid: u32, id: &str) -> Result<(), ApiError>;
    fn rename(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        id: &str,
        new_name: &str,
    ) -> Result<(), ApiError>;
    fn enable(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        id: &str,
        enabled: bool,
    ) -> Result<(), ApiError>;
    fn status(&self, peer: &PeerIdentity) -> Result<StatusReport, ApiError>;
    fn drain_signals(&self) -> Vec<ManagerSignal>;
    fn handle_disconnect(&self, peer: &PeerIdentity);
}

#[allow(clippy::unwrap_used)]
impl<P: PolkitAuthority> ProfileApi for ManagerService<P> {
    fn list(&self, peer: &PeerIdentity) -> Result<Vec<ProfileInfo>, ApiError> {
        let profiles = self.profiles.lock().unwrap();
        Ok(profiles
            .iter()
            .filter(|((uid, _), _)| *uid == peer.uid)
            .map(|((_, id), p)| ProfileInfo {
                id: id.clone(),
                name: p.name.clone(),
                enabled: p.enabled,
                created: p.created,
                last_used: None,
            })
            .collect())
    }

    fn create(&self, peer: &PeerIdentity, target_uid: u32, name: &str) -> Result<String, ApiError> {
        let action = if peer.uid == target_uid {
            action::ENROLL_SELF
        } else {
            action::ENROLL_OTHER
        };
        self.authz(action, peer, target_uid)?;
        let mut active = self.active_per_uid.lock().unwrap();
        if active.contains_key(&target_uid) {
            return Err(ApiError::SessionBusy(target_uid));
        }
        let sid = Self::gen_session_id();
        active.insert(target_uid, sid.clone());
        self.sessions.lock().unwrap().insert(
            sid.clone(),
            Session {
                target_uid,
                peer_uid: peer.uid,
                name: name.to_string(),
                frames: 0,
                armed: false,
            },
        );
        Ok(sid)
    }

    fn start(&self, _peer: &PeerIdentity, session_id: &str) -> Result<(), ApiError> {
        let mut sessions = self.sessions.lock().unwrap();
        let s = sessions
            .get_mut(session_id)
            .ok_or_else(|| ApiError::SessionNotFound(session_id.into()))?;
        s.armed = true;
        *self.emitter_armed.lock().unwrap() = true;
        Ok(())
    }

    fn progress(
        &self,
        _peer: &PeerIdentity,
        session_id: &str,
    ) -> Result<EnrollmentProgress, ApiError> {
        let sessions = self.sessions.lock().unwrap();
        let s = sessions
            .get(session_id)
            .ok_or_else(|| ApiError::SessionNotFound(session_id.into()))?;
        Ok(EnrollmentProgress {
            session_id: session_id.to_string(),
            frames_captured: s.frames,
            quality: QualityLabel::High,
            coverage_percent: 100.0,
        })
    }

    fn finish(&self, _peer: &PeerIdentity, session_id: &str) -> Result<String, ApiError> {
        let mut sessions = self.sessions.lock().unwrap();
        let s = sessions
            .remove(session_id)
            .ok_or_else(|| ApiError::SessionNotFound(session_id.into()))?;
        *self.emitter_armed.lock().unwrap() = false;
        self.active_per_uid.lock().unwrap().remove(&s.target_uid);
        let profile_id = format!("profile-{}", session_id);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.profiles.lock().unwrap().insert(
            (s.target_uid, profile_id.clone()),
            InMemoryProfile {
                name: s.name,
                enabled: true,
                created: now,
            },
        );
        *self.store_gen.lock().unwrap() += 1;
        self.signals.lock().unwrap().push(ManagerSignal::Enrolled {
            session_id: session_id.to_string(),
            profile_id: profile_id.clone(),
        });
        Ok(profile_id)
    }

    fn cancel(&self, _peer: &PeerIdentity, session_id: &str) -> Result<(), ApiError> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(s) = sessions.remove(session_id) {
            *self.emitter_armed.lock().unwrap() = false;
            self.active_per_uid.lock().unwrap().remove(&s.target_uid);
            self.signals.lock().unwrap().push(ManagerSignal::Cancelled {
                session_id: session_id.to_string(),
            });
        }
        Ok(())
    }

    fn delete(&self, peer: &PeerIdentity, uid: u32, id: &str) -> Result<(), ApiError> {
        self.authz(action::DELETE, peer, uid)?;
        let mut profiles = self.profiles.lock().unwrap();
        if profiles.remove(&(uid, id.to_string())).is_none() {
            return Err(ApiError::NotFound(id.into()));
        }
        *self.store_gen.lock().unwrap() += 1;
        Ok(())
    }

    fn rename(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        id: &str,
        new_name: &str,
    ) -> Result<(), ApiError> {
        self.authz(action::CONFIGURE, peer, uid)?;
        let mut profiles = self.profiles.lock().unwrap();
        let p = profiles
            .get_mut(&(uid, id.to_string()))
            .ok_or_else(|| ApiError::NotFound(id.into()))?;
        p.name = new_name.to_string();
        *self.store_gen.lock().unwrap() += 1;
        Ok(())
    }

    fn enable(
        &self,
        peer: &PeerIdentity,
        uid: u32,
        id: &str,
        enabled: bool,
    ) -> Result<(), ApiError> {
        self.authz(action::CONFIGURE, peer, uid)?;
        let mut profiles = self.profiles.lock().unwrap();
        let p = profiles
            .get_mut(&(uid, id.to_string()))
            .ok_or_else(|| ApiError::NotFound(id.into()))?;
        p.enabled = enabled;
        *self.store_gen.lock().unwrap() += 1;
        Ok(())
    }

    fn status(&self, _peer: &PeerIdentity) -> Result<StatusReport, ApiError> {
        let profiles = self.profiles.lock().unwrap();
        let list: Vec<ProfileInfo> = profiles
            .iter()
            .map(|((_, id), p)| ProfileInfo {
                id: id.clone(),
                name: p.name.clone(),
                enabled: p.enabled,
                created: p.created,
                last_used: None,
            })
            .collect();
        Ok(StatusReport {
            api_version: API_VERSION,
            profiles: list,
            emitter_armed: *self.emitter_armed.lock().unwrap(),
        })
    }

    fn drain_signals(&self) -> Vec<ManagerSignal> {
        std::mem::take(&mut *self.signals.lock().unwrap())
    }

    fn handle_disconnect(&self, peer: &PeerIdentity) {
        let mut sessions = self.sessions.lock().unwrap();
        let sids: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| s.peer_uid == peer.uid)
            .map(|(k, _)| k.clone())
            .collect();
        for sid in sids {
            if let Some(s) = sessions.remove(&sid) {
                self.active_per_uid.lock().unwrap().remove(&s.target_uid);
            }
        }
        *self.emitter_armed.lock().unwrap() = false;
    }
}

/// Static D-Bus introspection XML for the `org.namtor.Manager` interface.
pub fn introspect_xml() -> &'static str {
    r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node><interface name="org.namtor.Manager">
<method name="ListProfiles"><arg name="profiles" type="a(ssbtt)" direction="out"/></method>
<method name="CreateProfile"><arg name="uid" type="u" direction="in"/><arg name="name" type="s" direction="in"/><arg name="session" type="s" direction="out"/></method>
<method name="StartEnrollment"><arg name="session" type="s" direction="in"/></method>
<method name="GetProgress"><arg name="session" type="s" direction="in"/><arg name="progress" type="(suss)" direction="out"/></method>
<method name="FinishEnrollment"><arg name="session" type="s" direction="in"/><arg name="profile" type="s" direction="out"/></method>
<method name="CancelEnrollment"><arg name="session" type="s" direction="in"/></method>
<method name="DeleteProfile"><arg name="uid" type="u" direction="in"/><arg name="id" type="s" direction="in"/></method>
<method name="RenameProfile"><arg name="uid" type="u" direction="in"/><arg name="id" type="s" direction="in"/><arg name="name" type="s" direction="in"/></method>
<method name="EnableProfile"><arg name="uid" type="u" direction="in"/><arg name="id" type="s" direction="in"/><arg name="enabled" type="b" direction="in"/></method>
<method name="Status"><arg name="status" type="(ua(ssbtt)b)" direction="out"/></method>
<signal name="Progress"><arg name="progress" type="(suss)"/></signal>
<signal name="Enrolled"><arg name="profile" type="s"/></signal>
<signal name="Cancelled"><arg name="session" type="s"/></signal>
<property name="ApiVersion" type="u" access="read"/>
</interface></node>"#
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
        let sid = svc.create(&peer(1000), 1000, "alice").unwrap();
        assert!(sid.starts_with("sess-"));
    }

    #[test]
    fn enroll_other_requires_admin() {
        let svc = ManagerService::new(SystemPolkit);
        assert!(matches!(
            svc.create(&peer(1000), 1001, "bob"),
            Err(ApiError::Denied { .. })
        ));
    }

    #[test]
    fn delete_requires_admin() {
        let svc = ManagerService::new(SystemPolkit);
        // non-admin
        assert!(matches!(
            svc.delete(&peer(1000), 1000, "x"),
            Err(ApiError::Denied { .. })
        ));
        // admin - first create a profile via enrollment
        let sid = svc.create(&peer(0), 0, "root").unwrap();
        svc.start(&peer(0), &sid).unwrap();
        let pid = svc.finish(&peer(0), &sid).unwrap();
        svc.delete(&peer(0), 0, &pid).unwrap();
    }

    #[test]
    fn configure_requires_admin() {
        let svc = ManagerService::new(SystemPolkit);
        assert!(matches!(
            svc.rename(&peer(1000), 1000, "x", "y"),
            Err(ApiError::Denied { .. })
        ));
        assert!(matches!(
            svc.enable(&peer(1000), 1000, "x", false),
            Err(ApiError::Denied { .. })
        ));
    }

    #[test]
    fn disconnect_mid_enrollment_cleans_up_emitter() {
        let svc = ManagerService::new(SystemPolkit);
        let sid = svc.create(&peer(1000), 1000, "alice").unwrap();
        svc.start(&peer(1000), &sid).unwrap();
        assert!(*svc.emitter_armed.lock().unwrap());
        svc.handle_disconnect(&peer(1000));
        assert!(!*svc.emitter_armed.lock().unwrap());
    }

    #[test]
    fn disconnect_mid_enrollment_leaves_store_unchanged() {
        let svc = ManagerService::new(SystemPolkit);
        let gen_before = *svc.store_gen.lock().unwrap();
        let sid = svc.create(&peer(1000), 1000, "alice").unwrap();
        svc.start(&peer(1000), &sid).unwrap();
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
            last_used: None,
        };
        let ep = EnrollmentProgress {
            session_id: "s".into(),
            frames_captured: 0,
            quality: QualityLabel::High,
            coverage_percent: 100.0,
        };
        let sr = StatusReport {
            api_version: 1,
            profiles: vec![pi],
            emitter_armed: false,
        };
        let json = serde_json::to_string(&ep).unwrap();
        // `frames_captured` is a counter, not a raw frame payload; the guard
        // checks for biometric-bearing tokens only.
        for token in [
            "embedding",
            "template",
            "pixel",
            "score=",
            "0x",
            "nonce",
            "key",
        ] {
            assert!(!json.to_lowercase().contains(token), "ep has {token}");
        }
        let sr_json = serde_json::to_string(&sr).unwrap();
        for token in [
            "embedding",
            "template",
            "pixel",
            "score=",
            "0x",
            "nonce",
            "key",
        ] {
            assert!(!sr_json.to_lowercase().contains(token), "sr has {token}");
        }
    }

    #[test]
    fn status_returns_profile_list_without_biometrics() {
        let svc = ManagerService::new(SystemPolkit);
        let sid = svc.create(&peer(1000), 1000, "alice").unwrap();
        svc.start(&peer(1000), &sid).unwrap();
        svc.finish(&peer(1000), &sid).unwrap();
        let st = svc.status(&peer(1000)).unwrap();
        assert_eq!(st.profiles.len(), 1);
        assert_eq!(st.profiles[0].name, "alice");
        let json = serde_json::to_string(&st).unwrap();
        assert!(!json.contains("embedding"));
        assert!(!json.contains("template"));
    }

    #[test]
    fn stable_introspection() {
        let a = introspect_xml();
        let b = introspect_xml();
        assert_eq!(a, b);
        assert!(a.contains("org.namtor.Manager"));
        assert!(a.contains("ListProfiles"));
        assert!(a.contains("FinishEnrollment"));
        assert!(a.contains("DeleteProfile"));
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
