// SPDX-License-Identifier: GPL-3.0-only
//! Lifecycle commands: snapshot, validate, migrate, disable-all, purge.
//!
//! These coordinate the registry and the transactional editor to support
//! package upgrade (migrate with rollback) and removal (disable-all + purge)
//! workflows. The registry tracks which PAM services have `pam_namtor.so`
//! enabled; the snapshot/restore pair provides the rollback guarantee for
//! failed migrations.

use std::fs;
use std::path::{Path, PathBuf};

use crate::candidate;
use crate::editor::{
    disable_service, enable_service, lock_path, EditError, EditOptions, EditOutcome,
};
use crate::flock;
use crate::registry::{self, Registry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationStatus {
    Consistent,
    Inconsistent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationInconsistency {
    pub service_id: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReport {
    pub status: ValidationStatus,
    pub inconsistencies: Vec<ValidationInconsistency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub migrated_services: Vec<String>,
    pub restored: bool,
}

/// Snapshot the current PAM state and registry for rollback.
///
/// Saves the registry JSON and a copy of every PAM file for services in
/// the registry to `{registry_root}/pam-snapshot/`.
pub fn snapshot(opts: &EditOptions) -> Result<PathBuf, EditError> {
    let snap = registry::snapshot_dir(&opts.registry_root);
    fs::create_dir_all(&snap).map_err(|e| EditError::Io(e.to_string()))?;
    let reg = Registry::load(&opts.registry_root).unwrap_or_default();
    reg.snapshot_to(&snap).map_err(EditError::Registry)?;

    let pam_snap = snap.join("pam.d");
    if !reg.services().is_empty() {
        fs::create_dir_all(&pam_snap).map_err(|e| EditError::Io(e.to_string()))?;
    }
    for svc in reg.services() {
        let src = opts.pamd_root.join(svc);
        if src.exists() {
            let content = fs::read(&src).map_err(|e| EditError::Io(e.to_string()))?;
            flock::atomic_write(&pam_snap.join(svc), &content)?;
        }
    }
    Ok(snap)
}

/// Validate that the PAM state matches the registry.
///
/// For each service in the registry, checks the PAM file has the Nam-tor
/// block. For each PAM file with a Nam-tor block not in the registry,
/// reports an orphaned block.
pub fn validate(opts: &EditOptions) -> Result<ValidationReport, EditError> {
    let reg = Registry::load(&opts.registry_root).unwrap_or_default();
    let mut inconsistencies = Vec::new();

    for svc in reg.services() {
        let target = opts.pamd_root.join(svc);
        match fs::read_to_string(&target) {
            Ok(content) => {
                if !candidate::has_block(&content) {
                    inconsistencies.push(ValidationInconsistency {
                        service_id: svc.clone(),
                        kind: "missing_block".into(),
                    });
                }
            }
            Err(_) => {
                inconsistencies.push(ValidationInconsistency {
                    service_id: svc.clone(),
                    kind: "missing_block".into(),
                });
            }
        }
    }

    if let Ok(entries) = fs::read_dir(&opts.pamd_root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.')
                || name.ends_with(".namtor.bak")
                || name.ends_with(".namtor.bak.sha256")
                || name.ends_with(".namtor.tmp")
                || name.starts_with(".namtor.verify.")
            {
                continue;
            }
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Ok(content) = fs::read_to_string(&path) {
                if candidate::has_block(&content) && !reg.is_enabled(&name) {
                    inconsistencies.push(ValidationInconsistency {
                        service_id: name,
                        kind: "orphaned_block".into(),
                    });
                }
            }
        }
    }

    let status = if inconsistencies.is_empty() {
        ValidationStatus::Consistent
    } else {
        ValidationStatus::Inconsistent
    };
    Ok(ValidationReport {
        status,
        inconsistencies,
    })
}

/// Migrate PAM entries to the current format, preserving enabled services.
///
/// Snapshots the current state, then re-enables every service in the
/// registry (which re-renders the block in the current format). If any
/// service fails, the snapshot is restored.
pub fn migrate(opts: &EditOptions) -> Result<MigrationReport, EditError> {
    let snap = snapshot(opts)?;
    let reg = Registry::load(&opts.registry_root).unwrap_or_default();
    let mut migrated = Vec::new();

    for svc in reg.services() {
        match enable_service(svc, opts) {
            Ok(EditOutcome::Changed) | Ok(EditOutcome::NoOp) => {
                migrated.push(svc.clone());
            }
            Err(e) => {
                restore_snapshot(&snap, opts)?;
                return Err(e);
            }
        }
    }
    Ok(MigrationReport {
        migrated_services: migrated,
        restored: true,
    })
}

/// Disable every Nam-tor PAM service listed in the registry.
///
/// Returns the list of service IDs that were disabled. Idempotent —
/// services whose PAM file is missing or already clean are silently
/// skipped (removed from the registry but not reported as disabled).
pub fn disable_all_services(opts: &EditOptions) -> Result<Vec<String>, EditError> {
    let reg = Registry::load(&opts.registry_root).unwrap_or_default();
    let mut disabled = Vec::new();
    for svc in reg.services() {
        match disable_service(svc, opts) {
            Ok(EditOutcome::Changed) => {
                disabled.push(svc.clone());
            }
            Ok(EditOutcome::NoOp) => {}
            Err(EditError::TargetMissing(_)) => {
                let mut r = Registry::load(&opts.registry_root).unwrap_or_default();
                r.disable(svc);
                r.save(&opts.registry_root).map_err(EditError::Registry)?;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(disabled)
}

/// Purge all Nam-tor PAM state: disable every service, remove backups,
/// remove the registry, and remove the snapshot directory.
pub fn purge(opts: &EditOptions) -> Result<(), EditError> {
    disable_all_services(opts)?;
    remove_all_backups(&opts.pamd_root)?;
    registry::purge_file(&opts.registry_root).map_err(EditError::Registry)?;
    registry::purge_snapshot(&opts.registry_root).map_err(EditError::Registry)?;
    flock::cleanup_stale_temps(&opts.pamd_root);
    Ok(())
}

fn restore_snapshot(snap: &Path, opts: &EditOptions) -> Result<(), EditError> {
    let reg = Registry::restore_from(snap).map_err(EditError::Registry)?;
    let pam_snap = snap.join("pam.d");
    if pam_snap.exists() {
        if let Ok(entries) = fs::read_dir(&pam_snap) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let content = fs::read(entry.path()).map_err(|e| EditError::Io(e.to_string()))?;
                flock::atomic_write(&opts.pamd_root.join(&name), &content)?;
            }
        }
    }
    reg.save(&opts.registry_root).map_err(EditError::Registry)?;
    Ok(())
}

fn remove_all_backups(pamd_root: &Path) -> Result<(), EditError> {
    let lock = lock_path(pamd_root);
    if lock.exists() {
        fs::remove_file(&lock).map_err(|e| EditError::Io(e.to_string()))?;
    }
    if let Ok(entries) = fs::read_dir(pamd_root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".namtor.bak") || name.ends_with(".namtor.bak.sha256") {
                fs::remove_file(entry.path()).map_err(|e| EditError::Io(e.to_string()))?;
            }
        }
    }
    Ok(())
}
