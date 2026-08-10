// SPDX-License-Identifier: GPL-3.0-only
//! Directory scanner for `/etc/pam.d/`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::model::ScanError;

/// Filter applied to directory entries to reject unsafe files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanFilter;

/// A file that passed the scan filter and is ready for parsing.
#[derive(Debug, Clone)]
pub struct ScannedFile {
    /// Service ID — the filename within the `pam.d` directory.
    pub id: String,
    /// Absolute path to the file (within the descriptor root).
    pub path: PathBuf,
}

/// Scan a `pam.d` directory descriptor-relative. Returns scanned files
/// sorted by ID. Rejects symlinks, backups, and dotfiles.
pub fn scan_pamd(dir: &Path) -> Result<Vec<ScannedFile>, ScanError> {
    scan_pamd_with(dir, &ScanFilter)
}

/// Scan with an explicit filter (for testing).
pub fn scan_pamd_with(dir: &Path, _filter: &ScanFilter) -> Result<Vec<ScannedFile>, ScanError> {
    if !dir.is_dir() {
        return Err(ScanError::NotADirectory(dir.display().to_string()));
    }

    let mut files = Vec::new();

    let entries =
        fs::read_dir(dir).map_err(|_| ScanError::ReadFailed(dir.display().to_string()))?;

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        let fname = entry.file_name().to_string_lossy().to_string();

        if is_rejected_name(&fname) {
            continue;
        }

        // Reject symlinks — security: never follow links in pam.d.
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            return Err(ScanError::SymlinkRejected(fname));
        }
        if !meta.file_type().is_file() {
            continue;
        }

        files.push(ScannedFile { id: fname, path });
    }

    files.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(files)
}

/// True if a filename should be rejected (backup, dotfile, or rpm/tmp save).
fn is_rejected_name(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    if name.ends_with('~') {
        return true;
    }
    if name.ends_with(".bak") {
        return true;
    }
    if name.ends_with(".namtor.bak") {
        return true;
    }
    if name.ends_with(".namtor.bak.sha256") {
        return true;
    }
    if name.contains(".namtor.bak") {
        return true;
    }
    // RPM save files: .rpmsave, .rpmnew, .rpmorig
    if name.ends_with(".rpmsave") || name.ends_with(".rpmnew") || name.ends_with(".rpmorig") {
        return true;
    }
    // dpkg save files
    if name.ends_with(".dpkg-dist") || name.ends_with(".dpkg-old") || name.ends_with(".dpkg-new") {
        return true;
    }
    false
}
