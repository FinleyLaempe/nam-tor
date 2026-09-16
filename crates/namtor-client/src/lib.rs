// SPDX-FileCopyrightText: 2026 Nam-tor Project
// SPDX-License-Identifier: GPL-3.0-only

//! Shared D-Bus client crate for Nam-tor UIs — wire types, zbus proxies,
//! [`ManagerClient`] trait, live D-Bus backend, and test fake.
//!
//! The UI crate (`namtor-ui`) depends on this crate so that it and
//! `namtor-cli` cannot drift in their D-Bus contract surface.

#![warn(missing_docs)]

pub mod client;
pub mod contract;
pub mod dbus_client;
pub mod fake;
pub mod fake_bus;
pub mod proxy;
pub mod types;

// Re-export key types at the crate root for convenience.
pub use client::ManagerClient;
pub use dbus_client::DbusManagerClient;
pub use fake::{FakeManagerClient, Mode as FakeMode};
pub use types::{
    EnrollmentProgress, EnrollmentProgressV3, EnrollmentReviewV3, Guidance, ManagerError,
    ManagerResult, PamServiceCompatibility, PamServiceInfo, ProfileId, ProfileInfo,
    ProtectionKeyKind, ProtectionStatus, QualityLabel, ReviewOutcome, SessionId, StatusReport,
    API_VERSION_V3,
};
