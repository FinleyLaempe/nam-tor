// SPDX-License-Identifier: GPL-3.0-only
//! D-Bus contract types shared between the KCM bridge and the (test) fake
//! service.
//!
//! The shared wire types (`ProfileInfo`, `EnrollmentProgress`, `ReviewOutcome`,
//! `StatusReport`, `EnrollmentProgressV3`, `EnrollmentReviewV3`) are re-exported
//! from [`namtor_client`] so both GUI front-ends share a single source of truth.
//! KDE-specific types ([`EnrollOutcome`], [`KcmError`]) remain here.

use std::fmt;

/// One enrolled face profile as exposed to the UI (frozen wire `(ssbtt)`).
pub use namtor_client::types::ProfileInfo;

/// Live enrollment progress emitted as the `EnrollmentProgress` signal.
pub use namtor_client::types::EnrollmentProgress;

/// Coarse outcome of `ReviewProfile` (frozen wire `(sbs)`).
pub use namtor_client::types::ReviewOutcome;

/// Snapshot returned by `GetStatus` (frozen wire `(ua(ssbtt)b)`).
pub use namtor_client::types::StatusReport;

/// v3 enrollment progress (frozen wire `(ssuuuuuys)`).
pub use namtor_client::types::EnrollmentProgressV3;

/// v3 review snapshot (frozen wire `(sbuuus)`).
pub use namtor_client::types::EnrollmentReviewV3;

/// Re-export `QualityLabel` for constructing `EnrollmentProgress`/`ReviewOutcome`.
pub use namtor_client::types::QualityLabel;

/// Outcome of an enrollment attempt as the KCM surfaces it to QML. Distinct
/// from the wire types so the UI layer can model cancel and auth-loss without
/// inspecting D-Bus errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollOutcome {
    /// Enrollment succeeded.
    Success,
    /// User cancelled.
    Cancelled,
    /// Enrollment failed.
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
