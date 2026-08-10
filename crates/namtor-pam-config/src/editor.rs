// SPDX-License-Identifier: GPL-3.0-only
//! Transactional PAM service editor.
//!
//! Inserts or removes a marked `# namtor-begin` ... `# namtor-end` block
//! containing `auth sufficient pam_namtor.so` into a PAM service file
//! atomically, with backup, checksum, and rollback.
//!
//! ## Safety properties
//!
//! - **Idempotent**: enabling an already-enabled service replaces the
//!   block in place; disabling a service without the block is a no-op.
//! - **Stale-edit conflict**: if the file changed since our backup
//!   (outside the Nam-tor block), the operation is refused.
//! - **Crash-safe**: a crash at any transition leaves the file in its
//!   original state (before the atomic rename) or fully edited (after).
//! - **Fallback preserved**: after insertion, a password-fallback auth
//!   line (direct or through includes) must exist below the block.
//! - **Unsafe ordering refused**: `pam_deny.so` before the block, or
//!   between the block and the first password fallback, is rejected.
//! - **Concurrency**: an exclusive `flock` serialises concurrent editors.

use std::fs;
use std::path::{Path, PathBuf};

use crate::candidate;
use crate::classify::classify_service;
use crate::flock;
use crate::registry::Registry;

/// Errors from the transactional editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    InvalidServiceId(String),
    SymlinkRejected(String),
    TargetMissing(String),
    BackupChecksumMismatch(String),
    StaleEdit(String),
    UnsafeControlField(String),
    NoAuthLines(String),
    NoPasswordFallback(String),
    DenyBeforeBlock(String),
    DenyBeforePassword(String),
    ModuleNotFound,
    Io(String),
    Registry(String),
    CrashInjected(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidServiceId(s) => write!(f, "invalid service id: {s}"),
            Self::SymlinkRejected(s) => write!(f, "symlink rejected: {s}"),
            Self::TargetMissing(s) => write!(f, "target PAM file missing: {s}"),
            Self::BackupChecksumMismatch(s) => write!(f, "backup checksum mismatch: {s}"),
            Self::StaleEdit(s) => write!(f, "stale external edit: {s}"),
            Self::UnsafeControlField(s) => write!(f, "unsafe control field: {s}"),
            Self::NoAuthLines(s) => write!(f, "no auth lines in {s}"),
            Self::NoPasswordFallback(s) => write!(f, "no password fallback: {s}"),
            Self::DenyBeforeBlock(s) => write!(f, "pam_deny before block: {s}"),
            Self::DenyBeforePassword(s) => write!(f, "pam_deny before fallback: {s}"),
            Self::ModuleNotFound => write!(f, "pam_namtor.so not found"),
            Self::Io(s) => write!(f, "io: {s}"),
            Self::Registry(s) => write!(f, "registry: {s}"),
            Self::CrashInjected(s) => write!(f, "crash at: {s}"),
        }
    }
}

impl std::error::Error for EditError {}

impl From<std::io::Error> for EditError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// PAM control field for the Nam-tor block. Only `sufficient` or
/// `[default=ignore]` are accepted — `required` would lock users out
/// when the daemon is unreachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamtorControl {
    Sufficient,
    DefaultIgnore,
}

impl NamtorControl {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sufficient => "sufficient",
            Self::DefaultIgnore => "[default=ignore]",
        }
    }
}

/// Where to inject a crash for testing the rollback guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    BeforeBackup,
    AfterBackup,
    BeforeWrite,
    AfterWrite,
    BeforeVerify,
    AfterVerify,
}

/// Options for the transactional editor.
#[derive(Debug, Clone)]
pub struct EditOptions {
    pub pamd_root: PathBuf,
    pub registry_root: PathBuf,
    pub control: NamtorControl,
    pub crash: Option<CrashPoint>,
}

impl EditOptions {
    pub fn new(pamd_root: PathBuf, registry_root: PathBuf) -> Self {
        Self {
            pamd_root,
            registry_root,
            control: NamtorControl::Sufficient,
            crash: None,
        }
    }
}

/// Outcome of an enable/disable operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOutcome {
    Changed,
    NoOp,
}

// --- public API -------------------------------------------------------------

