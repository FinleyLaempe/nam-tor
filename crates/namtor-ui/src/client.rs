// SPDX-License-Identifier: GPL-3.0-only
//! Re-exports of the shared D-Bus client types from [`namtor_client`].
//!
//! The wire types ([`ProfileInfo`], [`EnrollmentProgress`], [`StatusReport`],
//! etc.) and the [`ManagerClient`] trait now live in the `namtor-client` crate
//! so the UI and any other front-end share a single source of truth.

pub use namtor_client::client::ManagerClient;
pub use namtor_client::types::{
    EnrollmentProgress, EnrollmentProgressV3, EnrollmentReviewV3, Guidance, ManagerError,
    ManagerResult, PamServiceCompatibility, PamServiceInfo, ProfileId, ProfileInfo,
    ProtectionKeyKind, ProtectionStatus, QualityLabel, ReviewOutcome, SessionId, StatusReport,
    API_VERSION_V3,
};
