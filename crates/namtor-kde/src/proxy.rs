// SPDX-License-Identifier: GPL-3.0-only
//! Generated `org.namtor.Manager` D-Bus proxy. One source of truth for the
//! Task 25 contract; the GNOME UI is expected to reuse the same interface
//! definition to prevent drift.
//!
//! Signal arg types are fully-qualified (`crate::types::...`) because the
//! `#[zbus(signal)]` macro generates a message struct named after the signal
//! in PascalCase (e.g. `EnrollmentProgress`) which would otherwise shadow the
//! body type of the same name.

#[zbus::proxy(interface = "org.namtor.Manager", assume_defaults = true)]
pub trait Manager {
    fn list_profiles(&self) -> zbus::Result<Vec<crate::types::ProfileInfo>>;
    fn create_profile(&self, name: &str) -> zbus::Result<String>;
    fn start_enrollment(&self, session_id: &str) -> zbus::Result<()>;
    fn get_progress(&self, session_id: &str) -> zbus::Result<crate::types::EnrollmentProgress>;
    fn finish_enrollment(&self, session_id: &str) -> zbus::Result<crate::types::FinishResult>;
    fn cancel_enrollment(&self, session_id: &str) -> zbus::Result<()>;
    fn merge_profiles(&self, src: &str, dst: &str) -> zbus::Result<()>;
    fn replace_profile(&self, id: &str, name: &str) -> zbus::Result<()>;
    fn rename_profile(&self, id: &str, new_name: &str) -> zbus::Result<()>;
    fn enable_profile(&self, id: &str, enabled: bool) -> zbus::Result<()>;
    fn delete_profile(&self, id: &str) -> zbus::Result<()>;
    fn status(&self) -> zbus::Result<crate::types::StatusInfo>;

    #[zbus(signal)]
    fn enrollment_progress(
        &self,
        session_id: String,
        progress: crate::types::EnrollmentProgress,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    fn enrollment_complete(
        &self,
        session_id: String,
        result: crate::types::FinishResult,
    ) -> zbus::Result<()>;
}
