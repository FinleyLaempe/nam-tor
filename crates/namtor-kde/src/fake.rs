// SPDX-License-Identifier: GPL-3.0-only
//! In-process fake `org.namtor.Manager` service for hermetic KCM tests.
//!
//! Drives a p2p Unix-socket pair (no D-Bus daemon required) so the contract
//! suite runs anywhere `cargo test` runs. The fake is configurable through
//! [`FakeState`] held in a `Mutex`; tests flip `daemon_unavailable` /
//! `auth_denied` to exercise the error and auth-loss paths without touching
//! real polkit.
//!
//! Implements the frozen Task 8 contract: 11 methods, 4 signals, 1 property.
//! No biometric data crosses this surface.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use zbus::interface;
use zbus::zvariant::Type;

use crate::contract::ScenarioHook;
use crate::types::{
    EnrollmentProgress, EnrollmentProgressV3, EnrollmentReviewV3, ProfileInfo, QualityLabel,
    ReviewOutcome, StatusReport,
};

/// Frozen API version (matches `namtor-daemon::manager::contract::API_VERSION`).
const FAKE_API_VERSION: u32 = 1;
/// Frozen v3 API version (matches `namtor-daemon::manager::contract::API_VERSION_V3`).
const FAKE_API_VERSION_V3: u32 = 3;

/// Mutable state behind the fake interface. Tests mutate it through
/// [`FakeHandle`] (which implements [`ScenarioHook`]).
#[derive(Debug, Default)]
pub struct FakeState {
    pub profiles: Vec<ProfileInfo>,
    pub daemon_unavailable: bool,
    pub auth_denied: bool,
    pub next_session: u32,
    /// v3 session state: session_id → phase ("capturing" or "stopped").
    pub v3_sessions: std::collections::HashMap<String, V3Phase>,
}

/// v3 enrollment session phase tracked by the fake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V3Phase {
    Capturing,
    Stopped,
}

/// The zbus interface implementation. Method handlers are deliberately tiny:
/// they read/write [`FakeState`] and return the wire types. They never touch
/// biometric data.
pub struct FakeManager {
    state: Arc<Mutex<FakeState>>,
}

impl FakeManager {
    pub fn new(state: Arc<Mutex<FakeState>>) -> Self {
        Self { state }
    }

    fn denied(&self) -> Option<zbus::fdo::Error> {
        let s = self.state.lock().unwrap();
        if s.daemon_unavailable {
            Some(zbus::fdo::Error::ServiceUnknown(
                "org.namtor.Manager".to_string(),
            ))
        } else if s.auth_denied {
            Some(zbus::fdo::Error::AccessDenied("polkit denied".to_string()))
        } else {
            None
        }
    }
}

