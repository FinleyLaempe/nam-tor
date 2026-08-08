// SPDX-License-Identifier: GPL-3.0-only
//! D-Bus client contract for `org.namtor.Manager` (spec §9.1, task 25/26).
//!
//! The GNOME UI talks to the daemon exclusively over the session D-Bus
//! `org.namtor.Manager` interface. This module defines:
//!
//! - The pure data types crossing the bus ([`ProfileInfo`],
//!   [`EnrollmentProgress`], [`StatusInfo`], [`FinishResult`]).
//! - The [`ManagerClient`] trait every backend implements.
//!
//! Privacy invariant (asserted by `tests/privacy_scan.rs`): none of these
//! types carry embeddings, raw frames, match scores, or thresholds. The UI
//! only ever sees names, booleans, counts, quality *labels*, and timestamps.

use std::fmt;

use serde::{Deserialize, Serialize};

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

/// Public profile record surfaced to the UI. Mirrors the D-Bus struct.
///
/// Fields are deliberately coarse: a display name, an enabled flag, and two
/// timestamps. No biometric data, no template count beyond what the user
/// already controls by enrolling, no scores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
pub struct ProfileInfo {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// Unix-epoch seconds, matching the D-Bus `t` signature.
    pub created: u64,
    /// Unix-epoch seconds; `None` if the profile has never authenticated.
    pub last_used: Option<u64>,
}

/// Live enrollment progress. Spec §6.5: the UI shows *both* the approved
/// metrics (frames captured, coverage %) *and* the human-readable quality
/// label ("low"/"medium"/"high"). The label is a coarse bucket so the user
/// cannot reverse-engineer a threshold from the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
pub struct EnrollmentProgress {
    pub session_id: String,
    pub frames_captured: u32,
    pub quality_label: QualityLabel,
    pub coverage_percent: u32,
}

/// Three-bucket quality label. The UI must render this verbatim — it is the
/// only quality signal a user sees.
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

/// Outcome of `finish_enrollment`. The UI does not see raw scores; only a
/// coarse success/failure with a human reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
pub struct FinishResult {
    pub success: bool,
    pub profile_id: Option<String>,
    pub reason: Option<String>,
}

/// Daemon health surfaced to the settings panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
pub struct StatusInfo {
    pub daemon_reachable: bool,
    pub camera_present: bool,
    pub active_profile: Option<String>,
    pub version: Option<String>,
    pub always_on_emitter: bool,
}

/// All ways a manager call can fail that the UI must distinguish. These map
/// 1:1 to the D-Bus error names the service raises (task 25).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagerError {
    /// `org.namtor.Error.DaemonUnavailable` — the session service is not
    /// registered on the bus. UI shows recovery guidance.
    #[error("daemon unavailable: {0}")]
    DaemonUnavailable(String),
    /// `org.freedesktop.DBus.Error.ServiceUnknown` — bus itself reachable
    /// but the namtor service is gone.
    #[error("namtor service not registered on the bus")]
    ServiceNotRegistered,
    /// `org.namtor.Error.AuthRequired` / polkit denial. UI shows that
    /// administrator approval is needed.
    #[error("polkit denied: {0}")]
    PolkitDenied(String),
    /// `org.namtor.Error.NotFound` — profile/session id unknown.
    #[error("not found: {0}")]
    NotFound(String),
    /// `org.namtor.Error.Conflict` — duplicate name or in-flight session.
    #[error("conflict: {0}")]
    Conflict(String),
    /// `org.namtor.Error.NotEnrolling` — finish/cancel called outside a
    /// session.
    #[error("not enrolling: {0}")]
    NotEnrolling(String),
    /// Any other transport or protocol failure.
    #[error("dbus transport error: {0}")]
    Transport(String),
}

pub type ManagerResult<T> = Result<T, ManagerError>;

/// Trait every manager backend implements. The GNOME presenter depends only
/// on this trait; the real D-Bus client ([`crate::dbus_client`]) and the test
/// fake ([`crate::fake`]) are interchangeable.
///
/// Methods mirror the `org.namtor.Manager` interface 1:1. Signals
/// `enrollment_progress` and `enrollment_complete` are surfaced as polled
/// [`Self::get_progress`] / [`Self::finish_enrollment`] so the trait stays
/// synchronous and trivially testable; the live client caches the latest
/// signal emission for `get_progress`.
pub trait ManagerClient {
    fn list_profiles(&mut self) -> ManagerResult<Vec<ProfileInfo>>;
    fn create_profile(&mut self, name: &str) -> ManagerResult<SessionId>;
    fn start_enrollment(&mut self, session: &SessionId) -> ManagerResult<()>;
    fn get_progress(&mut self, session: &SessionId) -> ManagerResult<EnrollmentProgress>;
    fn finish_enrollment(&mut self, session: &SessionId) -> ManagerResult<FinishResult>;
    fn cancel_enrollment(&mut self, session: &SessionId) -> ManagerResult<()>;
    fn merge_profiles(&mut self, src: &ProfileId, dst: &ProfileId) -> ManagerResult<()>;
    fn replace_profile(&mut self, id: &ProfileId, name: &str) -> ManagerResult<()>;
    fn rename_profile(&mut self, id: &ProfileId, new_name: &str) -> ManagerResult<()>;
    fn enable_profile(&mut self, id: &ProfileId, enabled: bool) -> ManagerResult<()>;
    fn delete_profile(&mut self, id: &ProfileId) -> ManagerResult<()>;
    fn status(&mut self) -> ManagerResult<StatusInfo>;
}
