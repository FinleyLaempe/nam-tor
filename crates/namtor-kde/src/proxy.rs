// SPDX-License-Identifier: GPL-3.0-only
//! Generated `org.namtor.Manager` D-Bus proxy matching the frozen wire
//! contract (Task 8, `docs/evidence/dbus-contract.xml`). One source of truth
//! for the KDE KCM; the GNOME UI is expected to reuse the same interface
//! definition to prevent drift.
//!
//! ## Frozen method set (11 methods, 4 signals, 1 property)
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
//! Signal arg types are fully-qualified (`crate::types::...`) because the
//! `#[zbus(signal)]` macro generates a message struct named after the signal
//! in PascalCase (e.g. `EnrollmentProgress`) which would otherwise shadow the
//! body type of the same name.

#[zbus::proxy(interface = "org.namtor.Manager", assume_defaults = true)]
pub trait Manager {
    // --- Methods (frozen Task 8 contract) ---

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

    // --- Property ---

    #[zbus(property)]
    fn api_version(&self) -> zbus::Result<u32>;

    // --- Signals (frozen Task 8 contract: 4 signals) ---

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
