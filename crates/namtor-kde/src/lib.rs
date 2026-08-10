// SPDX-License-Identifier: GPL-3.0-only
//! KDE System Settings KCM bridge for Nam-tor (spec §9.3).
//!
//! QML + Kirigami KCM backed by a thin Rust bridge that talks exclusively to
//! the `org.namtor.Manager` D-Bus service (Task 25 contract). The bridge never
//! touches the template store, the daemon socket, or any biometric data — all
//! enrollment, profile management, and status flows go through D-Bus, so the
//! KCM runs fully unprivileged and is policy-kit gated on the daemon side.
//!
//! No biometric data (embeddings, frames, raw scores, match thresholds) ever
//! crosses this boundary. `ProfileInfo`, `EnrollmentProgress`, and
//! `StatusInfo` carry only metadata the spec permits in a UI (§9.4).
//!
//! The contract suite in [`contract`] is shared shape-for-shape with the GNOME
//! UI so the two front-ends cannot drift; both drive the same D-Bus types and
//! the same happy / error / cancel / auth-loss / offline scenarios.

pub mod abi;
mod bridge;
pub mod contract;
mod proxy;
mod types;

pub use bridge::{EnrollmentController, EnrollmentState, KcmBridge, ProfileList};
pub use types::{
    EnrollOutcome, EnrollmentProgress, EnrollmentProgressV3, EnrollmentReviewV3, KcmError,
    ProfileInfo, ReviewOutcome, StatusReport,
};

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
