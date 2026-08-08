// SPDX-License-Identifier: GPL-3.0-only
//! ABI smoke test (Rust integration side): asserts the exported C ABI returns
//! the canonical frozen D-Bus address strings and the expected ABI version.
//! The C-side counterpart (`tests/abi_smoke.cpp`, run via CTest) links the
//! cdylib and asserts the same values from C++; an internal drift check
//! (`src/tests.rs::abi_strings_match_bridge_constants`) catches a Rust-side
//! rename that the C literals did not pick up.
//!
//! No biometric data is involved — only the version integer and two public
//! D-Bus address strings already in the frozen introspection XML (Task 8).

use std::ffi::CStr;

use namtor_kde::abi;

#[test]
fn abi_version_matches_constant() {
    assert_eq!(
        abi::namtor_kde_abi_version(),
        abi::NAMTOR_KDE_ABI_VERSION,
        "ABI version function must return the compile-time constant"
    );
    assert_eq!(abi::NAMTOR_KDE_ABI_VERSION, 1);
}

#[test]
fn abi_bus_name_is_canonical() {
    let bus = unsafe {
        // SAFETY: namtor_kde_manager_bus_name returns a pointer to a
        // process-lifetime `&'static CStr` literal, always valid.
        CStr::from_ptr(abi::namtor_kde_manager_bus_name())
    };
    assert_eq!(bus.to_bytes(), b"org.namtor.Manager");
}

#[test]
fn abi_object_path_is_canonical() {
    let path = unsafe {
        // SAFETY: namtor_kde_manager_object_path returns a pointer to a
        // process-lifetime `&'static CStr` literal, always valid.
        CStr::from_ptr(abi::namtor_kde_manager_object_path())
    };
    assert_eq!(path.to_bytes(), b"/org/namtor/Manager");
}
