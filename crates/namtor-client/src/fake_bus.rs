// SPDX-FileCopyrightText: 2026 Nam-tor Project
// SPDX-License-Identifier: GPL-3.0-only

//! Hermetic p2p `FakeBus` — an in-process `org.namtor.Manager` service for
//! testing [`crate::dbus_client::DbusManagerClient`] without a real D-Bus
//! daemon. No `DBUS_SESSION_BUS_ADDRESS` or system bus is touched.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use crate::dbus_client::DbusManagerClient;
use crate::types::{
    PamServiceCompatibility, PamServiceInfo, ProfileInfo, ProtectionKeyKind, ProtectionStatus,
    ReviewOutcome, StatusReport,
};

/// Mutable state behind the fake `org.namtor.Manager` service.
#[derive(Debug, Default)]
struct FakeState {
    profiles: Vec<ProfileInfo>,
    daemon_unavailable: bool,
    next_session: u32,
    /// The PAM services the fake reports, in display order.
    pam_services: Vec<PamServiceInfo>,
    /// Session id to the name it was started with. The real daemon carries
    /// the name from `StartEnrollment` through to the committed profile; a
    /// fake that named every profile the same made every rendered list and
    /// every rename assertion weaker than it looked.
    session_names: std::collections::HashMap<String, String>,
}

/// One service of each compatibility, which is what the Authentication page
/// has to render correctly: a safe one that applies immediately, a
/// high-impact one that asks first, and an incompatible one whose switch is
/// inert.
fn default_pam_services() -> Vec<PamServiceInfo> {
    vec![
        PamServiceInfo {
            id: "gdm-password".into(),
            display_name: "GDM Password".into(),
            enabled: false,
            compatibility: PamServiceCompatibility::Risky,
            description: "Graphical login and unlock".into(),
        },
        PamServiceInfo {
            id: "polkit-1".into(),
            display_name: "System Prompts".into(),
            enabled: false,
            compatibility: PamServiceCompatibility::Safe,
            description: "Authorisation dialogs raised by desktop apps".into(),
        },
        PamServiceInfo {
            id: "sshd".into(),
            display_name: "Remote Login".into(),
            enabled: false,
            compatibility: PamServiceCompatibility::Unsupported,
            description: "Face unlock cannot serve a remote session".into(),
        },
    ]
}

/// The v2 surface, `org.namtor.Manager2`.
struct FakeManagerV2 {
    state: Arc<Mutex<FakeState>>,
}

#[zbus::interface(name = "org.namtor.Manager2")]
impl FakeManagerV2 {
    fn list_pam_services(&self) -> zbus::fdo::Result<Vec<PamServiceInfo>> {
        let s = self.state.lock().expect("fake state poisoned");
        if s.daemon_unavailable {
            return Err(zbus::fdo::Error::ServiceUnknown(
                "org.namtor.Manager".to_string(),
            ));
        }
        Ok(s.pam_services.clone())
    }

    fn set_pam_service_enabled(&self, service_id: &str, enabled: bool) -> Result<bool, FakeError> {
        let mut s = self.state.lock().expect("fake state poisoned");
        match s.pam_services.iter_mut().find(|p| p.id == service_id) {
            Some(p) if p.compatibility == PamServiceCompatibility::Unsupported => {
                Err(FakeError::Unsupported(service_id.to_string()))
            }
            Some(p) => {
                p.enabled = enabled;
                Ok(true)
            }
            None => Err(FakeError::NotFound(service_id.to_string())),
        }
    }

    fn get_protection_status(&self) -> zbus::fdo::Result<ProtectionStatus> {
        Ok(ProtectionStatus {
            sealing: ProtectionKeyKind::Tpm2,
            has_tpm: true,
            warning: String::new(),
        })
    }

    #[zbus(property, name = "ApiVersion")]
    fn api_version(&self) -> u32 {
        2
    }
}

/// The error names the real daemon raises, so the client's `map_zbus_error`
/// is exercised on a real message rather than on a hand-built one. A fake
/// that raised `org.freedesktop.DBus.Error.*` instead would have every
/// method error arrive as `ManagerError::Transport`, which the CLI presents
/// as a daemon problem.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.namtor.Error")]
pub enum FakeError {
    /// Transport-level failure, as zbus requires.
    #[zbus(error)]
    ZBus(zbus::Error),
    /// `org.namtor.Error.NotFound`.
    NotFound(String),
    /// `org.namtor.Error.Unsupported`.
    Unsupported(String),
}

/// The zbus interface implementation for the v1 `org.namtor.Manager` surface.
struct FakeManagerV1 {
    state: Arc<Mutex<FakeState>>,
}

#[zbus::interface(name = "org.namtor.Manager")]
impl FakeManagerV1 {
    fn list_profiles(&self) -> zbus::fdo::Result<Vec<ProfileInfo>> {
        let s = self.state.lock().expect("fake state poisoned");
        if s.daemon_unavailable {
            return Err(zbus::fdo::Error::ServiceUnknown(
                "org.namtor.Manager".to_string(),
            ));
        }
        Ok(s.profiles.clone())
    }

    fn start_enrollment(&self, _uid: u32, name: &str) -> zbus::fdo::Result<String> {
        let mut s = self.state.lock().expect("fake state poisoned");
        s.next_session = s.next_session.wrapping_add(1);
        let session = format!("sess-{name}-{}", s.next_session);
        s.session_names.insert(session.clone(), name.to_string());
        Ok(session)
    }

    fn cancel_enrollment(&self, _session: &str) -> zbus::fdo::Result<()> {
        Ok(())
    }

