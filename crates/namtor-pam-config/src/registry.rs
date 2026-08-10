// SPDX-License-Identifier: GPL-3.0-only
//! Root-owned enabled-service registry.
//!
//! Tracks which PAM services have `pam_namtor.so` enabled. Stored as
//! JSON at `{registry_root}/pam-registry.json`, written atomically with
//! `flock` + `fsync`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::flock;

const REGISTRY_FILE: &str = "pam-registry.json";

/// Registry of PAM services with `pam_namtor.so` enabled.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(rename = "version")]
    version: u32,
    services: Vec<String>,
}

impl Registry {
    /// Load the registry from `{registry_root}/pam-registry.json`.
    /// Returns an empty registry if the file does not exist.
    pub fn load(registry_root: &Path) -> Result<Self, String> {
        let path = registry_root.join(REGISTRY_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path).map_err(|e| format!("read: {e}"))?;
        serde_json::from_str(&content).map_err(|e| format!("parse: {e}"))
    }

    /// Save the registry atomically.
    pub fn save(&self, registry_root: &Path) -> Result<(), String> {
        fs::create_dir_all(registry_root).map_err(|e| format!("mkdir: {e}"))?;
        let path = registry_root.join(REGISTRY_FILE);
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("serialize: {e}"))?;
        flock::atomic_write(&path, json.as_bytes()).map_err(|e| format!("{e}"))
    }

    /// True if `service_id` is in the registry.
    pub fn is_enabled(&self, service_id: &str) -> bool {
        self.services.iter().any(|s| s == service_id)
    }

    /// Add `service_id` to the registry (idempotent).
    pub fn enable(&mut self, service_id: &str) {
        if !self.is_enabled(service_id) {
            self.services.push(service_id.to_string());
            self.services.sort();
        }
    }

    /// Remove `service_id` from the registry (idempotent).
    pub fn disable(&mut self, service_id: &str) {
        self.services.retain(|s| s != service_id);
    }

    /// List all enabled service IDs.
    pub fn services(&self) -> &[String] {
        &self.services
    }

    /// Remove all tracked services (idempotent).
    pub fn clear(&mut self) {
        self.services.clear();
    }

    /// Serialise this registry to a snapshot directory as `registry.json`.
    /// The caller is responsible for saving PAM files alongside it.
    pub fn snapshot_to(&self, dir: &Path) -> Result<(), String> {
        fs::create_dir_all(dir).map_err(|e| format!("mkdir: {e}"))?;
        let path = dir.join("registry.json");
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("serialize: {e}"))?;
        flock::atomic_write(&path, json.as_bytes()).map_err(|e| format!("{e}"))
    }

    /// Load a registry previously saved by [`Self::snapshot_to`].
    pub fn restore_from(dir: &Path) -> Result<Self, String> {
        let path = dir.join("registry.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path).map_err(|e| format!("read: {e}"))?;
        serde_json::from_str(&content).map_err(|e| format!("parse: {e}"))
    }
}

/// Path to the registry file under `registry_root`.
pub fn registry_path(registry_root: &Path) -> PathBuf {
    registry_root.join(REGISTRY_FILE)
}

/// Path to the lifecycle snapshot directory under `registry_root`.
pub fn snapshot_dir(registry_root: &Path) -> PathBuf {
    registry_root.join("pam-snapshot")
}

/// Delete the registry file (idempotent — succeeds if absent).
pub fn purge_file(registry_root: &Path) -> Result<(), String> {
    let path = registry_path(registry_root);
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("remove: {e}"))
    } else {
        Ok(())
    }
}

/// Recursively remove the snapshot directory (idempotent).
pub fn purge_snapshot(registry_root: &Path) -> Result<(), String> {
    let dir = snapshot_dir(registry_root);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("remove snapshot: {e}"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn empty_registry_when_no_file() {
        let tmp = TempDir::new().unwrap();
        let reg = Registry::load(tmp.path()).unwrap();
        assert!(reg.services().is_empty());
        assert!(!reg.is_enabled("gdm-password"));
    }

    #[test]
    fn enable_and_disable_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut reg = Registry::default();
        reg.enable("gdm-password");
        reg.enable("sudo");
        reg.save(tmp.path()).unwrap();
        let loaded = Registry::load(tmp.path()).unwrap();
        assert!(loaded.is_enabled("gdm-password"));
        assert!(loaded.is_enabled("sudo"));
        assert!(!loaded.is_enabled("su"));
    }

    #[test]
    fn enable_is_idempotent() {
        let mut reg = Registry::default();
        reg.enable("gdm-password");
        reg.enable("gdm-password");
        assert_eq!(reg.services().len(), 1);
    }

    #[test]
    fn disable_removes_entry() {
        let mut reg = Registry::default();
        reg.enable("gdm-password");
        reg.enable("sudo");
        reg.disable("gdm-password");
        assert!(!reg.is_enabled("gdm-password"));
        assert!(reg.is_enabled("sudo"));
    }

    #[test]
    fn disable_nonexistent_is_noop() {
        let mut reg = Registry::default();
        reg.disable("nothing");
        assert!(reg.services().is_empty());
    }

    #[test]
    fn services_are_sorted() {
        let mut reg = Registry::default();
        reg.enable("zebra");
        reg.enable("alpha");
        reg.enable("mid");
        assert_eq!(reg.services(), &["alpha", "mid", "zebra"]);
    }

    #[test]
    fn clear_empties_services() {
        let mut reg = Registry::default();
        reg.enable("gdm-password");
        reg.enable("sudo");
        reg.clear();
        assert!(reg.services().is_empty());
    }

    #[test]
    fn snapshot_and_restore_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let snap = tmp.path().join("snap");
        let mut reg = Registry::default();
        reg.enable("gdm-password");
        reg.enable("sudo");
        reg.snapshot_to(&snap).unwrap();
        let loaded = Registry::restore_from(&snap).unwrap();
        assert!(loaded.is_enabled("gdm-password"));
        assert!(loaded.is_enabled("sudo"));
    }

    #[test]
    fn restore_from_missing_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nope");
        let loaded = Registry::restore_from(&missing).unwrap();
        assert!(loaded.services().is_empty());
    }

    #[test]
    fn purge_file_removes_registry() {
        let tmp = TempDir::new().unwrap();
        let mut reg = Registry::default();
        reg.enable("gdm-password");
        reg.save(tmp.path()).unwrap();
        assert!(registry_path(tmp.path()).exists());
        purge_file(tmp.path()).unwrap();
        assert!(!registry_path(tmp.path()).exists());
    }

    #[test]
    fn purge_file_idempotent_when_absent() {
        let tmp = TempDir::new().unwrap();
        purge_file(tmp.path()).unwrap();
    }

    #[test]
    fn purge_snapshot_removes_dir() {
        let tmp = TempDir::new().unwrap();
        let reg = Registry::default();
        reg.snapshot_to(&snapshot_dir(tmp.path())).unwrap();
        assert!(snapshot_dir(tmp.path()).exists());
        purge_snapshot(tmp.path()).unwrap();
        assert!(!snapshot_dir(tmp.path()).exists());
    }

    #[test]
    fn purge_snapshot_idempotent_when_absent() {
        let tmp = TempDir::new().unwrap();
        purge_snapshot(tmp.path()).unwrap();
    }
}
