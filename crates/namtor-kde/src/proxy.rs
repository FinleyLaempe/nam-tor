// SPDX-License-Identifier: GPL-3.0-only
//! Generated `org.namtor.Manager` D-Bus proxies matching the frozen wire
//! contracts. One source of truth for the KDE KCM; the GNOME UI is expected
//! to reuse the same interface definitions to prevent drift.
//!
//! Each interface lives in its own submodule because the `#[zbus(signal)]`
//! macro generates a `SignalArgs` struct named after the signal in PascalCase
//! (e.g. `EnrollmentProgress`) at the enclosing module scope. Both v1 and v3
//! expose `EnrollmentProgress`, so without module separation the two
//! generated structs would collide.
//!
//! ## v1 frozen method set (11 methods, 4 signals, 1 property)
//!
//! | Method | In → Out |
//! |---|---|
//! | `ListProfiles` | () → `a(ssbtt)` |
//! | `StartEnrollment` | (u, s) → s |
//! | `CancelEnrollment` | (s) → |
//! | `CommitProfile` | (s) → s |
//! | `ReviewProfile` | (u, s) → `(sbs)` |
//! | `RenameProfile` | (u, s, s) → |
//! | `EnableProfile` | (u, s, b) → |
//! | `DeleteProfile` | (u, s) → |
//! | `MergeProfiles` | (u, s, s) → |
//! | `ReplaceProfile` | (u, s, s) → |
//! | `GetStatus` | () → `(ua(ssbtt)b)` |
//!
//! ## v3 frozen method set (6 methods, 4 signals, 1 property)
//!
//! | Method | In → Out |
//! |---|---|
//! | `StartEnrollment` | (u, s) → s |
//! | `CancelEnrollment` | (s) → |
//! | `CommitProfile` | (s) → s |
//! | `StopEnrollment` | (s) → |
//! | `ResumeEnrollment` | (s) → |
//! | `ReviewEnrollment` | (s) → `(sbuuus)` |
//!
//! The v3 `EnrollmentProgress` signal carries a single `(ssuuuuuys)` progress
//! struct (session is the struct's first field); v1's carries `(s, susu)`.

/// v1 `org.namtor.Manager` proxy (frozen Task 8 contract).
pub mod v1 {
    #[zbus::proxy(interface = "org.namtor.Manager", assume_defaults = true)]
    pub trait Manager {
        fn list_profiles(&self) -> zbus::Result<Vec<crate::types::ProfileInfo>>;

        fn start_enrollment(&self, uid: u32, name: &str) -> zbus::Result<String>;

        fn cancel_enrollment(&self, session: &str) -> zbus::Result<()>;

        fn commit_profile(&self, session: &str) -> zbus::Result<String>;

        fn review_profile(&self, uid: u32, id: &str) -> zbus::Result<crate::types::ReviewOutcome>;

        fn rename_profile(&self, uid: u32, id: &str, name: &str) -> zbus::Result<()>;

        fn enable_profile(&self, uid: u32, id: &str, enabled: bool) -> zbus::Result<()>;

        fn delete_profile(&self, uid: u32, id: &str) -> zbus::Result<()>;

        fn merge_profiles(&self, uid: u32, src: &str, dst: &str) -> zbus::Result<()>;

        fn replace_profile(&self, uid: u32, id: &str, name: &str) -> zbus::Result<()>;

        fn get_status(&self) -> zbus::Result<crate::types::StatusReport>;

        #[zbus(property)]
        fn api_version(&self) -> zbus::Result<u32>;

        #[zbus(signal)]
        fn enrollment_progress(
            &self,
            session: String,
            progress: crate::types::EnrollmentProgress,
        ) -> zbus::Result<()>;

        #[zbus(signal)]
        fn enrollment_completed(&self, session: String, profile: String) -> zbus::Result<()>;

        #[zbus(signal)]
        fn profile_changed(&self, uid: u32, id: String) -> zbus::Result<()>;

        #[zbus(signal)]
        fn status_changed(&self, status: crate::types::StatusReport) -> zbus::Result<()>;
    }
}

/// v3 `org.namtor.Manager3` proxy (enrollment redesign, frozen contract in
/// `docs/evidence/dbus-contract-v3.xml`).
pub mod v3 {
    #[zbus::proxy(interface = "org.namtor.Manager3", assume_defaults = true)]
    pub trait Manager3 {
        fn start_enrollment(&self, uid: u32, name: &str) -> zbus::Result<String>;

        fn cancel_enrollment(&self, session: &str) -> zbus::Result<()>;

        fn commit_profile(&self, session: &str) -> zbus::Result<String>;

        fn stop_enrollment(&self, session: &str) -> zbus::Result<()>;

        fn resume_enrollment(&self, session: &str) -> zbus::Result<()>;

        fn review_enrollment(
            &self,
            session: &str,
        ) -> zbus::Result<crate::types::EnrollmentReviewV3>;

        #[zbus(property)]
        fn api_version(&self) -> zbus::Result<u32>;

        /// Single `(ssuuuuuys)` argument; session is the struct's first field.
        #[zbus(signal)]
        fn enrollment_progress(
            &self,
            progress: crate::types::EnrollmentProgressV3,
        ) -> zbus::Result<()>;

        #[zbus(signal)]
        fn enrollment_completed(&self, session: String, profile: String) -> zbus::Result<()>;

        #[zbus(signal)]
        fn profile_changed(&self, uid: u32, id: String) -> zbus::Result<()>;

        #[zbus(signal)]
        fn status_changed(&self, status: crate::types::StatusReport) -> zbus::Result<()>;
    }
}

pub use v1::ManagerProxy;
pub use v3::Manager3Proxy;
