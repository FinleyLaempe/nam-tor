// SPDX-License-Identifier: GPL-3.0-only
// The zbus proxy macro generates structs/methods without doc comments;
// suppress missing_docs at the module level to avoid macro-generated noise.
#![allow(missing_docs)]

//! Generated `org.namtor.Manager` D-Bus proxies matching the frozen wire
//! contracts in `docs/evidence/dbus-contract-v3.xml`.
//!
//! Each interface lives in its own submodule because the `#[zbus(signal)]`
//! macro generates a `SignalArgs` struct named after the signal in PascalCase
//! (e.g. `EnrollmentProgress`) at the enclosing module scope. Both v1 and v3
//! expose `EnrollmentProgress`, so without module separation the two
//! generated structs would collide. Types are referenced via fully-qualified
//! `crate::types::` paths to avoid further name collisions with the generated
//! signal structs.

/// v1 `org.namtor.Manager` proxy (frozen contract).
pub mod v1 {
    /// v1 Manager proxy trait.
    #[zbus::proxy(
        default_service = "org.namtor.Manager",
        default_path = "/org/namtor/Manager",
        interface = "org.namtor.Manager"
    )]
    pub trait Manager {
        /// List caller's profiles.
        fn list_profiles(&self) -> zbus::Result<Vec<crate::types::ProfileInfo>>;

        /// Start an enrollment session.
        fn start_enrollment(&self, uid: u32, name: &str) -> zbus::Result<String>;

        /// Cancel an enrollment session.
        fn cancel_enrollment(&self, session: &str) -> zbus::Result<()>;

        /// Commit a session into a persistent profile.
        fn commit_profile(&self, session: &str) -> zbus::Result<String>;

        /// Coarse re-review of an existing profile.
        fn review_profile(&self, uid: u32, id: &str) -> zbus::Result<crate::types::ReviewOutcome>;

        /// Relabel a profile.
        fn rename_profile(&self, uid: u32, id: &str, name: &str) -> zbus::Result<()>;

        /// Toggle a profile.
        fn enable_profile(&self, uid: u32, id: &str, enabled: bool) -> zbus::Result<()>;

        /// Remove a profile.
        fn delete_profile(&self, uid: u32, id: &str) -> zbus::Result<()>;

        /// Fold src into dst, remove src.
        fn merge_profiles(&self, uid: u32, src: &str, dst: &str) -> zbus::Result<()>;

        /// Re-name an existing profile in place.
        fn replace_profile(&self, uid: u32, id: &str, name: &str) -> zbus::Result<()>;

        /// Daemon health + profile snapshot.
        fn get_status(&self) -> zbus::Result<crate::types::StatusReport>;

        /// Read-only API version property.
        #[zbus(property)]
        fn api_version(&self) -> zbus::Result<u32>;

        /// Enrollment progress signal.
        #[zbus(signal)]
        fn enrollment_progress(
            &self,
            session: String,
            progress: crate::types::EnrollmentProgress,
        ) -> zbus::Result<()>;

        /// Enrollment completed signal.
        #[zbus(signal)]
        fn enrollment_completed(&self, session: String, profile: String) -> zbus::Result<()>;

        /// Profile changed signal.
        #[zbus(signal)]
        fn profile_changed(&self, uid: u32, id: String) -> zbus::Result<()>;

        /// Status changed signal.
        #[zbus(signal)]
        fn status_changed(&self, status: crate::types::StatusReport) -> zbus::Result<()>;
    }
}

/// v2 `org.namtor.Manager2` proxy (PAM service management + protection status).
pub mod v2 {
    /// v2 Manager2 proxy trait.
    #[zbus::proxy(
        default_service = "org.namtor.Manager",
        default_path = "/org/namtor/Manager",
        interface = "org.namtor.Manager2"
    )]
    pub trait Manager2 {
        /// List detected PAM auth services.
        fn list_pam_services(&self) -> zbus::Result<Vec<crate::types::PamServiceInfo>>;

        /// Toggle namtor for a PAM service.
        fn set_pam_service_enabled(
            &self,
            service_id: &str,
            enabled: bool,
        ) -> zbus::Result<bool>;

        /// Template storage protection status.
        fn get_protection_status(&self) -> zbus::Result<crate::types::ProtectionStatus>;

        /// Read-only API version property.
        #[zbus(property)]
        fn api_version(&self) -> zbus::Result<u32>;

        /// PAM services changed signal.
        #[zbus(signal)]
        fn pam_services_changed(
            &self,
            services: Vec<crate::types::PamServiceInfo>,
        ) -> zbus::Result<()>;

        /// Protection status changed signal.
        #[zbus(signal)]
        fn protection_status_changed(&self, status: crate::types::ProtectionStatus)
            -> zbus::Result<()>;
    }
}

/// v3 `org.namtor.Manager3` proxy (enrollment redesign, frozen contract in
/// `docs/evidence/dbus-contract-v3.xml`).
pub mod v3 {
    /// v3 Manager3 proxy trait.
    #[zbus::proxy(
        default_service = "org.namtor.Manager",
        default_path = "/org/namtor/Manager",
        interface = "org.namtor.Manager3"
    )]
    pub trait Manager3 {
        /// Start a v3 enrollment session.
        fn start_enrollment(&self, uid: u32, name: &str) -> zbus::Result<String>;

        /// Cancel a v3 enrollment session.
        fn cancel_enrollment(&self, session: &str) -> zbus::Result<()>;

        /// Commit a v3 session into a persistent profile.
        fn commit_profile(&self, session: &str) -> zbus::Result<String>;

        /// Stop capture, retain reservoir.
        fn stop_enrollment(&self, session: &str) -> zbus::Result<()>;

        /// Resume capture from stopped state.
        fn resume_enrollment(&self, session: &str) -> zbus::Result<()>;

        /// Review a stopped enrollment session.
        fn review_enrollment(
            &self,
            session: &str,
        ) -> zbus::Result<crate::types::EnrollmentReviewV3>;

        /// Poll the latest enrollment progress.
        fn poll_progress(
            &self,
            session: &str,
        ) -> zbus::Result<(bool, crate::types::EnrollmentProgressV3)>;

        /// Read-only API version property.
        #[zbus(property)]
        fn api_version(&self) -> zbus::Result<u32>;

        /// Single `(ssuuuuuys)` argument; session is the struct's first field.
        #[zbus(signal)]
        fn enrollment_progress(&self, progress: crate::types::EnrollmentProgressV3)
            -> zbus::Result<()>;

        /// Enrollment completed signal.
        #[zbus(signal)]
        fn enrollment_completed(&self, session: String, profile: String) -> zbus::Result<()>;

        /// Profile changed signal.
        #[zbus(signal)]
        fn profile_changed(&self, uid: u32, id: String) -> zbus::Result<()>;

        /// Status changed signal.
        #[zbus(signal)]
        fn status_changed(&self, status: crate::types::StatusReport) -> zbus::Result<()>;
    }
}

pub use v1::ManagerProxy;
pub use v2::Manager2Proxy;
pub use v3::Manager3Proxy;
