// SPDX-License-Identifier: GPL-3.0-only
//! Typed PAM service inventory, classifier, and transactional editor.
//!
//! Scans a `/etc/pam.d/` directory (descriptor-relative) and classifies
//! auth-relevant services (GDM, SDDM, sudo, su, polkit, etc.) into a typed
//! status with stable IDs and human-readable labels. The transactional
//! editor (`editor` module) inserts/removes a marked `pam_namtor.so` block
//! atomically with backup, checksum, and rollback.
//!
//! ## Classification states
//!
//! - [`ServiceStatus::Enabled`] — `pam_namtor.so` already present in the auth
//!   stack.
//! - [`ServiceStatus::Disabled`] — `pam_namtor.so` not present, safe to add
//!   (password fallback exists below the insertion point).
//! - [`ServiceStatus::Unsupported`] — PAM stack structure not compatible
//!   (no auth lines, no password fallback, complex substacks).
//! - [`ServiceStatus::Risky`] — could be added but with warnings (e.g.
//!   `pam_deny.so` in a position that could interfere).
//!
//! ## Safety
//!
//! The scanner rejects symlinks (security), backup files (`~` / `.bak`
//! suffixes), dotfiles, and malformed files. Include resolution detects
//! cycles and directory traversal. No file outside the descriptor root is
//! ever read. The editor uses `flock` for concurrency, atomic `rename(2)`
//! for writes, and fsynced backups with SHA-256 checksums.

mod candidate;
mod classify;
mod editor;
mod flock;
mod lifecycle;
mod model;
mod parser;
mod registry;
mod scanner;

pub use classify::{classify_service, known_service_label, ClassificationContext};
pub use editor::{
    disable_service, enable_service, resolve_module_path, CrashPoint, EditError, EditOptions,
    EditOutcome, NamtorControl,
};
pub use lifecycle::{
    disable_all_services, migrate, purge, snapshot, validate, MigrationReport,
    ValidationInconsistency, ValidationReport, ValidationStatus,
};
pub use model::{
    ControlField, PamLine, PamServiceCompatibility, PamServiceInfo, ScanError, ServiceStatus,
    StackSummary,
};
pub use parser::{parse_service, resolve_includes, IncludeResult, ParsedService};
pub use registry::{purge_file, purge_snapshot, registry_path, snapshot_dir, Registry};
pub use scanner::{scan_pamd, ScanFilter, ScannedFile};
