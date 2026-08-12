// SPDX-License-Identifier: GPL-3.0-only
//! List and remove enrolled profiles.
//!
//! Two modes (USA-13):
//!
//! - **`--daemon` (default)**: calls the daemon's `ListProfiles` / `DeleteProfile`
//!   D-Bus methods on `org.namtor.Manager`. The daemon enforces polkit
//!   authorization and returns the caller's profiles (no raw templates).
//! - **`--local`**: opens the encrypted template store directly. This is the
//!   recovery path for when the daemon is unavailable; it requires filesystem
//!   access to the store and does not go through polkit.

use std::path::Path;
use std::process::ExitCode;

use namtor_store::TemplateStore;

use crate::status::ProfileInfo;

/// D-Bus bus name / object path / interface for `org.namtor.Manager` (v1).
const MANAGER_BUS_NAME: &str = "org.namtor.Manager";
const MANAGER_OBJECT_PATH: &str = "/org/namtor/Manager";
const MANAGER_INTERFACE: &str = "org.namtor.Manager";

/// Errors surfaced by a [`ProfileSource`]. The CLI maps each variant to a
/// distinct user-facing message so a transport failure is distinguishable from
/// a daemon-side rejection.
#[derive(Debug, Clone)]
pub enum ProfileSourceError {
    Transport(String),
    Daemon(String),
}

impl std::fmt::Display for ProfileSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileSourceError::Transport(m) => write!(f, "daemon unreachable: {m}"),
            ProfileSourceError::Daemon(m) => write!(f, "daemon error: {m}"),
        }
    }
}

impl std::error::Error for ProfileSourceError {}

/// Source of profile listings and deletions. Production uses
/// [`DbusProfileSource`]; tests inject a fake implementation.
pub trait ProfileSource {
    fn list_profiles(&self) -> Result<Vec<ProfileInfo>, ProfileSourceError>;
    fn delete_profile(&self, uid: u32, profile_id: &str) -> Result<(), ProfileSourceError>;
}

fn classify_dbus_error(e: zbus::Error) -> ProfileSourceError {
    let msg = e.to_string();
    match e {
        zbus::Error::FDO(boxed) => match *boxed {
            zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_) => {
                ProfileSourceError::Transport(msg)
            }
            _ => ProfileSourceError::Daemon(msg),
        },
        zbus::Error::MethodError(name, _, _)
            if name
                .as_str()
                .starts_with("org.freedesktop.DBus.Error.Service")
                || name.as_str() == "org.freedesktop.DBus.Error.NoReply" =>
        {
            ProfileSourceError::Transport(msg)
        }
        _ => ProfileSourceError::Daemon(msg),
    }
}

/// Production [`ProfileSource`] backed by the system D-Bus
/// `org.namtor.Manager` `ListProfiles` / `DeleteProfile` methods.
pub struct DbusProfileSource {
    proxy: zbus::blocking::Proxy<'static>,
}

impl DbusProfileSource {
    /// Connect to the system bus and bind a proxy to `org.namtor.Manager`
    /// at the canonical object path. Fails if the bus is unreachable.
    pub fn connect() -> Result<Self, ProfileSourceError> {
        let conn = zbus::blocking::Connection::system()
            .map_err(|e| ProfileSourceError::Transport(e.to_string()))?;
        let proxy = zbus::blocking::Proxy::new(
            &conn,
            MANAGER_BUS_NAME,
            MANAGER_OBJECT_PATH,
            MANAGER_INTERFACE,
        )
        .map_err(|e| ProfileSourceError::Transport(e.to_string()))?;
        Ok(Self { proxy })
    }
}

impl ProfileSource for DbusProfileSource {
    fn list_profiles(&self) -> Result<Vec<ProfileInfo>, ProfileSourceError> {
        self.proxy
            .call("ListProfiles", &())
            .map_err(classify_dbus_error)
    }

    fn delete_profile(&self, uid: u32, profile_id: &str) -> Result<(), ProfileSourceError> {
        self.proxy
            .call("DeleteProfile", &(uid, profile_id))
            .map_err(classify_dbus_error)
    }
}

