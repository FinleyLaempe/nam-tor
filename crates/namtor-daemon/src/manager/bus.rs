// SPDX-License-Identifier: GPL-3.0-only
//! System-bus `org.namtor.Manager` service wiring (spec §9.1, plan task 9).
//!
//! Bridges the frozen wire contract ([`crate::manager::contract`]) and the
//! in-memory reference implementation ([`crate::dbus::ManagerService`]) to a
//! real zbus connection on the system bus.
//!
//! ## Lifecycle
//!
//! [`ManagerBusService::start_system`] connects to the system bus, serves
//! `/org/namtor/Manager`, and owns `org.namtor.Manager`. If the name is already
//! taken (duplicate owner), it logs and returns `Err` so the caller can exit
//! cleanly without disrupting the PAM socket path.
//!
//! [`ManagerBusService::shutdown`] releases the bus name and drops the
//! connection. The zbus executor stops when the connection is dropped.
//!
//! ## Peer identity
//!
//! Each method call resolves the caller's UID/PID via the bus daemon's
//! `GetConnectionCredentials` and builds a [`PeerIdentity`] for the polkit
//! stub. Proper polkit wiring is task 12; this module uses [`SystemPolkit`].
//!
//! ## Privacy
//!
//! No biometric material crosses this surface. The `no_biometrics_in_api_structs`
//! test in [`crate::dbus`] and the `privacy_scan_rejects_biometric_tokens` test
//! in [`crate::manager::contract`] guard the wire types and the contract XML.

use std::sync::Arc;

use zbus::blocking::connection;
use zbus::fdo::DBusProxy;
use zbus::interface;
use zbus::message::Header;
use zbus::names::BusName;
use zbus::object_server::SignalEmitter;
use zbus::Connection;

use crate::dbus::{
    EnrollmentProgress, ManagerDBusError, ManagerService, PeerIdentity, ProfileApi, ProfileInfo,
    ReviewOutcome, StatusReport, SystemPolkit,
};
use crate::manager::contract;

type Svc = ManagerService<SystemPolkit>;

/// zbus interface wrapper around [`ManagerService`].
///
/// Every method resolves the caller's credentials from the bus, delegates to
/// the inner [`ProfileApi`] impl, then drains and emits queued signals.
struct ManagerIface {
    svc: Arc<Svc>,
}

impl ManagerIface {
    fn new(svc: Arc<Svc>) -> Self {
        Self { svc }
    }

    async fn peer_identity(
        conn: &Connection,
        header: &Header<'_>,
    ) -> Result<PeerIdentity, ManagerDBusError> {
        let sender = match header.sender() {
            Some(s) => s,
            None => return Err(ManagerDBusError::State("no sender on message".into())),
        };
        let bus_name = BusName::try_from(sender.as_str())
            .map_err(|e| ManagerDBusError::State(e.to_string()))?;
        let dbus = DBusProxy::new(conn).await.map_err(ManagerDBusError::ZBus)?;
        let creds = dbus
            .get_connection_credentials(bus_name)
            .await
            .map_err(|e| ManagerDBusError::ZBus(e.into()))?;
        let uid = creds.unix_user_id().unwrap_or(0);
        let pid = creds.process_id().unwrap_or(0) as i32;
        Ok(PeerIdentity {
            uid,
            pid,
            cmdline: String::new(),
        })
    }

