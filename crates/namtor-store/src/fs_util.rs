// SPDX-License-Identifier: GPL-3.0-only
//! Filesystem primitives for the template store: atomic write, symlink
//! rejection, owner check, and `flock` lock discipline.
//!
//! These are Linux/Unix-specific. On non-Unix targets the lock and chown
//! paths degrade to no-ops so the crate still compiles for `cargo check` on
//! other platforms; production runs only on Linux.

use std::io::Write;
use std::path::{Path, PathBuf};

use rand::RngCore;

use crate::store::StoreError;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// Refuse if `path` is a symlink. A missing path is fine (caller will create).
pub(crate) fn reject_symlink(path: &Path) -> Result<(), StoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(StoreError::Symlink(path.to_path_buf())),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Verify `path` is owned by `expected`. Unix-only; no-op elsewhere.
pub(crate) fn check_owner(path: &Path, expected: u32) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path)?;
        let actual = meta.uid();
        if actual != expected {
            return Err(StoreError::WrongOwner {
                path: path.to_path_buf(),
                expected,
                actual,
            });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, expected);
    }
    Ok(())
}

/// Atomic write: `tmp` -> `fsync` -> `rename` -> `fsync` dir.
///
/// `expected_owner` is the uid files must be owned by (and will be chowned
/// to when running as root). On any error the temp file is removed and the
/// previous record at `path` is left untouched.
pub(crate) fn atomic_write(
    path: &Path,
    data: &[u8],
    expected_owner: u32,
) -> Result<(), StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| StoreError::Crypto("record path has no parent".into()))?;

    let mut tmp_name = path
        .file_name()
        .ok_or_else(|| StoreError::Crypto("record path has no file name".into()))?
        .to_os_string();
    let mut rand_bytes = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut rand_bytes);
    tmp_name.push(format!(".tmp.{}", hex_short(&rand_bytes)));
    let tmp_path = parent.join(tmp_name);

    let _ = std::fs::remove_file(&tmp_path);

    let write_result = (|| -> Result<(), StoreError> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp_path)?;
        file.write_all(data)?;
        file.sync_all()?;
        chown_if_root(&tmp_path, expected_owner)?;
        std::fs::rename(&tmp_path, path)?;
        let dir = std::fs::OpenOptions::new().read(true).open(parent)?;
        dir.sync_all()?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    write_result?;
    Ok(())
}

fn hex_short(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// `chown` `path` to `expected_owner` (and that user's primary gid) only when
/// running as root. As a non-root process we cannot chown and the file is
/// already owned by the current user, so this is a no-op.
///
/// Hard-fails on `chown` error rather than silently leaving a root-owned
/// artefact the daemon (running as the `namtor` system user) could not read.
pub(crate) fn chown_if_root(path: &Path, expected_owner: u32) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        let euid = unsafe { libc::geteuid() };
        if euid != 0 {
            return Ok(());
        }
        let gid = gid_for_uid(expected_owner).unwrap_or(expected_owner);
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|e| StoreError::Crypto(format!("path nul: {e}")))?;
        // SAFETY: `c_path` is a valid NUL-terminated C string owned by this
        // scope; `chown` does not retain the pointer.
        let rc = unsafe { libc::chown(c_path.as_ptr(), expected_owner, gid) };
        if rc != 0 {
            return Err(StoreError::Io(std::io::Error::last_os_error()));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, expected_owner);
    }
    Ok(())
}

/// Ensure `dir` (and the directory file descriptor itself) is owned by
/// `expected_owner` when running as root. No-op for non-root. Used at store
/// open time so the templates directory — which the daemon must read/write —
/// is handed to the `namtor` system user, not left root-owned by an
/// enrollment run that created it.
pub(crate) fn ensure_dir_owner(dir: &Path, expected_owner: u32) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        let euid = unsafe { libc::geteuid() };
        if euid != 0 {
            return Ok(());
        }
        let gid = gid_for_uid(expected_owner).unwrap_or(expected_owner);
        let c_path = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes())
            .map_err(|e| StoreError::Crypto(format!("path nul: {e}")))?;
        // SAFETY: `c_path` is a valid NUL-terminated C string; `chown` does
        // not retain the pointer.
        let rc = unsafe { libc::chown(c_path.as_ptr(), expected_owner, gid) };
        if rc != 0 {
            return Err(StoreError::Io(std::io::Error::last_os_error()));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (dir, expected_owner);
    }
    Ok(())
}

#[cfg(unix)]
fn gid_for_uid(uid: u32) -> Option<u32> {
    // SAFETY: `getpwuid` returns a pointer into static/thread-local storage
    // valid until the next pwent call; we copy the gid out immediately.
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return None;
        }
        Some((*pw).pw_gid)
    }
}

/// RAII `flock(LOCK_EX)` guard. Dropping the `File` releases the lock.
pub(crate) struct FileLock {
    _file: std::fs::File,
}

impl FileLock {
    /// Acquire the lock and, when running as root, chown the lock file to
    /// `expected_owner` so the daemon (running as `namtor`) can truncate and
    /// lock it on a later write. Non-root callers pass their own euid, which
    /// `chown_if_root` turns into a no-op.
    pub(crate) fn acquire_with_owner(path: &Path, expected_owner: u32) -> Result<Self, StoreError> {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)?;
            let fd = file.as_raw_fd();
            // SAFETY: `fd` is a valid open file descriptor owned by `file`.
            let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
            if rc != 0 {
                return Err(StoreError::Lock(format!(
                    "flock({path:?}): {}",
                    std::io::Error::last_os_error()
                )));
            }
            chown_if_root(path, expected_owner)?;
            Ok(Self { _file: file })
        }
        #[cfg(not(unix))]
        {
            let _ = (path, expected_owner);
            Ok(Self {
                _file: std::fs::File::create("/dev/null")?,
            })
        }
    }
}

#[allow(dead_code)]
fn _unused_pathbuf() {
    let _ = PathBuf::new();
}
