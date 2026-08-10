// SPDX-License-Identifier: GPL-3.0-only
//! Filesystem primitives for the PAM editor: `flock`, atomic write,
//! backup with checksum, and stale-temp cleanup.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::editor::EditError;

/// RAII exclusive `flock` guard. Dropping the file releases the lock.
pub(crate) struct FileLock {
    #[cfg(unix)]
    _file: fs::File,
}

impl FileLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self, EditError> {
        #[cfg(unix)]
        {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let file = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)?;
            let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
            // SAFETY: `fd` is a valid open file descriptor owned by `file`.
            let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
            if rc != 0 {
                return Err(EditError::Io(format!(
                    "flock({}): {}",
                    path.display(),
                    std::io::Error::last_os_error()
                )));
            }
            Ok(Self { _file: file })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(Self {})
        }
    }
}

/// Atomic write: temp → fsync → rename → fsync dir.
/// The temp file is in the same directory so `rename(2)` is atomic.
pub(crate) fn atomic_write(target: &Path, data: &[u8]) -> Result<(), EditError> {
    let parent = target
        .parent()
        .ok_or_else(|| EditError::Io("target path has no parent".into()))?;
    let mut tmp_name = target
        .file_name()
        .ok_or_else(|| EditError::Io("target has no filename".into()))?
        .to_os_string();
    tmp_name.push(".namtor.tmp");
    let tmp_path = parent.join(&tmp_name);
    let _ = fs::remove_file(&tmp_path);

    let result = (|| -> Result<(), EditError> {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp_path, target)?;
        let dir = fs::OpenOptions::new().read(true).open(parent)?;
        dir.sync_all()?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

/// SHA-256 hex digest of `data`.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut s = String::with_capacity(64);
    for byte in digest {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Write a backup file and its SHA-256 checksum sidecar, both fsynced.
pub(crate) fn write_backup(backup: &Path, checksum: &Path, data: &[u8]) -> Result<(), EditError> {
    let mut file = fs::File::create(backup)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    let hash = sha256_hex(data);
    let mut sum_file = fs::File::create(checksum)?;
    sum_file.write_all(hash.as_bytes())?;
    sum_file.sync_all()?;
    Ok(())
}

/// Verify the backup's checksum matches the stored sidecar.
pub(crate) fn verify_backup(backup: &Path, checksum: &Path) -> Result<(), EditError> {
    if !backup.exists() {
        return Ok(());
    }
    let data = fs::read(backup)?;
    let actual = sha256_hex(&data);
    if checksum.exists() {
        let stored = fs::read_to_string(checksum)?;
        if actual.trim() != stored.trim() {
            return Err(EditError::BackupChecksumMismatch(
                backup.display().to_string(),
            ));
        }
    }
    Ok(())
}

/// Remove leftover `.namtor.tmp` and `.namtor.verify.*` files from a
/// previous crashed run.
pub(crate) fn cleanup_stale_temps(pamd_root: &Path) {
    if let Ok(entries) = fs::read_dir(pamd_root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".namtor.tmp") || name.starts_with(".namtor.verify.") {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Backup path: `<target>.namtor.bak`.
pub(crate) fn backup_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".namtor.bak");
    target.parent().unwrap_or(Path::new(".")).join(name)
}

/// Checksum path: `<target>.namtor.bak.sha256`.
pub(crate) fn checksum_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".namtor.bak.sha256");
    target.parent().unwrap_or(Path::new(".")).join(name)
}
