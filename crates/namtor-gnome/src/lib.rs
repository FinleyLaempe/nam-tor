// SPDX-License-Identifier: GPL-3.0-only
//! GNOME management UI for Nam-tor — D-Bus client (spec §9.1, §9.2).
//!
//! GTK4 + libadwaita application. The UI talks to the daemon exclusively
//! over the session D-Bus `org.namtor.Manager` interface (task 25). There is
//! no direct template-store access from the GNOME process: every operation
//! — list, create, enroll, rename, enable, delete, merge, replace — goes
//! through the bus. This keeps biometric data inside the privileged daemon
//! and lets polkit gate every mutation.
//!
//! Architecture:
//! - [`client`] — the `ManagerClient` trait + UI-facing data types.
//! - [`fake`] — in-memory test double.
//! - [`dbus_client`] — live zbus-backed client (feature `dbus`).
//! - [`presenter`] — headless state machine the GTK view renders.
//! - [`privacy`] — runtime scan ensuring no biometric data leaks into UI
//!   structs.
//!
//! The wizard binary (`namtor-enroll-wizard`) lives in `src/bin/wizard.rs`
//! and is the only place GTK is constructed.

pub mod client;
pub mod fake;
pub mod presenter;
pub mod privacy;

#[cfg(feature = "dbus")]
pub mod dbus_client;

#[cfg(feature = "dbus")]
pub use dbus_client::DbusManagerClient;

pub use client::{
    EnrollmentProgress, FinishResult, ManagerClient, ManagerError, ManagerResult, ProfileId,
    ProfileInfo, QualityLabel, SessionId, StatusInfo,
};
pub use fake::{FakeManagerClient, Mode as FakeMode};
pub use presenter::{
    after, EnrollmentView, ErrorView, PendingMerge, PendingReplace, PresenterModel,
    ProfilePresenter, ProfileRowView, QualityView, RecoveryHint,
};
