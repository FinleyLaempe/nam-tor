# KDE KCM Build — Task 21

**Date:** 2026-08-08
**Commit:** (this commit)
**Scope:** Introduce a reproducible Plasma 6 / KF6 build for the Nam-tor KCM (spec §9.3). Build scaffolding only — the full async D-Bus bridge is Task 22.

## Files created

| File | Purpose |
|---|---|
| `crates/namtor-kde/CMakeLists.txt` | CMake build: QML module + ABI smoke test + optional KCM plugin |
| `crates/namtor-kde/metadata.json` | KPlugin JSON metadata (Plasma 6 format, replaces `metadata.desktop`) |
| `crates/namtor-kde/src/cpp/namtor_kde.h` | C ABI header (3 functions, 1 version constant) |
| `crates/namtor-kde/src/cpp/namtor_kcm.h` | C++ KCModule shim: `NamtorKcm` QML singleton |
| `crates/namtor-kde/src/cpp/namtor_kcm.cpp` | Shim implementation (ABI check + D-Bus address strings) |
| `crates/namtor-kde/tests/abi_smoke.cpp` | C-side ABI smoke test (CTest) |
| `crates/namtor-kde/src/abi.rs` | Rust C ABI export (`#[no_mangle] extern "C"`) |
| `docs/evidence/ui/kde/build.md` | This document |

## Files modified

| File | Change |
|---|---|
| `crates/namtor-kde/Cargo.toml` | Added `[lib] crate-type = ["cdylib", "rlib"]` for the C++ link |
| `crates/namtor-kde/src/lib.rs` | Added `pub mod abi;` |
| `crates/namtor-kde/src/tests.rs` | Added `abi_version_and_strings_match_bridge_constants` test |

## Build design

### C ABI surface (audited)

The Rust cdylib (`libnamtor_kde.so`) exports three `extern "C"` functions:

| Function | Returns | Purpose |
|---|---|---|
| `namtor_kde_abi_version()` | `uint32_t` | ABI version; C++ compares to `NAMTOR_KDE_ABI_VERSION` (1) |
| `namtor_kde_manager_bus_name()` | `const char *` | `"org.namtor.Manager"` (static lifetime) |
| `namtor_kde_manager_object_path()` | `const char *` | `"/org/namtor/Manager"` (static lifetime) |

The C++ shim checks `namtor_kde_abi_version()` at construction; on mismatch it
sets `abiOk=false` and `abiError` so QML renders the error state instead of
loading an incompatible bridge — the only safe response.

No biometric data (embeddings, frames, scores, thresholds) crosses this
boundary. Only the ABI version integer and two public D-Bus address strings
that are already in the frozen introspection XML (Task 8) are exposed.

### QML module

`qt_add_qml_module(namtorqml URI org.namtor VERSION 1.0)` compiles the C++
shim (`NamtorKcm` QML singleton with `QML_ELEMENT` + `QML_SINGLETON`) and
`qml/main.qml` into a loadable Qt6 QML module (`libnamtorqml.so`). The existing
`main.qml` imports `org.namtor 1.0` and binds `bridge` to the singleton.

### KCM plugin (conditional)

The full KCM plugin target (`kcoreaddons_add_plugin` + KPluginFactory) is
gated on `KF6KCMUtils` + `ECM` being found. When absent, the build produces
the QML module + ABI smoke test only. The packaging layer (install-manifest)
asserts the full dependency set at package-build time.

## Pinned build dependencies

| Dependency | Minimum | Found on this host |
|---|---|---|
| CMake | 3.28 | 4.4.2 |
| Qt6 (Core, Gui, Qml) | 6.6 | yes |
| KF6Kirigami | 6.0 | yes (6.28.0) |
| KF6CoreAddons | 6.0 | yes (6.28.0) |
| KF6I18n | 6.0 | yes (6.28.0) |
| KF6KCMUtils | 6.0 | **no** (WITHHELD) |
| ECM (Extra CMake Modules) | 6.0 | **no** (WITHHELD) |
| Rust cdylib `libnamtor_kde.so` | — | yes (cargo build) |

