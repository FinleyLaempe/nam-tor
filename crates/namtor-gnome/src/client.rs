// SPDX-License-Identifier: GPL-3.0-only
//! D-Bus client contract for `org.namtor.Manager` (v1) and
//! `org.namtor.Manager2` (v2) on the **system bus** (spec §9.1, task 14).
//!
//! The GNOME UI talks to the daemon exclusively over the system D-Bus
//! `org.namtor.Manager` (v1) and `org.namtor.Manager2` (v2) interfaces. This
//! module defines:
//!
//! - The pure data types crossing the bus ([`ProfileInfo`],
//!   [`EnrollmentProgress`], [`StatusReport`], [`ReviewOutcome`],
//!   [`PamServiceInfo`], [`ProtectionStatus`]).
//! - The [`ManagerClient`] trait every backend implements.
//!
//! Privacy invariant (asserted by `tests/privacy_scan.rs`): none of these
//! types carry embeddings, raw frames, match scores, or thresholds. The UI
//! only ever sees names, booleans, counts, quality *labels*, and timestamps.
//!
//! Wire signatures are frozen in `docs/evidence/dbus-contract-v2.xml` and
//! mirrored here byte-for-byte. The `zvariant::Type` derive ensures the
//! Rust structs serialize to the correct D-Bus signatures.

use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// v1 types — org.namtor.Manager
// ---------------------------------------------------------------------------

/// Opaque identifier for a named face profile (D-Bus: `s`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileId(pub String);

impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque identifier for an in-flight enrollment session (D-Bus: `s`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Public profile record returned by `ListProfiles` (wire `(ssbtt)`).
///
/// Fields are deliberately coarse: a display name, an enabled flag, and two
/// timestamps. `last_used` is `0` when the profile has never authenticated
/// (the daemon uses a bare `u64`, not an `Option`, on the wire). No
/// biometric data, no scores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
pub struct ProfileInfo {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// Unix-epoch seconds (wire `t`).
    pub created: u64,
    /// Unix-epoch seconds; `0` if the profile has never authenticated.
    pub last_used: u64,
}

/// Live enrollment progress emitted by the `EnrollmentProgress` signal
/// (wire `(susu)`). The UI shows *both* the approved metrics (frames
/// captured, coverage %) *and* the human-readable quality label
/// ("low"/"medium"/"high"). The label is a coarse bucket so the user
/// cannot reverse-engineer a threshold from the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
pub struct EnrollmentProgress {
    pub session_id: String,
    pub frames_captured: u32,
    pub quality: QualityLabel,
    pub coverage_percent: u32,
}

/// Three-bucket quality label (wire `s`, lowercase). The UI must render
/// this verbatim — it is the only quality signal a user sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
#[serde(rename_all = "lowercase")]
pub enum QualityLabel {
    Low,
    Medium,
    High,
}

impl QualityLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Coarse outcome of `ReviewProfile` (wire `(sbs)`): the reviewed profile
/// id, whether re-enrollment is recommended, and a quality label. No score,
/// no threshold, no embedding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
pub struct ReviewOutcome {
    pub profile_id: String,
    pub needs_reenroll: bool,
    pub quality: QualityLabel,
}

/// Snapshot returned by `GetStatus` (wire `(ua(ssbtt)b)`). Carries the
/// negotiated API version, the caller's profile list, and whether the IR
/// emitter is currently armed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
pub struct StatusReport {
    pub api_version: u32,
    pub profiles: Vec<ProfileInfo>,
    pub emitter_armed: bool,
}

// ---------------------------------------------------------------------------
// v2 types — org.namtor.Manager2
// ---------------------------------------------------------------------------

/// v2: PAM service compatibility classification (wire `s`, lowercase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
#[cfg_attr(feature = "dbus", zvariant(signature = "s", rename_all = "lowercase"))]
#[serde(rename_all = "lowercase")]
pub enum PamServiceCompatibility {
    /// Safe to enable — has password fallback, not deny-first.
    Safe,
    /// Unsupported — risky stack shape (deny-first, no fallback, malformed).
    Unsupported,
    /// Risky — can be enabled but high-impact (login, sudo, su, polkit).
    Risky,
}

impl PamServiceCompatibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Unsupported => "unsupported",
            Self::Risky => "risky",
        }
    }
}

