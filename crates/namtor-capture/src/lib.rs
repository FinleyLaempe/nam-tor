// SPDX-License-Identifier: GPL-3.0-only
//! Capture abstraction for Nam-tor.
//!
//! Provides the [`CaptureSource`] trait implemented by every capture path,
//! along with recording, replay, and synthetic session generation.

pub mod frame;
pub mod recording;
pub mod replay;
pub mod source;
pub mod synth;

#[cfg(target_os = "linux")]
pub mod ir_uvc;
#[cfg(target_os = "linux")]
pub mod v4l2;