## Verification

### `cargo build -p namtor-kde` — PASS

```
$ cargo build -p namtor-kde
    Finished `dev` profile [unoptimized + debuginfo] in 0.25s
```

The cdylib is produced at `target/debug/libnamtor_kde.so` (32 MB debug, ~3 MB
release). Exported symbols verified:

```
$ nm -D target/debug/libnamtor_kde.so | grep namtor_kde
000000000004eb00 T namtor_kde_abi_version
000000000004eb10 T namtor_kde_manager_bus_name
000000000004eb40 T namtor_kde_manager_object_path
```

### `cargo test -p namtor-kde` — PASS (10/10)

```
running 10 tests
test tests::abi_version_and_strings_match_bridge_constants ... ok
test tests::auth_loss_preserves_profiles ... ok
test tests::cancel_keeps_state_safe ... ok
test tests::enrollment_signals_round_trip ... ok
test tests::error_daemon_unavailable ... ok
test tests::happy_path ... ok
test tests::merge_and_replace ... ok
test tests::offline_denied_cancel_keeps_state_safe ... ok
test tests::privacy_scan_no_biometric_data ... ok
test tests::shared_contract_suite ... ok

test result: ok. 10 passed; 0 failed; 0 ignored
```

The new `abi_version_and_strings_match_bridge_constants` test asserts:
1. `namtor_kde_abi_version()` returns `NAMTOR_KDE_ABI_VERSION` (1).
2. The C ABI bus name string matches `bridge::MANAGER_BUS_NAME`.
3. The C ABI object path string matches `bridge::MANAGER_OBJECT_PATH`.

### CMake configure — PASS (partial)

```
$ cmake -B build-kde -S .
-- KCM plugin build disabled (KF6KCMUtils_FOUND=0 ECM_FOUND=0).
   Only the QML module + abi-smoke test will be built.
-- Configuring done (2.5s)
-- Generating done (0.1s)
```

Configured successfully with the QML module + ABI smoke test. The full KCM
plugin target is WITHHELD pending `kf6-kcmutils` + `extra-cmake-modules`.

### CMake build — PASS

```
$ cmake --build build-kde
[ 83%] Built target namtorqml          → libnamtorqml.so (163 KB)
[100%] Built target namtor-abi-smoke   → namtor-abi-smoke (16 KB)
```

### CTest ABI smoke test — PASS

```
$ ctest --test-dir build-kde --output-on-failure
1/1 Test #1: namtor-abi-smoke ... Passed  0.00 sec
100% tests passed out of 1
```

Direct run:
```
$ ./build-kde/namtor-abi-smoke
ABI SMOKE OK: version=1 bus=org.namtor.Manager path=/org/namtor/Manager
```

### clippy + fmt + SPDX — PASS

```
$ cargo clippy -p namtor-kde -- -D warnings   # clean
$ cargo fmt -p namtor-kde -- --check          # clean
$ bash scripts/check-spdx.sh                  # passed
```

## WITHHELD

| Gate | Reason |
|---|---|
| `kcmshell6 kcm_namtor` loads without null bridge/import errors | `kcmshell6` is not installed on this host. Requires the full Plasma 6 system settings stack (`plasma-systemsettings` or equivalent). The QML module builds and links the Rust cdylib; runtime loading in a real Plasma session is the next HIL step. |
| Full KCM plugin target (`kcoreaddons_add_plugin`) | `kf6-kcmutils` and `extra-cmake-modules` packages are not installed. Install with: `sudo pacman -S kf6-kcmutils extra-cmake-modules`. The CMakeLists builds the plugin target when both are present. |

## What is NOT here (out of scope)

- **Full async D-Bus bridge** (Task 22): method calls, signal forwarding, QML profile model. The C++ shim only checks the ABI version and surfaces the D-Bus address strings.
- **Enrollment UI** (Task 23): the wizard QML exists in `main.qml` but the bridge methods it calls (`bridge.createProfile`, `bridge.startEnrollment`, etc.) are not wired through the C ABI yet.
- **Profile management UI** (Task 24): same — QML exists, bridge calls are Task 22.
