// SPDX-License-Identifier: Apache-2.0
//! `namtor-probe`: hardware probe and contribution CLI.
//!
//! Tiered emitter discovery against a real UVC device, plus candidate quirks
//! entry emission for upstream contribution. Apache-2.0 so the probe is
//! reusable by any project — IR camera detection is a Linux-wide problem.

pub mod describe;
pub mod emit;
pub mod flywheel;
pub mod probe;
