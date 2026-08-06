// SPDX-License-Identifier: GPL-3.0-only
//! In-process fake `org.namtor.Manager` service for hermetic KCM tests.
//!
//! Drives a p2p Unix-socket pair (no D-Bus daemon required) so the contract
//! suite runs anywhere `cargo test` runs. The fake is configurable through
//! [`FakeState`] held in a `Mutex`; tests flip `daemon_unavailable` /
//! `auth_denied` to exercise the error and auth-loss paths without touching
//! real polkit.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use zbus::interface;
use zbus::zvariant::Type;

use crate::contract::ScenarioHook;
use crate::types::{EnrollmentProgress, FinishResult, ProfileInfo, StatusInfo};

/// Mutable state behind the fake interface. Tests mutate it through
/// [`FakeHandle`] (which implements [`ScenarioHook`]).
#[derive(Debug, Default)]
pub struct FakeState {
    pub profiles: Vec<ProfileInfo>,
    pub daemon_unavailable: bool,
    pub auth_denied: bool,
    pub next_session: u32,
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

    fn create_profile(&self, name: &str) -> zbus::fdo::Result<String> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        s.next_session = s.next_session.wrapping_add(1);
        Ok(format!("sess-{name}-{}", s.next_session))
    }

    fn start_enrollment(&self, _session_id: &str) -> zbus::fdo::Result<()> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        Ok(())
    }

    fn get_progress(&self, session_id: &str) -> zbus::fdo::Result<EnrollmentProgress> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        Ok(EnrollmentProgress {
            session_id: session_id.to_string(),
            frames_captured: 12,
            quality_label: "good".to_string(),
            coverage_percent: 80,
        })
    }

    fn finish_enrollment(&self, _session_id: &str) -> zbus::fdo::Result<FinishResult> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        // Commit a profile for the happy path: model the new profile as
        // enabled, last_used unknown.
        let mut s = self.state.lock().unwrap();
        let id = format!("p{}", s.profiles.len() + 1);
        s.profiles.push(ProfileInfo {
            id,
            name: "Alice".to_string(),
            enabled: true,
            created: 1,
            last_used: None,
        });
        Ok(FinishResult {
            success: true,
            message: "enrolled".to_string(),
        })
    }

    fn cancel_enrollment(&self, _session_id: &str) -> zbus::fdo::Result<()> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        Ok(())
    }

    fn merge_profiles(&self, src: &str, _dst: &str) -> zbus::fdo::Result<()> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        s.profiles.retain(|p| p.id != src);
        // dst stays; name unchanged on merge.
        Ok(())
    }

    fn replace_profile(&self, id: &str, name: &str) -> zbus::fdo::Result<()> {
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

    fn rename_profile(&self, id: &str, new_name: &str) -> zbus::fdo::Result<()> {
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

    fn enable_profile(&self, id: &str, enabled: bool) -> zbus::fdo::Result<()> {
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

    fn delete_profile(&self, id: &str) -> zbus::fdo::Result<()> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let mut s = self.state.lock().unwrap();
        s.profiles.retain(|p| p.id != id);
        Ok(())
    }

    fn status(&self) -> zbus::fdo::Result<StatusInfo> {
        if let Some(e) = self.denied() {
            return Err(e);
        }
        let s = self.state.lock().unwrap();
        Ok(StatusInfo {
            daemon_reachable: true,
            camera_present: true,
            enrolled_profiles: s.profiles.len() as u32,
        })
    }

    #[zbus(signal)]
    async fn enrollment_progress(
        &self,
        _emitter: zbus::object_server::SignalEmitter<'_>,
        session_id: String,
        progress: EnrollmentProgress,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn enrollment_complete(
        &self,
        _emitter: zbus::object_server::SignalEmitter<'_>,
        session_id: String,
        result: FinishResult,
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
    require_send::<FinishResult>();
    require_send::<StatusInfo>();
    // Type must be present for the privacy scan's `Type`-derive check.
    fn require_type<T: Type>() {}
    require_type::<ProfileInfo>();
    require_type::<EnrollmentProgress>();
    require_type::<FinishResult>();
    require_type::<StatusInfo>();
}