/// Default profile id when the operator does not pass one explicitly.
const DEFAULT_PROFILE_ID: &str = "default";

/// Render a list of profiles for the operator. Carries no biometric payload:
/// only profile id, name, enabled flag, and timestamps.
pub fn render_profile_list(profiles: &[ProfileInfo]) -> String {
    let mut out = String::new();
    if profiles.is_empty() {
        out.push_str("no profiles\n");
        return out;
    }
    out.push_str("profiles:\n");
    for p in profiles {
        let state = if p.enabled { "enabled" } else { "disabled" };
        out.push_str(&format!("  {} — {} ({})\n", p.name, p.id, state));
    }
    out
}

/// Run `list --daemon`: fetch profiles from the D-Bus source and print them.
pub fn run_list_daemon(source: &dyn ProfileSource) -> ExitCode {
    match source.list_profiles() {
        Ok(profiles) => {
            print!("{}", render_profile_list(&profiles));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("list: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Run `list --local`: open the store directly and list enrolled uids. This is
/// the recovery path (no daemon required).
pub fn run_list_local(templates_dir: &Path, key_path: &Path) -> ExitCode {
    let store = match TemplateStore::open(templates_dir, key_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("store open failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    match store.list_uids() {
        Ok(uids) if uids.is_empty() => {
            println!("no enrollments");
            ExitCode::SUCCESS
        }
        Ok(uids) => {
            println!("enrolled users:");
            for uid in uids {
                match store.load(uid) {
                    Ok(record) => {
                        println!(
                            "  uid={} ({}) — {} templates, model={}",
                            record.meta.uid,
                            record.meta.username,
                            record.meta.num_templates,
                            record.meta.model_id,
                        );
                    }
                    Err(e) => {
                        println!("  uid={} — error: {e}", uid);
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("list failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Run `remove --daemon`: call `DeleteProfile` on the D-Bus source.
pub fn run_remove_daemon(source: &dyn ProfileSource, uid: u32, profile_id: &str) -> ExitCode {
    match source.delete_profile(uid, profile_id) {
        Ok(()) => {
            println!("removed profile '{}' for uid={uid}", profile_id);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("remove: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Run `remove --local`: open the store directly and delete all profiles for
/// the uid. This is the recovery path (no daemon required).
pub fn run_remove_local(uid: u32, templates_dir: &Path, key_path: &Path) -> ExitCode {
    let store = match TemplateStore::open(templates_dir, key_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("store open failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    match store.delete(uid) {
        Ok(()) => {
            println!("removed enrollment for uid={uid}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("remove failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Resolve the profile id for the `remove` command. Returns the explicit id
/// when given, or [`DEFAULT_PROFILE_ID`] when omitted.
pub fn resolve_profile_id(id: Option<&str>) -> &str {
    id.unwrap_or(DEFAULT_PROFILE_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// In-memory fake [`ProfileSource`] for hermetic list/remove tests.
    /// Records every call so tests can assert the D-Bus method was invoked.
    struct FakeProfileSource {
        calls: Arc<Mutex<Vec<String>>>,
        profiles: Vec<ProfileInfo>,
        fail_with_transport: bool,
    }

    impl FakeProfileSource {
        fn new(profiles: Vec<ProfileInfo>) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                profiles,
                fail_with_transport: false,
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ProfileSource for FakeProfileSource {
        fn list_profiles(&self) -> Result<Vec<ProfileInfo>, ProfileSourceError> {
            self.calls.lock().unwrap().push("list_profiles".into());
            if self.fail_with_transport {
                Err(ProfileSourceError::Transport("bus unreachable".into()))
            } else {
                Ok(self.profiles.clone())
            }
        }

        fn delete_profile(&self, uid: u32, profile_id: &str) -> Result<(), ProfileSourceError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("delete_profile:{uid}:{profile_id}"));
            if self.fail_with_transport {
                Err(ProfileSourceError::Transport("bus unreachable".into()))
            } else {
                Ok(())
            }
        }
    }

    fn profile(id: &str, name: &str, enabled: bool) -> ProfileInfo {
        ProfileInfo {
            id: id.into(),
            name: name.into(),
            enabled,
            created: 1000,
            last_used: 2000,
        }
    }

    #[test]
    fn list_daemon_calls_dbus_method_and_renders_profiles() {
        let svc = FakeProfileSource::new(vec![
            profile("default", "Alice", true),
            profile("work", "Alice-Work", false),
        ]);
        let exit = run_list_daemon(&svc);
        assert_eq!(exit, ExitCode::SUCCESS);
        let calls = svc.calls();
        assert!(calls.contains(&"list_profiles".to_string()));
    }

    #[test]
    fn list_daemon_renders_empty_profile_list() {
        let svc = FakeProfileSource::new(vec![]);
        let exit = run_list_daemon(&svc);
        assert_eq!(exit, ExitCode::SUCCESS);
    }

    #[test]
    fn list_daemon_transport_error_returns_failure() {
        let svc = FakeProfileSource {
            fail_with_transport: true,
            ..FakeProfileSource::new(vec![])
        };
        let exit = run_list_daemon(&svc);
        assert_eq!(exit, ExitCode::FAILURE);
    }

    #[test]
    fn list_local_returns_failure_when_store_unopenable() {
        // Pointing at a path that cannot be opened (under a read-only dir)
        // exercises the error path. The happy path (open a real store and
        // list uids) is tested in namtor-store itself; SEC-3 argon2id root
        // secret changes currently block TemplateStore::open in CLI tests
        // until the store test infrastructure is updated.
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        // Opening with a nonexistent path that can't be auto-created (under
        // a file, not a dir) → store open fails → FAILURE.
        let bogus = dir.path().join("not_a_dir");
        std::fs::write(&bogus, b"x").unwrap();
        let exit = run_list_local(&bogus, &key_path);
        assert_eq!(exit, ExitCode::FAILURE);
    }

    #[test]
    fn remove_daemon_calls_delete_profile_with_uid_and_id() {
        let svc = FakeProfileSource::new(vec![]);
        let exit = run_remove_daemon(&svc, 1000, "default");
        assert_eq!(exit, ExitCode::SUCCESS);
        let calls = svc.calls();
        assert!(
            calls.contains(&"delete_profile:1000:default".to_string()),
            "remove --daemon must call delete_profile with uid+id, got: {calls:?}"
        );
    }

    #[test]
    fn remove_daemon_transport_error_returns_failure() {
        let svc = FakeProfileSource {
            fail_with_transport: true,
            ..FakeProfileSource::new(vec![])
        };
        let exit = run_remove_daemon(&svc, 1000, "default");
        assert_eq!(exit, ExitCode::FAILURE);
    }

    #[test]
    fn remove_local_returns_failure_when_store_unopenable() {
        // Same SEC-3 caveat as list_local_returns_failure_when_store_unopenable:
        // the happy path is tested in namtor-store; here we exercise the error
        // path (unopenable store → FAILURE).
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let bogus = dir.path().join("not_a_dir");
        std::fs::write(&bogus, b"x").unwrap();
        let exit = run_remove_local(1000, &bogus, &key_path);
        assert_eq!(exit, ExitCode::FAILURE);
    }

    #[test]
    fn resolve_profile_id_defaults_to_default() {
        assert_eq!(resolve_profile_id(None), "default");
        assert_eq!(resolve_profile_id(Some("work")), "work");
    }

    #[test]
    fn render_profile_list_no_biometric_tokens() {
        let profiles = vec![profile("default", "Alice", true)];
        let rendered = render_profile_list(&profiles);
        for forbidden in ["embedding", "vector", "threshold", "template", "biometric"] {
            assert!(
                !rendered.to_lowercase().contains(forbidden),
                "rendered list leaked `{forbidden}`: {rendered}"
            );
        }
    }
}