#[interface(name = "org.namtor.Manager")]
impl FakeManager {
    fn list_profiles(&self) -> zbus::fdo::Result<Vec<ProfileInfo>> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        Ok(self.state.lock().unwrap().profiles.clone())
    }

    /// `StartEnrollment(uid, name) -> session`. Allocates a fresh session id.
    fn start_enrollment(&self, _uid: u32, name: &str) -> zbus::fdo::Result<String> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        s.next_session = s.next_session.wrapping_add(1);
        Ok(format!("sess-{name}-{}", s.next_session))
    }

    fn cancel_enrollment(&self, _session: &str) -> zbus::fdo::Result<()> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        Ok(())
    }

    /// `CommitProfile(session) -> profile_id`. Commits a new profile.
    fn commit_profile(&self, _session: &str) -> zbus::fdo::Result<String> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        let id = format!("p{}", s.profiles.len() + 1);
        s.profiles.push(ProfileInfo {
            id: id.clone(),
            name: "Alice".to_string(),
            enabled: true,
            created: 1,
            last_used: 0,
        });
        Ok(id)
    }

    /// `ReviewProfile(uid, id) -> (sbs)`. Coarse quality review.
    fn review_profile(&self, _uid: u32, id: &str) -> zbus::fdo::Result<ReviewOutcome> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let s = self.state.lock().unwrap();
        let needs_reenroll = s.profiles.iter().any(|p| p.id == id && !p.enabled);
        Ok(ReviewOutcome {
            profile_id: id.to_string(),
            needs_reenroll,
            quality: QualityLabel::High,
        })
    }

    fn rename_profile(&self, _uid: u32, id: &str, new_name: &str) -> zbus::fdo::Result<()> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        for p in s.profiles.iter_mut() {
            if p.id == id {
                p.name = new_name.to_string();
            }
        }
        Ok(())
    }

    fn enable_profile(&self, _uid: u32, id: &str, enabled: bool) -> zbus::fdo::Result<()> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        for p in s.profiles.iter_mut() {
            if p.id == id {
                p.enabled = enabled;
            }
        }
        Ok(())
    }

    fn delete_profile(&self, _uid: u32, id: &str) -> zbus::fdo::Result<()> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        s.profiles.retain(|p| p.id != id);
        Ok(())
    }

    fn merge_profiles(&self, _uid: u32, src: &str, _dst: &str) -> zbus::fdo::Result<()> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        s.profiles.retain(|p| p.id != src);
        Ok(())
    }

    fn replace_profile(&self, _uid: u32, id: &str, name: &str) -> zbus::fdo::Result<()> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        for p in s.profiles.iter_mut() {
            if p.id == id {
                p.name = name.to_string();
            }
        }
        Ok(())
    }

    /// `GetStatus() -> (ua(ssbtt)b)`. Aggregated status snapshot.
    fn get_status(&self) -> zbus::fdo::Result<StatusReport> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let s = self.state.lock().unwrap();
        Ok(StatusReport {
            api_version: FAKE_API_VERSION,
            profiles: s.profiles.clone(),
            emitter_armed: false,
        })
    }

    /// `ApiVersion` property (read).
    #[zbus(property)]
    fn api_version(&self) -> zbus::fdo::Result<u32> {
        Ok(FAKE_API_VERSION)
    }

    #[zbus(signal)]
    async fn enrollment_progress(
        &self,
        _emitter: zbus::object_server::SignalEmitter<'_>,
        session: String,
        progress: EnrollmentProgress,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn enrollment_completed(
        &self,
        _emitter: zbus::object_server::SignalEmitter<'_>,
        session: String,
        profile: String,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn profile_changed(
        &self,
        _emitter: zbus::object_server::SignalEmitter<'_>,
        uid: u32,
        id: String,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_changed(
        &self,
        _emitter: zbus::object_server::SignalEmitter<'_>,
        status: StatusReport,
    ) -> zbus::Result<()>;
}

/// Test handle: owns the shared fake state and lets a test reconfigure it
/// between contract cases. Implements [`ScenarioHook`] so the contract suite
/// can drive it without knowing about zbus.
#[derive(Clone)]
pub struct FakeHandle {
    state: Arc<Mutex<FakeState>>,
}

impl FakeHandle {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeState::default())),
        }
    }

    pub fn state_arc(&self) -> Arc<Mutex<FakeState>> {
        self.state.clone()
    }
}

impl Default for FakeHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl ScenarioHook for FakeHandle {
    fn reset(&self, seed: &[ProfileInfo]) {
        *self.state.lock().unwrap() = FakeState {
            profiles: seed.to_vec(),
            daemon_unavailable: false,
            auth_denied: false,
            next_session: 0,
            v3_sessions: std::collections::HashMap::new(),
        };
    }

    fn set_daemon_unavailable(&self) {
        self.state.lock().unwrap().daemon_unavailable = true;
    }

    fn set_auth_denied(&self) {
        self.state.lock().unwrap().auth_denied = true;
    }
}

/// A live p2p fake bus: owns the server and client connections so the client
/// proxy stays usable for the test's lifetime. Drop closes both.
pub struct FakeBus {
    pub server: zbus::Connection,
    pub client: zbus::Connection,
}

impl FakeBus {
    /// Build a fake service pre-seeded with `seed` profiles and the given
    /// shared state handle (so the test can mutate behavior mid-test).
    pub async fn spawn(handle: &FakeHandle) -> Result<Self, zbus::Error> {
        let (a, b) = UnixStream::pair().map_err(|e| zbus::Error::Failure(e.to_string()))?;
        let guid = zbus::Guid::generate();
        let server_b = zbus::connection::Builder::unix_stream(a)
            .server(guid)
            .map_err(|e| zbus::Error::Failure(e.to_string()))?
            .p2p()
            .serve_at(
                crate::bridge::MANAGER_OBJECT_PATH,
                FakeManager::new(handle.state_arc()),
            )?
            .serve_at(
                crate::bridge::MANAGER_OBJECT_PATH,
                FakeManager3::new(handle.state_arc()),
            )?;
        let client_b = zbus::connection::Builder::unix_stream(b).p2p();
        let (server, client) = futures_util::try_join!(server_b.build(), client_b.build())?;
        Ok(Self { server, client })
    }
}

/// Compile-time privacy guard: the types the KCM exposes to QML must derive no
/// biometric payload. This is exercised by the privacy-scan test.
#[allow(dead_code)]
fn _assert_no_biometric_types() {
    fn require_send<T: Send>() {}
    require_send::<ProfileInfo>();
    require_send::<EnrollmentProgress>();
    require_send::<ReviewOutcome>();
    require_send::<StatusReport>();
    // Type must be present for the privacy scan's `Type`-derive check.
    fn require_type<T: Type>() {}
    require_type::<ProfileInfo>();
    require_type::<EnrollmentProgress>();
    require_type::<ReviewOutcome>();
    require_type::<StatusReport>();
    // v3 types are part of the v3 wire surface.
    require_send::<EnrollmentProgressV3>();
    require_send::<EnrollmentReviewV3>();
    require_type::<EnrollmentProgressV3>();
    require_type::<EnrollmentReviewV3>();
}

