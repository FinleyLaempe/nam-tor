// SPDX-License-Identifier: GPL-3.0-only
//! GNOME management UI for Nam-tor — D-Bus client (spec §9.1, §9.2).
//!
//! GTK4 + libadwaita application. The UI talks to the daemon exclusively
//! over the **system** D-Bus `org.namtor.Manager` (v1) and `org.namtor.Manager2`
//! (v2) interfaces (task 14). There is no direct template-store access from
//! the GNOME process: every operation — list, enroll, commit, rename,
//! enable, delete, merge, replace, PAM toggle — goes through the bus. This
//! keeps biometric data inside the privileged daemon and lets polkit gate
//! every mutation.
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
//!
//! ---------------------------------------------------------------------------

pub fn tr(s: &str) -> String {
    s.to_string()
}

pub mod client;
pub mod components;
pub mod fake;
pub mod navigation;
pub mod presenter;
pub mod privacy;
pub mod shell;
pub mod view_model;

pub use shell::{Destination, Shell};
pub use navigation::EnrollmentNavigation;

#[cfg(feature = "dbus")]
pub mod dbus_client;

#[cfg(feature = "dbus")]
pub use dbus_client::DbusManagerClient;

pub use client::{
    EnrollmentProgress, EnrollmentProgressV3, EnrollmentReviewV3, Guidance, ManagerClient,
    ManagerError, ManagerResult, PamServiceCompatibility, PamServiceInfo, ProfileId, ProfileInfo,
    ProtectionKeyKind, ProtectionStatus, QualityLabel, ReviewOutcome, SessionId, StatusReport,
    API_VERSION_V3,
};
pub use fake::{FakeManagerClient, Mode as FakeMode};
pub use presenter::ProfilePresenter;
pub use view_model::{
    compatibility_label, sealing_label, EnrollmentState, EnrollmentView, EnrollmentViewV3,
    ErrorView, PamServiceRowView, PendingMerge, PendingReplace, PresenterModel, ProfileRowView,
    ProtectionStatusView, ProtectionView, QualityView, ReadinessLevel, ReadinessView, RecoveryHint,
    ReviewView, ReviewViewV3,
};