    fn commit_profile(&self, session: &str) -> zbus::fdo::Result<String> {
        let mut s = self.state.lock().expect("fake state poisoned");
        let id = format!("p{}", s.profiles.len() + 1);
        let name = s
            .session_names
            .remove(session)
            .unwrap_or_else(|| "Alice".to_string());
        s.profiles.push(ProfileInfo {
            id: id.clone(),
            name,
            enabled: true,
            created: 1,
            last_used: 0,
        });
        Ok(id)
    }

    fn rename_profile(&self, _uid: u32, id: &str, new_name: &str) -> zbus::fdo::Result<()> {
        let mut s = self.state.lock().expect("fake state poisoned");
        for p in s.profiles.iter_mut() {
            if p.id == id {
                p.name = new_name.to_string();
            }
        }
        Ok(())
    }

    fn enable_profile(&self, _uid: u32, id: &str, enabled: bool) -> zbus::fdo::Result<()> {
        let mut s = self.state.lock().expect("fake state poisoned");
        for p in s.profiles.iter_mut() {
            if p.id == id {
                p.enabled = enabled;
            }
        }
        Ok(())
    }

    fn delete_profile(&self, _uid: u32, id: &str) -> zbus::fdo::Result<()> {
        let mut s = self.state.lock().expect("fake state poisoned");
        s.profiles.retain(|p| p.id != id);
        Ok(())
    }

    fn review_profile(&self, _uid: u32, id: &str) -> Result<ReviewOutcome, FakeError> {
        let s = self.state.lock().expect("fake state poisoned");
        match s.profiles.iter().find(|p| p.id == id) {
            Some(p) => Ok(ReviewOutcome {
                profile_id: p.id.clone(),
                needs_reenroll: false,
                quality: crate::types::QualityLabel::High,
            }),
            None => Err(FakeError::NotFound(id.to_string())),
        }
    }

    fn merge_profiles(&self, _uid: u32, src: &str, dst: &str) -> Result<(), FakeError> {
        let mut s = self.state.lock().expect("fake state poisoned");
        if !s.profiles.iter().any(|p| p.id == dst) {
            return Err(FakeError::NotFound(dst.to_string()));
        }
        s.profiles.retain(|p| p.id != src);
        Ok(())
    }

    fn replace_profile(&self, _uid: u32, id: &str, name: &str) -> Result<(), FakeError> {
        let mut s = self.state.lock().expect("fake state poisoned");
        match s.profiles.iter_mut().find(|p| p.id == id) {
            Some(p) => {
                p.name = name.to_string();
                Ok(())
            }
            None => Err(FakeError::NotFound(id.to_string())),
        }
    }

    fn get_status(&self) -> zbus::fdo::Result<StatusReport> {
        let s = self.state.lock().expect("fake state poisoned");
        Ok(StatusReport {
            api_version: 1,
            profiles: s.profiles.clone(),
            emitter_armed: false,
        })
    }

    #[zbus(property)]
    fn api_version(&self) -> zbus::fdo::Result<u32> {
        Ok(1)
    }
}

/// A live p2p fake bus: owns the server connection and a ready-to-use
/// [`DbusManagerClient`] wired to the client side of the socket pair.
pub struct FakeBus {
    /// Server-side connection; kept alive so the service stays up.
    pub server: zbus::Connection,
    /// A `DbusManagerClient` wired to the client side of the p2p pair.
    pub client: DbusManagerClient,
    /// The client-side connection, kept so a test can build a second client
    /// on the same bus and read back what the first one wrote. `client` is
    /// usually moved into whatever is under test.
    client_conn: zbus::blocking::Connection,
}

impl FakeBus {
    /// A second client on the same connection. Lets a test observe the effect
    /// of a call made by the client it handed to the code under test.
    pub fn observer(&self) -> DbusManagerClient {
        DbusManagerClient::from_connection(self.client_conn.clone())
    }

    /// Spawn a fresh in-process fake bus. No real D-Bus daemon required.
    ///
    /// The server connection is built on a background thread so its handshake
    /// can proceed concurrently with the client's handshake on this thread.
    /// Both sides of a p2p pair must be built simultaneously — building one
    /// before the other deadlocks the SASL handshake.
    pub fn spawn() -> Result<Self, zbus::Error> {
        let (a, b) = UnixStream::pair().map_err(|e| zbus::Error::Failure(e.to_string()))?;
        let state = Arc::new(Mutex::new(FakeState {
            pam_services: default_pam_services(),
            ..FakeState::default()
        }));
        let guid = zbus::Guid::generate();

        let (tx, rx) = std::sync::mpsc::channel::<Result<zbus::Connection, zbus::Error>>();
        std::thread::Builder::new()
            .name("fake_bus_server".into())
            .spawn(move || {
                let result = zbus::block_on(async {
                    zbus::connection::Builder::unix_stream(a)
                        .server(guid)
                        .map_err(|e| zbus::Error::Failure(e.to_string()))?
                        .p2p()
                        .serve_at(
                            "/org/namtor/Manager",
                            FakeManagerV1 {
                                state: Arc::clone(&state),
                            },
                        )?
                        .serve_at("/org/namtor/Manager", FakeManagerV2 { state })?
                        .build()
                        .await
                });
                let _ = tx.send(result);
            })
            .map_err(|e| zbus::Error::Failure(e.to_string()))?;

        let client_conn = zbus::blocking::connection::Builder::unix_stream(b)
            .p2p()
            .build()?;

        let server = rx
            .recv()
            .map_err(|e| zbus::Error::Failure(e.to_string()))??;
        let client = DbusManagerClient::from_connection(client_conn.clone());
        Ok(Self {
            server,
            client,
            client_conn,
        })
    }
}