/// Enable `pam_namtor.so` for a PAM service. Idempotent.
pub fn enable_service(service_id: &str, opts: &EditOptions) -> Result<EditOutcome, EditError> {
    validate_service_id(service_id)?;
    let target = opts.pamd_root.join(service_id);
    let backup = flock::backup_path(&target);
    let checksum = flock::checksum_path(&target);

    let _lock = flock::FileLock::acquire(&lock_path(&opts.pamd_root))?;
    flock::cleanup_stale_temps(&opts.pamd_root);
    reject_symlink(&target)?;
    let content = fs::read_to_string(&target)
        .map_err(|_| EditError::TargetMissing(target.display().to_string()))?;

    let stock = candidate::remove_block(&content);
    if backup.exists() {
        flock::verify_backup(&backup, &checksum)?;
        let backup_content = fs::read_to_string(&backup)?;
        if stock != backup_content {
            return Err(EditError::StaleEdit(format!(
                "{service_id}: changed since backup"
            )));
        }
    }

    if candidate::has_block(&content) && candidate::block_matches(&content, opts.control) {
        return Ok(EditOutcome::NoOp);
    }

    if !candidate::has_auth_lines(&stock) {
        return Err(EditError::NoAuthLines(service_id.into()));
    }

    if opts.crash == Some(CrashPoint::BeforeBackup) {
        return Err(EditError::CrashInjected("BeforeBackup".into()));
    }

    if !backup.exists() {
        flock::write_backup(&backup, &checksum, content.as_bytes())?;
    }

    if opts.crash == Some(CrashPoint::AfterBackup) {
        return Err(EditError::CrashInjected("AfterBackup".into()));
    }

    let rendered = candidate::render_enable(&stock, opts.control);
    candidate::verify_candidate(&rendered, &opts.pamd_root, service_id)?;

    if opts.crash == Some(CrashPoint::BeforeWrite) {
        return Err(EditError::CrashInjected("BeforeWrite".into()));
    }

    flock::atomic_write(&target, rendered.as_bytes())?;

    if opts.crash == Some(CrashPoint::AfterWrite) {
        return Err(EditError::CrashInjected("AfterWrite".into()));
    }

    if opts.crash == Some(CrashPoint::BeforeVerify) {
        return Err(EditError::CrashInjected("BeforeVerify".into()));
    }
    post_verify(&target, &opts.pamd_root, service_id)?;

    let mut reg = Registry::load(&opts.registry_root).unwrap_or_default();
    reg.enable(service_id);
    reg.save(&opts.registry_root).map_err(EditError::Registry)?;

    if opts.crash == Some(CrashPoint::AfterVerify) {
        return Err(EditError::CrashInjected("AfterVerify".into()));
    }

    Ok(EditOutcome::Changed)
}

/// Disable `pam_namtor.so` for a PAM service. Idempotent.
pub fn disable_service(service_id: &str, opts: &EditOptions) -> Result<EditOutcome, EditError> {
    validate_service_id(service_id)?;
    let target = opts.pamd_root.join(service_id);
    let backup = flock::backup_path(&target);
    let checksum = flock::checksum_path(&target);

    let _lock = flock::FileLock::acquire(&lock_path(&opts.pamd_root))?;
    flock::cleanup_stale_temps(&opts.pamd_root);
    reject_symlink(&target)?;
    let content = fs::read_to_string(&target)
        .map_err(|_| EditError::TargetMissing(target.display().to_string()))?;

    if !candidate::has_block(&content) {
        return Ok(EditOutcome::NoOp);
    }

    let stock = candidate::remove_block(&content);
    if backup.exists() {
        flock::verify_backup(&backup, &checksum)?;
        let backup_content = fs::read_to_string(&backup)?;
        if stock != backup_content {
            return Err(EditError::StaleEdit(format!(
                "{service_id}: changed since backup"
            )));
        }
    }

    candidate::verify_fallback_after_disable(&stock, &opts.pamd_root, service_id)?;
    flock::atomic_write(&target, stock.as_bytes())?;

    let mut reg = Registry::load(&opts.registry_root).unwrap_or_default();
    reg.disable(service_id);
    reg.save(&opts.registry_root).map_err(EditError::Registry)?;

    Ok(EditOutcome::Changed)
}

/// Resolve the PAM module path for the current distro.
pub fn resolve_module_path() -> Option<PathBuf> {
    let dirs = [
        Path::new("/usr/lib/security"),
        Path::new("/usr/lib64/security"),
        Path::new("/usr/lib/x86_64-linux-gnu/security"),
        Path::new("/usr/lib/aarch64-linux-gnu/security"),
    ];
    dirs.iter().find_map(|d| {
        let p = d.join("pam_namtor.so");
        p.is_file().then_some(p)
    })
}

// --- internal helpers -------------------------------------------------------

fn validate_service_id(id: &str) -> Result<(), EditError> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || id.starts_with('.')
    {
        return Err(EditError::InvalidServiceId(id.to_string()));
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<(), EditError> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            Err(EditError::SymlinkRejected(path.display().to_string()))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(EditError::TargetMissing(path.display().to_string()))
        }
        Err(e) => Err(EditError::Io(e.to_string())),
    }
}

pub(crate) fn lock_path(pamd_root: &Path) -> PathBuf {
    pamd_root.join(".namtor.lock")
}

fn post_verify(target: &Path, pamd_root: &Path, service_id: &str) -> Result<(), EditError> {
    let info = classify_service(service_id, target, pamd_root)
        .map_err(|e| EditError::Io(format!("post-verify: {e:?}")))?;
    if !info.enabled {
        return Err(EditError::Io(format!(
            "{service_id}: pam_namtor not found after edit"
        )));
    }
    Ok(())
}