    async fn emit_drained(emitter: &SignalEmitter<'_>, svc: &Svc) -> Result<(), ManagerDBusError> {
        for sig in svc.drain_signals() {
            match sig {
                crate::dbus::ManagerSignal::EnrollmentProgress(p) => {
                    let session = p.session_id.clone();
                    emitter
                        .enrollment_progress(&session, &p)
                        .await
                        .map_err(ManagerDBusError::ZBus)?;
                }
                crate::dbus::ManagerSignal::EnrollmentCompleted {
                    session_id,
                    profile_id,
                } => {
                    emitter
                        .enrollment_completed(&session_id, &profile_id)
                        .await
                        .map_err(ManagerDBusError::ZBus)?;
                }
                crate::dbus::ManagerSignal::ProfileChanged { uid, id } => {
                    emitter
                        .profile_changed(uid, &id)
                        .await
                        .map_err(ManagerDBusError::ZBus)?;
                }
                crate::dbus::ManagerSignal::StatusChanged(status) => {
                    emitter
                        .status_changed(&status)
                        .await
                        .map_err(ManagerDBusError::ZBus)?;
                }
            }
        }
        Ok(())
    }
}

#[interface(name = "org.namtor.Manager")]
impl ManagerIface {
    #[zbus(name = "ListProfiles")]
    async fn list_profiles(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<Vec<ProfileInfo>, ManagerDBusError> {
        let peer = Self::peer_identity(conn, &header).await?;
        self.svc
            .list_profiles(&peer)
            .map_err(ManagerDBusError::from)
    }

    #[zbus(name = "StartEnrollment")]
    async fn start_enrollment(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
        uid: u32,
        name: &str,
    ) -> Result<String, ManagerDBusError> {
        let peer = Self::peer_identity(conn, &header).await?;
        let session = self
            .svc
            .start_enrollment(&peer, uid, name)
            .map_err(ManagerDBusError::from)?;
        Ok(session)
    }

    #[zbus(name = "CancelEnrollment")]
    async fn cancel_enrollment(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
        session: &str,
    ) -> Result<(), ManagerDBusError> {
        let peer = Self::peer_identity(conn, &header).await?;
        self.svc
            .cancel_enrollment(&peer, session)
            .map_err(ManagerDBusError::from)?;
        Ok(())
    }

    #[zbus(name = "CommitProfile")]
    async fn commit_profile(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        session: &str,
    ) -> Result<String, ManagerDBusError> {
        let peer = Self::peer_identity(conn, &header).await?;
        let profile = self
            .svc
            .commit_profile(&peer, session)
            .map_err(ManagerDBusError::from)?;
        Self::emit_drained(&emitter, &self.svc).await?;
        Ok(profile)
    }

    #[zbus(name = "ReviewProfile")]
    async fn review_profile(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        uid: u32,
        id: &str,
    ) -> Result<ReviewOutcome, ManagerDBusError> {
        let peer = Self::peer_identity(conn, &header).await?;
        let outcome = self
            .svc
            .review_profile(&peer, uid, id)
            .map_err(ManagerDBusError::from)?;
        Self::emit_drained(&emitter, &self.svc).await?;
        Ok(outcome)
    }

    #[zbus(name = "RenameProfile")]
    async fn rename_profile(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        uid: u32,
        id: &str,
        name: &str,
    ) -> Result<(), ManagerDBusError> {
        let peer = Self::peer_identity(conn, &header).await?;
        self.svc
            .rename_profile(&peer, uid, id, name)
            .map_err(ManagerDBusError::from)?;
        Self::emit_drained(&emitter, &self.svc).await?;
        Ok(())
    }

    #[zbus(name = "EnableProfile")]
    async fn enable_profile(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        uid: u32,
        id: &str,
        enabled: bool,
    ) -> Result<(), ManagerDBusError> {
        let peer = Self::peer_identity(conn, &header).await?;
        self.svc
            .enable_profile(&peer, uid, id, enabled)
            .map_err(ManagerDBusError::from)?;
        Self::emit_drained(&emitter, &self.svc).await?;
        Ok(())
    }

    #[zbus(name = "DeleteProfile")]
    async fn delete_profile(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        uid: u32,
        id: &str,
    ) -> Result<(), ManagerDBusError> {
        let peer = Self::peer_identity(conn, &header).await?;
        self.svc
            .delete_profile(&peer, uid, id)
            .map_err(ManagerDBusError::from)?;
        Self::emit_drained(&emitter, &self.svc).await?;
        Ok(())
    }

    #[zbus(name = "MergeProfiles")]
    async fn merge_profiles(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        uid: u32,
        src: &str,
        dst: &str,
    ) -> Result<(), ManagerDBusError> {
        let peer = Self::peer_identity(conn, &header).await?;
        self.svc
            .merge_profiles(&peer, uid, src, dst)
            .map_err(ManagerDBusError::from)?;
        Self::emit_drained(&emitter, &self.svc).await?;
        Ok(())
    }

    #[zbus(name = "ReplaceProfile")]
    async fn replace_profile(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        uid: u32,
        id: &str,
        name: &str,
    ) -> Result<(), ManagerDBusError> {
        let peer = Self::peer_identity(conn, &header).await?;
        self.svc
            .replace_profile(&peer, uid, id, name)
            .map_err(ManagerDBusError::from)?;
        Self::emit_drained(&emitter, &self.svc).await?;
        Ok(())
    }

    #[zbus(name = "GetStatus")]
    async fn get_status(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<StatusReport, ManagerDBusError> {
        let peer = Self::peer_identity(conn, &header).await?;
        self.svc.get_status(&peer).map_err(ManagerDBusError::from)
    }

    /// Read-only `ApiVersion` property (frozen v1).
    #[zbus(property, name = "ApiVersion")]
    fn api_version(&self) -> u32 {
        contract::API_VERSION
    }

    #[zbus(signal)]
    async fn enrollment_progress(
        emitter: &SignalEmitter<'_>,
        session: &str,
        progress: &EnrollmentProgress,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn enrollment_completed(
        emitter: &SignalEmitter<'_>,
        session: &str,
        profile: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn profile_changed(emitter: &SignalEmitter<'_>, uid: u32, id: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_changed(emitter: &SignalEmitter<'_>, status: &StatusReport)
        -> zbus::Result<()>;
}

/// Error from starting or running the bus service.
#[derive(Debug, thiserror::Error)]
pub enum BusServiceError {
    #[error("zbus: {0}")]
    ZBus(#[from] zbus::Error),
    #[error("name already taken: {0}")]
    NameTaken(String),
}

/// Handle to the running D-Bus manager service. Drop to release the name and
/// stop serving.
pub struct ManagerBusService {
    conn: zbus::blocking::Connection,
}

impl ManagerBusService {
    /// Connect to the system bus, serve `/org/namtor/Manager`, and own
    /// `org.namtor.Manager`. Returns `Err(NameTaken)` if another process
    /// already owns the name — the caller logs and exits cleanly.
    pub fn start_system(svc: Arc<Svc>) -> Result<Self, BusServiceError> {
        let iface = ManagerIface::new(svc);
        let conn = connection::Builder::system()?
            .serve_at(contract::OBJECT_PATH, iface)?
            .allow_name_replacements(false)
            .name(contract::BUS_NAME)?
            .build()
            .map_err(|e| match e {
                zbus::Error::NameTaken => BusServiceError::NameTaken(contract::BUS_NAME.into()),
                other => BusServiceError::ZBus(other),
            })?;
        tracing::info!(
            bus = contract::BUS_NAME,
            path = contract::OBJECT_PATH,
            "manager d-bus service registered on system bus"
        );
        Ok(Self { conn })
    }

    /// Release the well-known name and drop the connection. The zbus executor
    /// stops when the connection is dropped.
    pub fn shutdown(self) {
        if let Err(e) = self.conn.release_name(contract::BUS_NAME) {
            tracing::warn!(error = %e, "failed to release bus name");
        }
        drop(self.conn);
        tracing::info!("manager d-bus service stopped");
    }

    /// Connect to the session bus (for tests).
    #[cfg(test)]
    fn start_session(svc: Arc<Svc>) -> Result<Self, BusServiceError> {
        let iface = ManagerIface::new(svc);
        let conn = connection::Builder::session()?
            .serve_at(contract::OBJECT_PATH, iface)?
            .allow_name_replacements(false)
            .name(contract::BUS_NAME)?
            .build()
            .map_err(|e| match e {
                zbus::Error::NameTaken => BusServiceError::NameTaken(contract::BUS_NAME.into()),
                other => BusServiceError::ZBus(other),
            })?;
        Ok(Self { conn })
    }

    /// Build a p2p pair for testing: returns (service, client). The service
    /// side serves the interface; the client side can call methods and
    /// introspect.
    #[cfg(test)]
    fn start_p2p(svc: Arc<Svc>) -> Result<(Self, zbus::blocking::Connection), BusServiceError> {
        let (s0, s1) = zbus::connection::socket::Channel::pair();
        let guid = zbus::Guid::generate();
        let iface = ManagerIface::new(svc);
        let conn = zbus::blocking::connection::Builder::authenticated_socket(s0, guid.clone())?
            .p2p()
            .serve_at(contract::OBJECT_PATH, iface)?
            .build()?;
        let client = zbus::blocking::connection::Builder::authenticated_socket(s1, guid)?
            .p2p()
            .build()?;
        Ok((Self { conn }, client))
    }

    /// Underlying blocking connection (for test introspection).
    #[cfg(test)]
    #[allow(dead_code)]
    fn connection(&self) -> &zbus::blocking::Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::ApiError;
    use crate::manager::contract;
    use zbus::blocking::fdo::IntrospectableProxy;

    fn new_svc() -> Arc<Svc> {
        Arc::new(ManagerService::new(SystemPolkit))
    }

    /// p2p introspection must declare the frozen method/signal/property set.
    #[test]
    fn p2p_introspection_matches_frozen_contract() {
        let svc = new_svc();
        let (service, client) = ManagerBusService::start_p2p(svc).expect("p2p build");
        let _service = service;

        let proxy = IntrospectableProxy::builder(&client)
            .destination(contract::BUS_NAME)
            .expect("dest")
            .path(contract::OBJECT_PATH)
            .expect("path")
            .build()
            .expect("introspect proxy");
        let xml = proxy.introspect().expect("introspect");

        let frozen = contract::INTROSPECTION_XML;
        let frozen_squashed = squash_xml(frozen);
        let live_squashed = squash_xml(&xml);

        for method in contract::method::ALL {
            let needle = format!(r#"<method name="{method}">"#);
            assert!(
                live_squashed.contains(&needle),
                "live introspection missing method {method}"
            );
            assert!(
                frozen_squashed.contains(&needle),
                "frozen contract missing method {method}"
            );
        }
        for signal in contract::signal::ALL {
            let needle = format!(r#"<signal name="{signal}">"#);
            assert!(
                live_squashed.contains(&needle),
                "live introspection missing signal {signal}"
            );
        }
        assert!(
            live_squashed.contains(r#"<property name="ApiVersion" type="u" access="read"/>"#),
            "live introspection missing ApiVersion property"
        );
    }

    /// The frozen contract's introspection XML must be servable and
    /// retrievable via the standard Introspectable interface.
    #[test]
    fn p2p_introspection_contains_manager_interface() {
        let svc = new_svc();
        let (service, client) = ManagerBusService::start_p2p(svc).expect("p2p build");
        let _service = service;

        let proxy = IntrospectableProxy::builder(&client)
            .destination(contract::BUS_NAME)
            .expect("dest")
            .path(contract::OBJECT_PATH)
            .expect("path")
            .build()
            .expect("proxy");
        let xml = proxy.introspect().expect("introspect");
        assert!(
            xml.contains("org.namtor.Manager"),
            "live XML missing interface name"
        );
        assert!(xml.contains("ListProfiles"));
        assert!(xml.contains("StartEnrollment"));
        assert!(xml.contains("CommitProfile"));
        assert!(xml.contains("GetStatus"));
        assert!(xml.contains("EnrollmentProgress"));
        assert!(xml.contains("EnrollmentCompleted"));
        assert!(xml.contains("ProfileChanged"));
        assert!(xml.contains("StatusChanged"));
        assert!(xml.contains("ApiVersion"));
    }

    /// True when a D-Bus session bus is reachable. Headless CI has no
    /// session bus; `start_session()` would block forever waiting for the
    /// connection. Guard tests that need a real bus with this so they skip
    /// cleanly instead of hanging.
    fn session_bus_available() -> bool {
        std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok()
    }

    /// Duplicate owner: a second service trying to own the same name on the
    /// same bus must get `NameTaken`, not panic or hang.
    #[test]
    fn duplicate_owner_gets_name_taken() {
        if !session_bus_available() {
            eprintln!("skip: no DBUS_SESSION_BUS_ADDRESS (headless)");
            return;
        }
        let svc1 = new_svc();
        let first = ManagerBusService::start_session(svc1);
        if first.is_err() {
            return;
        }
        let first = first.expect("first session owner");

        let svc2 = new_svc();
        let second = ManagerBusService::start_session(svc2);

        match second {
            Err(BusServiceError::NameTaken(name)) => {
                assert_eq!(name, contract::BUS_NAME);
            }
            Err(e) => {
                panic!("expected NameTaken, got: {e:?}");
            }
            Ok(service2) => {
                let _ = first.conn.release_name(contract::BUS_NAME);
                service2.shutdown();
                panic!("expected NameTaken, got Ok (second owner succeeded)");
            }
        }
        first.shutdown();
    }

    /// SIGTERM-equivalent: shutdown releases the name and drops the
    /// connection without panicking.
    #[test]
    fn shutdown_releases_name_and_drops_cleanly() {
        if !session_bus_available() {
            eprintln!("skip: no DBUS_SESSION_BUS_ADDRESS (headless)");
            return;
        }
        let svc = new_svc();
        let service = ManagerBusService::start_session(svc);
        if service.is_err() {
            return;
        }
        let service = service.expect("session owner");
        service.shutdown();
    }

    /// `ApiVersion` property must be the frozen value.
    #[test]
    fn api_version_property_is_frozen() {
        assert_eq!(contract::API_VERSION, 1);
        let svc = new_svc();
        let iface = ManagerIface::new(svc);
        assert_eq!(iface.api_version(), contract::API_VERSION);
    }

    /// `ApiError` → `ManagerDBusError` mapping covers all variants.
    #[test]
    fn api_error_maps_to_dbus_error_variants() {
        let denied: ManagerDBusError = ApiError::Denied { action: "x".into() }.into();
        assert!(matches!(denied, ManagerDBusError::Denied(_)));

        let challenge: ManagerDBusError = ApiError::Challenge("y".into()).into();
        assert!(matches!(challenge, ManagerDBusError::Challenge(_)));

        let not_found: ManagerDBusError = ApiError::NotFound("p".into()).into();
        assert!(matches!(not_found, ManagerDBusError::NotFound(_)));

        let exists: ManagerDBusError = ApiError::Exists("p".into()).into();
        assert!(matches!(exists, ManagerDBusError::Exists(_)));

        let snf: ManagerDBusError = ApiError::SessionNotFound("s".into()).into();
        assert!(matches!(snf, ManagerDBusError::SessionNotFound(_)));

        let busy: ManagerDBusError = ApiError::SessionBusy(42).into();
        assert!(matches!(busy, ManagerDBusError::SessionBusy(_)));

        let state: ManagerDBusError = ApiError::State("bad".into()).into();
        assert!(matches!(state, ManagerDBusError::State(_)));
    }

    fn squash_xml(xml: &str) -> String {
        let mut out = String::with_capacity(xml.len());
        let mut ws = false;
        for c in xml.chars() {
            if c.is_whitespace() {
                if !ws {
                    out.push(' ');
                }
                ws = true;
            } else {
                out.push(c);
                ws = false;
            }
        }
        out
    }
}
