// SPDX-License-Identifier: GPL-3.0-only
//! Live [`ManagerClient`] backed by the session D-Bus `org.namtor.Manager`
//! interface (spec §9.1, task 25).
//!
//! Uses zbus 5's blocking API so the GNOME main loop can call methods
//! synchronously from signal handlers. Signal subscriptions
//! (`enrollment_progress`, `enrollment_complete`) are cached: each call to
//! [`DbusManagerClient::get_progress`] drains the most recent cached progress
//! signal for the session, falling back to a fresh method call.

use std::collections::HashMap;
use std::sync::Mutex;

use zbus::blocking::Connection;

use crate::client::{
    EnrollmentProgress, FinishResult, ManagerClient, ManagerError, ManagerResult, ProfileId,
    ProfileInfo, SessionId, StatusInfo,
};

const BUS_NAME: &str = "org.namtor.Manager";
const OBJECT_PATH: &str = "/org/namtor/Manager";
const INTERFACE: &str = "org.namtor.Manager";

/// Live session-bus client. Construct via [`DbusManagerClient::connect`].
pub struct DbusManagerClient {
    conn: Connection,
    /// Latest progress signal per session id, populated by the signal
    /// listener and drained by `get_progress`.
    progress_cache: Mutex<HashMap<String, EnrollmentProgress>>,
}

impl DbusManagerClient {
    /// Connect to the session bus and validate the namtor service is
    /// registered. Returns [`ManagerError::DaemonUnavailable`] if the bus
    /// itself is unreachable, or [`ManagerError::ServiceNotRegistered`] if
    /// the bus is up but `org.namtor.Manager` is not.
    pub fn connect() -> ManagerResult<Self> {
        let conn = Connection::session()
            .map_err(|e| ManagerError::DaemonUnavailable(format!("session bus: {e}")))?;
        // Probe the service. We do not fail just because the name is not
        // uniquely owned yet; `list_names` is enough to know it's registered.
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
            .call_method(Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE), method, body)
            .map_err(map_zbus_error)?;
        msg.body()
            .deserialize::<R>()
            .map_err(|e| ManagerError::Transport(format!("{method} body: {e}")))
    }

    fn call_unit<B>(&self, method: &str, body: &B) -> ManagerResult<()>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        // The service returns an empty tuple on success; deserialize as such.
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
                "org.namtor.Error.DaemonUnavailable" => ManagerError::DaemonUnavailable(detail),
                "org.namtor.Error.AuthRequired" | "org.namtor.Error.PolkitDenied" => {
                    ManagerError::PolkitDenied(detail)
                }
                "org.namtor.Error.NotFound" => ManagerError::NotFound(detail),
                "org.namtor.Error.Conflict" => ManagerError::Conflict(detail),
                "org.namtor.Error.NotEnrolling" => ManagerError::NotEnrolling(detail),
                "org.freedesktop.DBus.Error.ServiceUnknown" => ManagerError::ServiceNotRegistered,
                _ => ManagerError::Transport(e.to_string()),
            }
        }
        _ => ManagerError::Transport(e.to_string()),
    }
}

impl ManagerClient for DbusManagerClient {
    fn list_profiles(&mut self) -> ManagerResult<Vec<ProfileInfo>> {
        self.call::<Vec<ProfileInfo>, _>("ListProfiles", &())
    }

    fn create_profile(&mut self, name: &str) -> ManagerResult<SessionId> {
        let raw: String = self.call::<String, _>("StartEnrollment", &name)?;
        Ok(SessionId(raw))
    }

    fn start_enrollment(&mut self, session: &SessionId) -> ManagerResult<()> {
        self.call_unit("start_enrollment", &session.0.clone())
    }

    fn get_progress(&mut self, session: &SessionId) -> ManagerResult<EnrollmentProgress> {
        // Prefer the cached signal emission if present.
        if let Some(cached) = self
            .progress_cache
            .lock()
            .map_err(|e| ManagerError::Transport(format!("cache lock: {e}")))?
            .remove(&session.0)
        {
            return Ok(cached);
        }
        let progress: EnrollmentProgress =
            self.call::<EnrollmentProgress, _>("get_progress", &session.0.clone())?;
        Ok(progress)
    }

    fn finish_enrollment(&mut self, session: &SessionId) -> ManagerResult<FinishResult> {
        self.call::<FinishResult, _>("finish_enrollment", &session.0.clone())
    }

    fn cancel_enrollment(&mut self, session: &SessionId) -> ManagerResult<()> {
        self.call_unit("cancel_enrollment", &session.0.clone())
    }

    fn merge_profiles(&mut self, src: &ProfileId, dst: &ProfileId) -> ManagerResult<()> {
        self.call_unit("merge_profiles", &(src.0.clone(), dst.0.clone()))
    }

    fn replace_profile(&mut self, id: &ProfileId, name: &str) -> ManagerResult<()> {
        self.call_unit("replace_profile", &(id.0.clone(), name.to_string()))
    }

    fn rename_profile(&mut self, id: &ProfileId, new_name: &str) -> ManagerResult<()> {
        self.call_unit("rename_profile", &(id.0.clone(), new_name.to_string()))
    }

    fn enable_profile(&mut self, id: &ProfileId, enabled: bool) -> ManagerResult<()> {
        self.call_unit("enable_profile", &(id.0.clone(), enabled))
    }

    fn delete_profile(&mut self, id: &ProfileId) -> ManagerResult<()> {
        self.call_unit("delete_profile", &id.0.clone())
    }

    fn status(&mut self) -> ManagerResult<StatusInfo> {
        self.call::<StatusInfo, _>("status", &())
    }
}