/// v2: Sealing mechanism backing template storage (wire `s`, lowercase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
#[cfg_attr(feature = "dbus", zvariant(signature = "s", rename_all = "lowercase"))]
#[serde(rename_all = "lowercase")]
pub enum ProtectionKeyKind {
    /// TPM2 2.0 sealed (hardware-bound).
    Tpm2,
    /// Software AES-256-GCM (file fallback).
    Software,
}

impl ProtectionKeyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tpm2 => "tpm2",
            Self::Software => "software",
        }
    }
}

/// v2: Descriptor of a detected PAM auth service (wire `(ssbss)`). Returned
/// by `ListPamServices`. Carries no biometric material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
pub struct PamServiceInfo {
    /// Stable service id (e.g. `"gdm-password"`, `"sddm"`).
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Whether namtor is currently enabled for this service.
    pub enabled: bool,
    /// Compatibility classification.
    pub compatibility: PamServiceCompatibility,
    /// Human-readable description.
    pub description: String,
}

/// v2: Template storage protection status (wire `(sbs)`). Returned by
/// `GetProtectionStatus`. Carries no sealing material — only the sealing
/// *kind*, TPM presence, and a human-readable warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
pub struct ProtectionStatus {
    /// Active sealing mechanism.
    pub sealing: ProtectionKeyKind,
    /// Whether a TPM2 device is present on the system.
    pub has_tpm: bool,
    /// Human-readable warning (empty when protection is hardware-bound).
    pub warning: String,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// All ways a manager call can fail that the UI must distinguish. These map
/// 1:1 to the D-Bus error names the service raises (see
/// `namtor-daemon::manager::contract::error` and `error_v2`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagerError {
    /// `org.namtor.Error.Denied` — polkit refused the action.
    #[error("denied: {0}")]
    Denied(String),
    /// `org.namtor.Error.Challenge` — polkit requires interactive auth.
    #[error("challenge: {0}")]
    Challenge(String),
    /// `org.namtor.Error.NotFound` — profile or session id unknown.
    #[error("not found: {0}")]
    NotFound(String),
    /// `org.namtor.Error.Exists` — duplicate profile name.
    #[error("exists: {0}")]
    Exists(String),
    /// `org.namtor.Error.SessionNotFound` — enrollment session id unknown.
    #[error("session not found: {0}")]
    SessionNotFound(String),
    /// `org.namtor.Error.SessionBusy` — target uid already has a session.
    #[error("session busy: {0}")]
    SessionBusy(String),
    /// `org.namtor.Error.State` — daemon in a state that rejects the call.
    #[error("state: {0}")]
    State(String),
    /// `org.namtor.Error.IncompatibleVersion` — version negotiation failed.
    #[error("incompatible version: {0}")]
    IncompatibleVersion(String),
    /// v2: `org.namtor.Error.Unsupported` — PAM service is unsupported.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// v2: `org.namtor.Error.Rollback` — PAM edit failed and rolled back.
    #[error("rollback: {0}")]
    Rollback(String),
    /// v2: `org.namtor.Error.Conflict` — concurrent PAM edit conflict.
    #[error("conflict: {0}")]
    Conflict(String),
    /// `org.freedesktop.DBus.Error.ServiceUnknown` — bus reachable but
    /// `org.namtor.Manager` is not registered.
    #[error("namtor service not registered on the system bus")]
    ServiceNotRegistered,
    /// Bus itself unreachable (system bus not running, access denied).
    #[error("daemon unavailable: {0}")]
    DaemonUnavailable(String),
    /// Any other transport or protocol failure.
    #[error("dbus transport error: {0}")]
    Transport(String),
}

pub type ManagerResult<T> = Result<T, ManagerError>;

