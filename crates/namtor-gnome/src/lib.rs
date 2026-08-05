// SPDX-License-Identifier: GPL-3.0-only
//! GNOME enrollment wizard and settings for Nam-tor (spec §9.2).
//!
//! GTK4 + libadwaita application. Provides:
//! - Enrollment wizard: capture IR frames → quality check → store templates
//! - Enrolled users list with delete capability
//! - Device status (daemon reachable, camera present)
//!
//! Talks directly to the template store (namtor-store) for enrollment
//! management, and to the daemon socket for status. A full D-Bus service
//! (org.namtor.Manager, §9.1) is a future enhancement.

pub mod store;

use std::path::PathBuf;

pub fn default_store_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("NAMTOR_STORE_DIR").unwrap_or_else(|_| "/var/lib/namtor/templates".into()),
    )
}

pub fn default_key_path() -> PathBuf {
    PathBuf::from(
        std::env::var("NAMTOR_KEY_PATH").unwrap_or_else(|_| "/var/lib/namtor/templates.key".into()),
    )
}

pub fn default_socket_path() -> PathBuf {
    PathBuf::from(std::env::var("NAMTOR_SOCKET").unwrap_or_else(|_| "/run/namtor/auth.sock".into()))
}
