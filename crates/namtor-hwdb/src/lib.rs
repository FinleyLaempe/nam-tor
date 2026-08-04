// SPDX-License-Identifier: Apache-2.0
//! Hardware database for Nam-tor: sysfs USB enumeration, descriptor-based IR
//! classification, USB→V4L2 node correlation, and device quirks matching.
//!
//! Licensed Apache-2.0 so the quirks database and detection logic are reusable
//! by any project — IR camera detection is a Linux-wide problem.

use thiserror::Error;

pub mod sysfs;

/// Errors raised by the hardware database.
#[derive(Debug, Error)]
pub enum HwdbError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("sysfs error: {0}")]
    Sysfs(String),
    #[error("not found: {0}")]
    NotFound(String),
}