// ---------------------------------------------------------------------------
// ManagerClient trait
// ---------------------------------------------------------------------------

/// Trait every manager backend implements. The GNOME presenter depends only
/// on this trait; the real D-Bus client ([`crate::dbus_client`]) and the
/// test fake ([`crate::fake`]) are interchangeable.
///
/// Method names mirror the frozen `org.namtor.Manager` (v1) and
/// `org.namtor.Manager2` (v2) interfaces 1:1. The `uid` parameter on v1
/// mutation methods is the *target* uid — for self-management the UI passes
/// the current user's uid. `ListProfiles` takes no uid because the daemon
/// resolves the caller from `SO_PEERCRED` and returns only the caller's
/// profiles.
///
/// The `EnrollmentProgress` signal is surfaced via [`Self::poll_progress`]
/// so the trait stays synchronous and trivially testable; the live client
/// caches the latest signal emission per session.
pub trait ManagerClient {
    // v1 methods

    /// `ListProfiles` → `a(ssbtt)`. Returns only profiles owned by the
    /// caller.
    fn list_profiles(&self) -> ManagerResult<Vec<ProfileInfo>>;

    /// `StartEnrollment(uid, name) → session`. Creates and arms a session.
    fn start_enrollment(&self, uid: u32, name: &str) -> ManagerResult<SessionId>;

    /// `CancelEnrollment(session)`. Disarms; no profile is written.
    fn cancel_enrollment(&self, session: &SessionId) -> ManagerResult<()>;

    /// `CommitProfile(session) → profile`. Finalises a session into a
    /// persistent profile. Returns the new profile id.
    fn commit_profile(&self, session: &SessionId) -> ManagerResult<String>;

    /// `ReviewProfile(uid, id) → (sbs)`. Coarse re-review of an existing
    /// profile.
    fn review_profile(&self, uid: u32, id: &ProfileId) -> ManagerResult<ReviewOutcome>;

    /// `RenameProfile(uid, id, name)`. Relabel a profile.
    fn rename_profile(&self, uid: u32, id: &ProfileId, new_name: &str) -> ManagerResult<()>;

    /// `EnableProfile(uid, id, enabled)`. Toggle a profile.
    fn enable_profile(&self, uid: u32, id: &ProfileId, enabled: bool) -> ManagerResult<()>;

    /// `DeleteProfile(uid, id)`. Remove a profile.
    fn delete_profile(&self, uid: u32, id: &ProfileId) -> ManagerResult<()>;

    /// `MergeProfiles(uid, src, dst)`. Fold `src` into `dst`, remove `src`.
    fn merge_profiles(&self, uid: u32, src: &ProfileId, dst: &ProfileId) -> ManagerResult<()>;

    /// `ReplaceProfile(uid, id, name)`. Re-name an existing profile in
    /// place, typically before re-enrolling.
    fn replace_profile(&self, uid: u32, id: &ProfileId, name: &str) -> ManagerResult<()>;

    /// `GetStatus` → `(ua(ssbtt)b)`. Daemon health + profile snapshot.
    fn get_status(&self) -> ManagerResult<StatusReport>;

    /// Poll the latest cached `EnrollmentProgress` signal for the given
    /// session. Returns `Ok(None)` when no signal has arrived since the
    /// last poll.
    fn poll_progress(&self, session: &SessionId) -> ManagerResult<Option<EnrollmentProgress>>;

    // v2 methods — org.namtor.Manager2

    /// `ListPamServices` → `a(ssbss)`. Detected PAM auth services.
    fn list_pam_services(&self) -> ManagerResult<Vec<PamServiceInfo>>;

    /// `SetPamServiceEnabled(service_id, enabled) → success`. Toggle namtor
    /// for a PAM service. Requires polkit `org.namtor.configure-pam`.
    fn set_pam_service_enabled(&self, service_id: &str, enabled: bool) -> ManagerResult<bool>;

    /// `GetProtectionStatus` → `(sbs)`. Template storage protection status.
    fn get_protection_status(&self) -> ManagerResult<ProtectionStatus>;
}
