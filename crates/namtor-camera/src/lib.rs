// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-camera`: emitter lifecycle, watchdog, resume, hotplug, and
//! multi-camera selection. Sits between the daemon and the capture layer.

pub mod always_on;
pub mod emitter_ctrl;
pub mod hotplug;
pub mod inventory;
pub mod resume;
pub mod selection;
pub mod watchdog;
