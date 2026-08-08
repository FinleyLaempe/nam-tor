// SPDX-License-Identifier: GPL-3.0-only
//
// namtor_kde.h — audited C ABI between the Rust `namtor-kde` cdylib and the
// C++ KCModule shim. This is the ONLY header the C++ side includes from the
// Rust crate; the surface is intentionally three functions and one constant.
//
// Contract:
//   - `NAMTOR_KDE_ABI_VERSION` must equal `namtor_kde::abi::NAMTOR_KDE_ABI_VERSION`
//     in crates/namtor-kde/src/abi.rs. Bump both together on any breaking
//     change. The Rust test `abi_version_and_strings_match_bridge_constants`
//     and the CTest `abi_smoke` together enforce this.
//   - The string pointers returned by `namtor_kde_manager_bus_name` and
//     `namtor_kde_manager_object_path` alias static storage and are valid for
//     the process lifetime; the caller must NOT free or modify them.
//   - No biometric data (embeddings, frames, scores, thresholds) crosses this
//     boundary — only the ABI version and two public D-Bus address strings
//     already present in the frozen introspection XML (Task 8).

#ifndef NAMTOR_KDE_H
#define NAMTOR_KDE_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

// Must stay in lock-step with `namtor_kde::abi::NAMTOR_KDE_ABI_VERSION`.
#define NAMTOR_KDE_ABI_VERSION 1u

// Returns the cdylib's ABI version. Compare against NAMTOR_KDE_ABI_VERSION;
// refuse to load the bridge on mismatch.
uint32_t namtor_kde_abi_version(void);

// Returns "org.namtor.Manager" (process-lifetime static C string).
const char *namtor_kde_manager_bus_name(void);

// Returns "/org/namtor/Manager" (process-lifetime static C string).
const char *namtor_kde_manager_object_path(void);

#ifdef __cplusplus
}
#endif

#endif // NAMTOR_KDE_H