// ---------------------------------------------------------------------------
// v3 fake interface: org.namtor.Manager3 (continuous enrollment lifecycle)
// ---------------------------------------------------------------------------

/// Fake `org.namtor.Manager3` service. Shares [`FakeState`] with
/// [`FakeManager`] so a test's `reset()` clears both surfaces. Implements the
/// six v3 methods with the same state-machine semantics as the daemon:
/// `Stop` moves a session from Capturing → Stopped (retains reservoir),
/// `Resume` moves Stopped → Capturing, `Review` requires Stopped,
/// `Commit` requires Stopped and produces a new profile, `Cancel` tears down.
pub struct FakeManager3 {
    state: Arc<Mutex<FakeState>>,
}

impl FakeManager3 {
    pub fn new(state: Arc<Mutex<FakeState>>) -> Self {
        Self { state }
    }

    fn denied(&self) -> Option<zbus::fdo::Error> {
        let s = self.state.lock().unwrap();
        if s.daemon_unavailable {
            Some(zbus::fdo::Error::ServiceUnknown(
                "org.namtor.Manager".to_string(),
            ))
        } else if s.auth_denied {
            Some(zbus::fdo::Error::AccessDenied("polkit denied".to_string()))
        } else {
            None
        }
    }
}

#[interface(name = "org.namtor.Manager3")]
impl FakeManager3 {
    fn start_enrollment(&self, _uid: u32, _name: &str) -> zbus::fdo::Result<String> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        s.next_session = s.next_session.wrapping_add(1);
        let session = format!("sess3-{}", s.next_session);
        s.v3_sessions.insert(session.clone(), V3Phase::Capturing);
        Ok(session)
    }

    fn cancel_enrollment(&self, session: &str) -> zbus::fdo::Result<()> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        self.state.lock().unwrap().v3_sessions.remove(session);
        Ok(())
    }

    fn commit_profile(&self, session: &str) -> zbus::fdo::Result<String> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        match s.v3_sessions.get(session) {
            Some(V3Phase::Stopped) => {}
            _ => {
                return Err(zbus::fdo::Error::Failed(
                    "session not in stopped phase".to_string(),
                ))
            }
        }
        let id = format!("p{}", s.profiles.len() + 1);
        s.profiles.push(ProfileInfo {
            id: id.clone(),
            name: name_from_session(session),
            enabled: true,
            created: 1,
            last_used: 0,
        });
        s.v3_sessions.remove(session);
        Ok(id)
    }

    fn stop_enrollment(&self, session: &str) -> zbus::fdo::Result<()> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        match s.v3_sessions.get_mut(session) {
            Some(phase) => {
                *phase = V3Phase::Stopped;
                Ok(())
            }
            None => Err(zbus::fdo::Error::Failed("session not found".into())),
        }
    }

    fn resume_enrollment(&self, session: &str) -> zbus::fdo::Result<()> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        match s.v3_sessions.get_mut(session) {
            Some(phase) => {
                *phase = V3Phase::Capturing;
                Ok(())
            }
            None => Err(zbus::fdo::Error::Failed("session not found".into())),
        }
    }

    fn review_enrollment(&self, session: &str) -> zbus::fdo::Result<EnrollmentReviewV3> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let s = self.state.lock().unwrap();
        match s.v3_sessions.get(session) {
            Some(V3Phase::Stopped) => Ok(EnrollmentReviewV3 {
                session: session.to_string(),
                ready_to_save: true,
                quality_percent: 80,
                diversity_percent: 60,
                frames_accepted: 8,
                guidance: "looking_good".to_string(),
            }),
            _ => Err(zbus::fdo::Error::Failed(
                "session not in stopped phase".into(),
            )),
        }
    }

    #[zbus(property)]
    fn api_version(&self) -> zbus::fdo::Result<u32> {
        Ok(FAKE_API_VERSION_V3)
    }

    #[zbus(signal)]
    async fn enrollment_progress(
        &self,
        _emitter: zbus::object_server::SignalEmitter<'_>,
        progress: EnrollmentProgressV3,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn enrollment_completed(
        &self,
        _emitter: zbus::object_server::SignalEmitter<'_>,
        session: String,
        profile: String,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn profile_changed(
        &self,
        _emitter: zbus::object_server::SignalEmitter<'_>,
        uid: u32,
        id: String,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_changed(
        &self,
        _emitter: zbus::object_server::SignalEmitter<'_>,
        status: StatusReport,
    ) -> zbus::Result<()>;
}

fn name_from_session(session: &str) -> String {
    session
        .strip_prefix("sess3-")
        .map(|n| format!("user{n}"))
        .unwrap_or_else(|| "Alice".to_string())
}
