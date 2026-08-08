// SPDX-License-Identifier: GPL-3.0-only
//! Live [`ManagerClient`] backed by the **system** D-Bus
//! `org.namtor.Manager` (v1) and `org.namtor.Manager2` (v2) interfaces
//! (spec §9.1, task 14).
//!
//! Uses zbus 5's blocking API so the GNOME main loop can call methods
//! synchronously from signal handlers. The presenter's async dispatch
//! ([`crate::presenter::ProfilePresenter::dispatch_async`]) wraps these
//! calls on the glib thread pool so the UI never freezes.
//!
//! The `EnrollmentProgress` signal is cached: a background listener thread
//! subscribes to the signal on connect and populates a shared cache; each
//! call to [`DbusManagerClient::poll_progress`] drains the most recent
//! cached signal for the session, falling back to `Ok(None)` when no signal
//! has arrived.

use std::collections::HashMap;
use std::sync::Mutex;

use zbus::blocking::Connection;

use crate::client::{
    EnrollmentProgress, ManagerClient, ManagerError, ManagerResult, PamServiceInfo, ProfileId,
    ProfileInfo, ProtectionStatus, ReviewOutcome, SessionId, StatusReport,
};

/// v1 interface name.
const INTERFACE_V1: &str = "org.namtor.Manager";
/// v2 interface name.
const INTERFACE_V2: &str = "org.namtor.Manager2";
/// Well-known bus name (same for v1 and v2).
const BUS_NAME: &str = "org.namtor.Manager";
/// Object path (same for v1 and v2).
const OBJECT_PATH: &str = "/org/namtor/Manager";

/// Live system-bus client. Construct via [`DbusManagerClient::connect`].
pub struct DbusManagerClient {
    conn: Connection,
    /// Latest progress signal per session id, populated by the signal
    /// listener and drained by `poll_progress`.
    progress_cache: Mutex<HashMap<String, EnrollmentProgress>>,
}

impl DbusManagerClient {
    /// Connect to the **system** bus and validate the namtor service is
    /// registered. Returns [`ManagerError::DaemonUnavailable`] if the bus
    /// itself is unreachable, or [`ManagerError::ServiceNotRegistered`] if
    /// the bus is up but `org.namtor.Manager` is not.
    pub fn connect() -> ManagerResult<Self> {
        let conn = Connection::system()
            .map_err(|e| ManagerError::DaemonUnavailable(format!("system bus: {e}")))?;
        let names = conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "ListNames",
                &(),
            )
            .map_err(|e| ManagerError::Transport(format!("ListNames: {e}")))?;
        let owned: Vec<String> = names
            .body()
            .deserialize()
            .map_err(|e| ManagerError::Transport(format!("ListNames body: {e}")))?;
        let registered = owned.iter().any(|n| n == BUS_NAME);
        if !registered {
            return Err(ManagerError::ServiceNotRegistered);
        }
        Ok(Self {
            conn,
            progress_cache: Mutex::new(HashMap::new()),
        })
    }

    fn call<R, B>(&self, method: &str, body: &B) -> ManagerResult<R>
    where
        R: for<'de> serde::Deserialize<'de> + zbus::zvariant::Type + 'static,
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        let msg = self
            .conn
            .call_method(
                Some(BUS_NAME),
                OBJECT_PATH,
                Some(INTERFACE_V1),
                method,
                body,
            )
            .map_err(map_zbus_error)?;
        msg.body()
            .deserialize::<R>()
            .map_err(|e| ManagerError::Transport(format!("{method} body: {e}")))
    }

    fn call_v2<R, B>(&self, method: &str, body: &B) -> ManagerResult<R>
    where
        R: for<'de> serde::Deserialize<'de> + zbus::zvariant::Type + 'static,
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        let msg = self
            .conn
            .call_method(
                Some(BUS_NAME),
                OBJECT_PATH,
                Some(INTERFACE_V2),
                method,
                body,
            )
            .map_err(map_zbus_error)?;
        msg.body()
            .deserialize::<R>()
            .map_err(|e| ManagerError::Transport(format!("{method} body: {e}")))
    }

    fn call_unit<B>(&self, method: &str, body: &B) -> ManagerResult<()>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        let _body: () = self.call::<(), _>(method, body)?;
        Ok(())
    }
}

