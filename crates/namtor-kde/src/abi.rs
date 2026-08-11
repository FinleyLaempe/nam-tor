// SPDX-License-Identifier: GPL-3.0-only
//! Audited C ABI surface for the KCM C++ shim (Task 21 build scaffolding).
//!
//! This is deliberately tiny: the C++ `KCModule` shim loads the Rust cdylib,
//! calls [`namtor_kde_abi_version`] and compares it against the
//! `NAMTOR_KDE_ABI_VERSION` constant in `namtor_kde.h`. On mismatch the shim
//! refuses to register the QML type and surfaces an error instead of loading a
//! bridge that speaks a different ABI — the only safe response.
//!
//! The full async D-Bus bridge (method calls, signal forwarding, QML model
//! plumbing) is implemented in [`crate::bridge`] and the C++ KCModule shim
//! lives in `namtor_kcm.cpp`. The two string accessors below let the C++ side
//! build a `QDBusInterface` against the canonical `org.namtor.Manager` service
//! without hardcoding the bus name a second time, so the Rust crate stays the
//! single source of truth for the D-Bus address.
//!
//! No biometric data (embeddings, frames, scores, thresholds) crosses this
//! boundary — only the ABI version integer and two public D-Bus address
//! strings that are already in the frozen introspection XML (Task 8).

use std::ffi::CStr;
use std::os::raw::c_char;

/// ABI version of the C surface. Bumped on any breaking change to the
/// functions or types in this module. The C++ shim's
/// `NAMTOR_KDE_ABI_VERSION` must match exactly.
pub const NAMTOR_KDE_ABI_VERSION: u32 = 1;

/// Compile-time C string for [`MANAGER_BUS_NAME`]. Kept in sync with the Rust
/// bridge constant by the `abi_strings_match_bridge_constants` test so a
/// rename of the bus name cannot silently desync the C++ D-Bus proxy.
const C_BUS_NAME: &CStr = c"org.namtor.Manager";
/// Compile-time C string for [`MANAGER_OBJECT_PATH`]; see [`C_BUS_NAME`].
const C_OBJECT_PATH: &CStr = c"/org/namtor/Manager";

/// Returns the ABI version the cdylib was built with. The C++ shim compares
/// this to its compile-time `NAMTOR_KDE_ABI_VERSION` and refuses to load on
/// mismatch.
#[no_mangle]
pub extern "C" fn namtor_kde_abi_version() -> u32 {
    NAMTOR_KDE_ABI_VERSION
}

/// Returns the well-known D-Bus bus name of the manager service
/// (`org.namtor.Manager`). The returned pointer aliases static storage valid
/// for the program lifetime; the caller must NOT free or modify it.
#[no_mangle]
pub extern "C" fn namtor_kde_manager_bus_name() -> *const c_char {
    C_BUS_NAME.as_ptr()
}

/// Returns the canonical D-Bus object path of the manager service
/// (`/org/namtor/Manager`). Same lifetime contract as
/// [`namtor_kde_manager_bus_name`].
#[no_mangle]
pub extern "C" fn namtor_kde_manager_object_path() -> *const c_char {
    C_OBJECT_PATH.as_ptr()
}
