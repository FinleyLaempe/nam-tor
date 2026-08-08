// SPDX-License-Identifier: GPL-3.0-only
//! UI components for the Nam-tor management application.
//!
//! This module provides the visual building blocks for the application,
//! ensuring consistency with the design system defined in `DESIGN.md`.

pub mod enrollment;
pub mod profile_list;
pub mod recovery;
pub mod review;

pub use enrollment::EnrollmentViewComponent;
pub use profile_list::ProfileListView;
pub use recovery::RecoveryView;
pub use review::ReviewViewComponent;
