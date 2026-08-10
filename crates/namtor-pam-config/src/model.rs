// SPDX-License-Identifier: GPL-3.0-only
//! Core types for PAM service inventory and classification.

use serde::{Deserialize, Serialize};

/// Compatibility of a PAM service with `pam_namtor.so` insertion.
///
/// Matches the v2 D-Bus contract `PamServiceCompatibility` enum
/// (`safe` / `unsupported` / `risky`). See `docs/evidence/dbus-contract-v2.xml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PamServiceCompatibility {
    /// Can safely have `pam_namtor.so` added — password fallback exists.
    Safe,
    /// Stack structure is not compatible — no auth lines, no fallback,
    /// or complex substacks prevent safe insertion.
    Unsupported,
    /// Could be added but with warnings — e.g. `pam_deny.so` in a
    /// position that could interfere with fall-through.
    Risky,
}

/// Combined 4-state classification for a PAM service.
///
/// Derives from the `enabled` boolean and [`PamServiceCompatibility`] of
/// [`PamServiceInfo`]. The v2 contract carries `enabled` as a separate
/// boolean field and `compatibility` as the enum; this type is the
/// convenience view that collapses them into one status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    /// `pam_namtor.so` already present in the auth stack.
    Enabled,
    /// Not present, safe to add.
    Disabled,
    /// Stack structure not compatible.
    Unsupported,
    /// Could be added but with warnings.
    Risky,
}

/// Wire type matching the v2 D-Bus contract `PamServiceInfo` `(ssbss)`.
///
/// Fields: `id`, `label`, `enabled`, `compatibility`, `reason`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PamServiceInfo {
    /// Stable service ID — the filename in `/etc/pam.d/`.
    pub id: String,
    /// Human-readable label (e.g. "GDM Password Login").
    pub label: String,
    /// True if `pam_namtor.so` is already present in the auth stack.
    pub enabled: bool,
    /// Compatibility classification for insertion.
    pub compatibility: PamServiceCompatibility,
    /// Typed, human-readable reason for the classification.
    pub reason: String,
}

impl PamServiceInfo {
    /// Collapse `enabled` + `compatibility` into the 4-state [`ServiceStatus`].
    pub fn status(&self) -> ServiceStatus {
        match (self.enabled, self.compatibility) {
            (true, _) => ServiceStatus::Enabled,
            (false, PamServiceCompatibility::Safe) => ServiceStatus::Disabled,
            (false, PamServiceCompatibility::Unsupported) => ServiceStatus::Unsupported,
            (false, PamServiceCompatibility::Risky) => ServiceStatus::Risky,
        }
    }
}

/// PAM management type (first column of a PAM line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PamType {
    Auth,
    Account,
    Password,
    Session,
}

/// PAM control field (second column).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlField {
    Required,
    Requisite,
    Sufficient,
    Optional,
    /// Bracket form, e.g. `[default=ignore]`, `[success=1 default=bad]`.
    Bracket(String),
}

/// A parsed PAM configuration line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PamLine {
    /// Empty line or comment.
    Comment,
    /// A typed PAM entry: `type control module [args...]`.
    Entry {
        pam_type: PamType,
        control: ControlField,
        module: String,
        args: Vec<String>,
    },
    /// Debian-style include: `@include <file>`.
    AtInclude(String),
    /// Arch/RHEL-style include: `include <file>` (as a management-type line).
    Include { pam_type: PamType, file: String },
    /// `substack <file>` directive.
    Substack {
        pam_type: PamType,
        control: ControlField,
        file: String,
    },
    /// Unrecognised line (not a comment, not a valid PAM entry).
    Other(String),
}

/// Summary of the auth-relevant lines in a PAM stack.
#[derive(Debug, Clone, Default)]
pub struct StackSummary {
    /// True if `pam_namtor.so` appears in any auth entry.
    pub has_namtor: bool,
    /// True if a password-fallback module appears in an auth entry
    /// (`pam_unix`, `pam_systemd_home`, `pam_env`, or an include of
    /// `common-auth` / `system-auth` / `password-auth`).
    pub has_password_fallback: bool,
    /// True if `pam_deny.so` appears in an auth entry.
    pub has_deny: bool,
    /// True if any `substack` directive appears.
    pub has_substack: bool,
    /// Count of auth-type entries (direct entries, not includes).
    pub auth_entry_count: usize,
}

/// Errors encountered during scanning or parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanError {
    /// Directory does not exist or is not a directory.
    NotADirectory(String),
    /// File is a symlink — refused for security.
    SymlinkRejected(String),
    /// File is a backup (`~` / `.bak` suffix) or dotfile — skipped.
    BackupOrDotfile(String),
    /// Include cycle detected.
    IncludeCycle(String),
    /// Include path escapes the descriptor root (directory traversal).
    TraversalRejected(String),
    /// File could not be read.
    ReadFailed(String),
    /// PAM file is malformed (no valid lines, or parse errors).
    Malformed(String),
}
