// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-cli`: user-facing CLI for Nam-tor.
//!
//! Currently exposes `namtor-cli status`, which reports the selected device,
//! stream state, and protection level. Enrollment and diagnostics subcommands
//! arrive in later milestones.

pub mod diagnostics;
pub mod enroll;
pub mod enroll_session;
pub mod list;
pub mod package_diagnostics;
pub mod status;
pub mod verify;

// Re-export the install-layout contract from the shared `namtor-layout`
// crate so existing consumers (`crate::install_layout::...`) continue to
// work without a separate module file.
pub use namtor_layout as install_layout;
