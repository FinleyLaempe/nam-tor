// SPDX-License-Identifier: GPL-3.0-only
//! Nam-tor management UI — D-Bus client (spec §9.1, §9.2).
//!
//! Desktop-neutral: GTK4/libadwaita, but no GNOME-only runtime dependency.
//!
//! GTK4 + libadwaita application. The UI talks to the daemon exclusively
//! over the **system** D-Bus `org.namtor.Manager` (v1) and `org.namtor.Manager2`
//! (v2) interfaces (task 14). There is no direct template-store access from
//! the UI process: every operation — list, enroll, commit, rename,
//! enable, delete, merge, replace, PAM toggle — goes through the bus. This
//! keeps biometric data inside the privileged daemon and lets polkit gate
//! every mutation.
//!
//! Architecture:
//! - [`client`] — the `ManagerClient` trait + UI-facing data types.
//! - [`fake`] — in-memory test double.
//! - [`dbus_client`] — live zbus-backed client (feature `dbus`).
//! - [`presenter`] — headless state machine the GTK view renders.
//! - [`components`] — the single enrollment rendering path (capture, review,
//!   profile list, PAM services, system status).
//! - [`privacy`] — runtime scan ensuring no biometric data leaks into UI
//!   structs.
//!
//! The wizard binary (`namtor-enroll-wizard`) lives in `src/bin/wizard.rs`
//! and is the only place GTK is constructed.
//!
//! ---------------------------------------------------------------------------

/// GTK application id.
///
/// MUST equal the installed desktop entry's basename and its
/// `StartupWMClass`, or the compositor cannot associate the window with its
/// `.desktop` file and shows a fallback icon and name. It is also the
/// D-Bus name GTK claims for window activation, so it is ABI for installed
/// systems: it does NOT track the crate name.
pub const APP_ID: &str = "org.namtor.Wizard";

pub mod a11y;
pub mod app;
pub mod client;
pub mod components;
pub mod fake;
pub mod polkit_auth;
pub mod presenter;
pub mod privacy;
pub mod shell;
pub mod view_model;

/// Translation seam for user-facing strings.
///
/// Every label, tooltip, accessible name, status title, and guidance phrase
/// shown in the GTK UI MUST pass through `tr()` so the string is the gettext
/// msgid. In the test environment `tr()` is the identity function (no
/// `.mo` catalog is loaded); in production the wizard binary loads a
/// `gettext::Catalog` and rebinds this to look up the msgid.
///
/// Tests in `tests/shell_navigation.rs` assert that user-facing string
/// literals are wrapped in `tr()` calls and that no forbidden biometric /
/// internal wording leaks through.
pub fn tr(s: &str) -> String {
    s.to_string()
}

pub use a11y::{ProgressAnnouncer, PROGRESS_ANNOUNCE_INTERVAL};
pub use shell::{Destination, Shell};

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
    compatibility_label, sealing_label, CaptureSignal, EnrollmentState, EnrollmentView,
    EnrollmentViewV3, ErrorView, GuidanceView, PamServiceRowView, PendingMerge, PendingReplace,
    PresenterModel, ProfileRowView, ProfileSetupView, ProtectionStatusView, ProtectionView,
    QualityView, ReadinessLevel, ReadinessView, RecoveryHint, ReviewView, ReviewViewV3,
};
