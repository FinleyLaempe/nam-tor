// SPDX-License-Identifier: Apache-2.0
//! UVC extension unit access, USB descriptor parsing, and payload-header
//! metadata parsing for Nam-tor.
//!
//! Licensed Apache-2.0 so any project can consume the hardware database this
//! crate underpins. IR camera detection is a Linux-wide problem.

pub mod descriptors;
pub mod device;
pub mod discover;
pub mod emitter;
pub mod metadata;
pub mod mode;
pub mod probe;
pub mod strobe;
pub mod xu;