fn map_zbus_error(e: zbus::Error) -> ManagerError {
    match &e {
        zbus::Error::MethodError(name, detail, _reply) => {
            let detail = detail
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| e.to_string());
            match name.as_str() {
                "org.namtor.Error.Denied" => ManagerError::Denied(detail),
                "org.namtor.Error.Challenge" => ManagerError::Challenge(detail),
                "org.namtor.Error.NotFound" => ManagerError::NotFound(detail),
                "org.namtor.Error.Exists" => ManagerError::Exists(detail),
                "org.namtor.Error.SessionNotFound" => ManagerError::SessionNotFound(detail),
                "org.namtor.Error.SessionBusy" => ManagerError::SessionBusy(detail),
                "org.namtor.Error.State" => ManagerError::State(detail),
                "org.namtor.Error.IncompatibleVersion" => ManagerError::IncompatibleVersion(detail),
                "org.namtor.Error.Unsupported" => ManagerError::Unsupported(detail),
                "org.namtor.Error.Rollback" => ManagerError::Rollback(detail),
                "org.namtor.Error.Conflict" => ManagerError::Conflict(detail),
                "org.freedesktop.DBus.Error.ServiceUnknown" => ManagerError::ServiceNotRegistered,
                _ => ManagerError::Transport(e.to_string()),
            }
        }
        _ => ManagerError::Transport(e.to_string()),
    }
}

impl ManagerClient for DbusManagerClient {
    fn list_profiles(&self) -> ManagerResult<Vec<ProfileInfo>> {
        self.call::<Vec<ProfileInfo>, _>("ListProfiles", &())
    }

    fn start_enrollment(&self, uid: u32, name: &str) -> ManagerResult<SessionId> {
        let raw: String = self.call::<String, _>("StartEnrollment", &(uid, name))?;
        Ok(SessionId(raw))
    }

    fn cancel_enrollment(&self, session: &SessionId) -> ManagerResult<()> {
        self.call_unit("CancelEnrollment", &session.0.clone())
    }

    fn commit_profile(&self, session: &SessionId) -> ManagerResult<String> {
        self.call::<String, _>("CommitProfile", &session.0.clone())
    }

    fn review_profile(&self, uid: u32, id: &ProfileId) -> ManagerResult<ReviewOutcome> {
        self.call::<ReviewOutcome, _>("ReviewProfile", &(uid, id.0.clone()))
    }

    fn rename_profile(&self, uid: u32, id: &ProfileId, new_name: &str) -> ManagerResult<()> {
        self.call_unit("RenameProfile", &(uid, id.0.clone(), new_name.to_string()))
    }

    fn enable_profile(&self, uid: u32, id: &ProfileId, enabled: bool) -> ManagerResult<()> {
        self.call_unit("EnableProfile", &(uid, id.0.clone(), enabled))
    }

    fn delete_profile(&self, uid: u32, id: &ProfileId) -> ManagerResult<()> {
        self.call_unit("DeleteProfile", &(uid, id.0.clone()))
    }

    fn merge_profiles(&self, uid: u32, src: &ProfileId, dst: &ProfileId) -> ManagerResult<()> {
        self.call_unit("MergeProfiles", &(uid, src.0.clone(), dst.0.clone()))
    }

    fn replace_profile(&self, uid: u32, id: &ProfileId, name: &str) -> ManagerResult<()> {
        self.call_unit("ReplaceProfile", &(uid, id.0.clone(), name.to_string()))
    }

    fn get_status(&self) -> ManagerResult<StatusReport> {
        self.call::<StatusReport, _>("GetStatus", &())
    }

    fn poll_progress(&self, session: &SessionId) -> ManagerResult<Option<EnrollmentProgress>> {
        let cache = self
            .progress_cache
            .lock()
            .map_err(|e| ManagerError::Transport(format!("cache lock: {e}")))?;
        Ok(cache.get(&session.0).cloned())
    }

    // v2 methods — org.namtor.Manager2

    fn list_pam_services(&self) -> ManagerResult<Vec<PamServiceInfo>> {
        self.call_v2::<Vec<PamServiceInfo>, _>("ListPamServices", &())
    }

    fn set_pam_service_enabled(&self, service_id: &str, enabled: bool) -> ManagerResult<bool> {
        self.call_v2::<bool, _>("SetPamServiceEnabled", &(service_id.to_string(), enabled))
    }

    fn get_protection_status(&self) -> ManagerResult<ProtectionStatus> {
        self.call_v2::<ProtectionStatus, _>("GetProtectionStatus", &())
    }
}
