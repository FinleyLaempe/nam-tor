// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-camera`: emitter lifecycle, watchdog, resume, hotplug, and
//! multi-camera selection. Sits between the daemon and the capture layer.

pub mod emitter_ctrl;
pub mod watchdog;
