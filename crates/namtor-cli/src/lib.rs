// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-cli`: user-facing CLI for Nam-tor.
//!
//! Currently exposes `namtor-cli status`, which reports the selected device,
//! stream state, and protection level. Enrollment and diagnostics subcommands
//! arrive in later milestones.

pub mod diagnostics;
pub mod enroll;
pub mod enroll_session;
pub mod install_layout;
pub mod list;
pub mod status;
pub mod verify;
