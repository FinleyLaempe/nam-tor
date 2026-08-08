// SPDX-License-Identifier: GPL-3.0-only
//! D-Bus contract types shared between the KCM bridge and the (test) fake
//! service. These mirror the frozen `org.namtor.Manager` wire format from
//! Task 8 exactly (`docs/evidence/dbus-contract.xml`). They MUST stay free of
//! biometric payload: no embeddings, frames, raw match scores, or thresholds
//! (spec §9.4, privacy scan enforced in [`crate::tests`]).
//!
//! ## Frozen wire signatures (Task 8)
//!
//! | Type | Wire | Fields |
//! |---|---|---|
//! | `ProfileInfo` | `(ssbtt)` | id, name, enabled, created, last_used |
//! | `EnrollmentProgress` | `(susu)` | session_id, frames_captured, quality_label, coverage_percent |
//! | `ReviewOutcome` | `(sbs)` | profile_id, needs_reenroll, quality |
//! | `StatusReport` | `(ua(ssbtt)b)` | api_version, profiles, emitter_armed |

use std::fmt;

use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

/// One enrolled face profile as exposed to the UI (frozen wire `(ssbtt)`).
///
/// `last_used` is `0` when the profile has never authenticated. It is the
/// only timestamp surfaced; creation/modification of templates stays daemon
/// side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProfileInfo {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub created: u64,
    pub last_used: u64,
}

/// Live enrollment progress emitted as the `EnrollmentProgress` signal (frozen
/// wire `(susu)`). `quality_label` is a human-readable bucket
/// ("low" / "medium" / "high") chosen by the daemon, never a numeric score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct EnrollmentProgress {
    pub session_id: String,
    pub frames_captured: u32,
    pub quality_label: String,
    pub coverage_percent: u32,
}

/// Coarse outcome of `ReviewProfile` (frozen wire `(sbs)`): the reviewed
/// profile id, whether re-enrollment is recommended, and a quality bucket.
/// No score, no threshold, no embedding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ReviewOutcome {
    pub profile_id: String,
    pub needs_reenroll: bool,
    pub quality: String,
}

/// Snapshot returned by `GetStatus` (frozen wire `(ua(ssbtt)b)`): API version,
/// the caller's profiles, and whether the IR emitter is armed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct StatusReport {
    pub api_version: u32,
    pub profiles: Vec<ProfileInfo>,
    pub emitter_armed: bool,
}

/// Outcome of an enrollment attempt as the KCM surfaces it to QML. Distinct
/// from the wire types so the UI layer can model cancel and auth-loss without
/// inspecting D-Bus errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollOutcome {
    Success,
    Cancelled,
    Failed,
}

/// Errors the bridge can return to QML. Kept deliberately small and free of
/// D-Bus internals so the QML side can pattern-match without pulling zbus.
#[derive(Debug)]
pub enum KcmError {
    /// The `org.namtor.Manager` service is not reachable on the bus.
    DaemonUnavailable,
    /// A method call failed (transport or daemon-reported).
    Call(String),
}

impl fmt::Display for KcmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DaemonUnavailable => f.write_str("namtor daemon unavailable"),
            Self::Call(msg) => write!(f, "namtor D-Bus call failed: {msg}"),
        }
    }
}

impl std::error::Error for KcmError {}

impl From<zbus::Error> for KcmError {
    fn from(err: zbus::Error) -> Self {
        // A method call against a missing well-known name surfaces as a
        // ServiceUnknown / NameHasNoOwner fdo error (or the lower-level
        // MethodError carrying the same error name). Treat all of these as
        // "daemon down" so the KCM renders its offline state (§9.4) without
        // leaking the underlying transport detail to QML.
        let down = match &err {
            zbus::Error::Address(_) => true,
            zbus::Error::FDO(boxed) => matches!(
                boxed.as_ref(),
                zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_)
            ),
            zbus::Error::MethodError(name, _, _) => matches!(
                name.as_str(),
                "org.freedesktop.DBus.Error.ServiceUnknown"
                    | "org.freedesktop.DBus.Error.NameHasNoOwner"
                    | "org.freedesktop.DBus.Error.NoReply"
            ),
            _ => false,
        };
        if down {
            Self::DaemonUnavailable
        } else {
            Self::Call(err.to_string())
        }
    }
}
