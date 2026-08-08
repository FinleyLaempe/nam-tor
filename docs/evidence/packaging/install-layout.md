<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Install Layout Reconciliation (Task 34)

**Date:** 2026-08-08  
**Author:** sisyphus-junior  
**Status:** complete — tests green, clippy clean, SPDX clean

## Goal

One authoritative install layout: all runtime paths derive from
`packaging/install-manifest.toml` (the single source of truth for installed
files) or `namtor-layout` (the single source of truth for runtime contract
paths). No hardcoded `/usr/lib`, `/var/lib`, `/etc` paths in Rust production
code outside the layout module's fallback defaults.

## What changed

### New crate: `namtor-layout`

Minimal shared crate (deps: `serde`, `toml`) extracted from the
`namtor-cli::install_layout` module. Both `namtor-daemon` and `namtor-cli`
depend on it.

**Public API:**

| Type | Purpose |
|---|---|
| `InstallRoot` | Enum: `Installed`, `Staged`, `Source` |
| `LayoutPaths` | Resolved paths: `daemon_bin`, `cli_bin`, `worker_bin`, `enroll_wizard_bin`, `models_dir`, `templates_dir`, `key_path`, `backoff_path`, `daemon_socket`, `quirks_dirs`, `manifest_path` |
| `ManifestDests` | All file/dir dests from the manifest (for cross-check tests) |
| `LayoutPaths::resolve(root, manifest_path, kind)` | Derives all paths from the manifest + runtime constants |
| `LayoutPaths::parse_all_dests(manifest_path)` | Returns every `[[files]]` dest and `[[dirs]]` path |
| `PAM_DEFAULT_SOCKET` | `"/run/namtor/auth.sock"` — the socket path the PAM module uses (kept as a const because `namtor-pam` cannot depend on `namtor-layout` without violating its < 500 KB / no-ML-deps constraint) |
| `REFERENCE_CONSTANTS` | Strings that must never appear in layout defaults |

**Runtime contract constants** (not installed files; declared in
`packaging/tmpfiles.d/namtor.conf`):

| Constant | Path |
|---|---|
| `RUNTIME_SOCKET_REL` | `run/namtor/auth.sock` |
| `STORE_DIR_REL` | `var/lib/namtor/templates` |
| `STORE_KEY_REL` | `var/lib/namtor/templates.key` |
| `MODELS_DIR_REL` | `var/lib/namtor/models` |
| `BACKOFF_REL` | `var/lib/namtor/backoff.json` |

### `namtor-cli` changes

- Deleted `src/install_layout.rs` (moved to `namtor-layout`).
- `lib.rs`: `pub use namtor_layout as install_layout;` — re-export preserves
  existing `crate::install_layout::...` import paths.
- `bin/namtor-cli.rs`: clap args changed from `default_value = "/var/lib/..."`
  to `Option<PathBuf>`; defaults filled from `LayoutPaths::resolve` at
  runtime. No hardcoded install paths in the CLI binary.
- `enroll.rs`: `emitter_cfg()` now uses `LayoutPaths::quirks_dirs` instead of
  a hardcoded `/usr/share/namtor/quirks/devices` array.

### `namtor-daemon` changes

- Added `namtor-layout` dependency.
- `bin/namtor-daemon.rs`: env var fallbacks (`NAMTOR_SOCKET`,
  `NAMTOR_MODELS_DIR`, `NAMTOR_STORE_DIR`, `NAMTOR_KEY_PATH`,
  `NAMTOR_WORKER_BIN`, `NAMTOR_BACKOFF_PATH`) now default to
  `LayoutPaths::resolve(...)` instead of hardcoded string literals.

### `namtor-pam` cross-check

- Added `namtor-layout` dev-dependency.
- New test `default_socket_matches_layout_contract` asserts
  `DEFAULT_SOCKET == namtor_layout::PAM_DEFAULT_SOCKET`.
- The PAM module itself keeps its `DEFAULT_SOCKET` const (cannot depend on
  `namtor-layout` at runtime — must stay < 500 KB with no TOML/serde deps).
  The cross-check test proves the const stays in sync with the layout
  contract.

### `packaging/install-manifest.toml` changes

Added entries for files previously installed manually by distro packaging:

| File | Dest | Mode |
|---|---|---|
| `packaging/pam-install.sh` | `/usr/share/namtor/pam-install.sh` | 0755 |
| `packaging/pam-uninstall.sh` | `/usr/share/namtor/pam-uninstall.sh` | 0755 |
| `packaging/pam-preflight.sh` | `/usr/share/namtor/pam-preflight.sh` | 0755 |
| `packaging/README.md` | `/usr/share/doc/namtor/README.md` | 0644 |

Added directory: `/usr/share/doc/namtor` (0755, root:root).

### Distro packaging reconciliation

**PKGBUILD (Arch):** Removed manual `install -Dm755` / `install -Dm644`
commands from `package()` — `stage.sh` now installs everything via the
manifest.

**namtor.spec (Fedora):**
- Removed manual `install` commands from `%install` — `stage.sh` handles it.
- Added missing entries to `%files`:
  - `/usr/share/dbus-1/system-services/org.namtor.Manager.service`
  - `/usr/share/polkit-1/actions/org.namtor.policy`
  - `/usr/share/applications/org.namtor.kcm.desktop` (KDE)
  - `/usr/share/metainfo/org.namtor.kcm.metainfo.xml` (KDE)
  - `/usr/share/icons/hicolor/scalable/apps/org.namtor.kcm.svg` (KDE)

**debian/rules:** Removed manual `install` commands from
`override_dh_auto_install` — `stage.sh` handles it.

### GNOME / KDE conflict reconciliation

The manifest already had both GNOME (Task 20) and KDE (Task 25) entries
without conflicts — they install to different paths
(`org.namtor.Wizard.*` vs `org.namtor.kcm.*`). The Fedora spec was missing
the KDE entries; now reconciled. The KDE QML module (`libnamtorqml.so`) and
`metadata.json` are installed by CMake's `install()` rules, not the manifest
— this is documented in the manifest comments and is correct (CMake handles
the Qt6 QML import dir).

## Remaining hardcoded paths (by design)

| Path | Location | Reason |
|---|---|---|
| `DEFAULT_SOCKET = "/run/namtor/auth.sock"` | `namtor-pam/src/client.rs` | PAM module must stay < 500 KB, no TOML/serde deps. Cross-checked by test. |
| `/usr/share/namtor/quirks/devices` | `namtor-camera/src/inventory.rs` `Default` impl | Low-level crate; `Default` is a convenience, callers pass explicit dirs. Matches layout's `quirks_dirs[0]`. |
| `/usr/share/namtor/quirks/devices/3277-0018.toml` | `namtor-camera-emitter-off.rs` | Last-resort fallback for the reference device; env var `NAMTOR_QUIRKS` overrides. |
| `/usr/lib/namtor/namtor-worker` | `namtor-worker-spawn.rs` test | Test fixture, not production code. Production takes `--worker <path>` as a CLI arg. |
| Fallback defaults in `namtor-layout` | `install_layout.rs` `parse_dests` | Single location; used only when the manifest is unreadable. |

## Tests

### `namtor-layout` (10 tests)

| Test | Verifies |
|---|---|
| `all_layout_paths_match_manifest_or_declared_constants` | Every `LayoutPaths` field matches the manifest or a declared runtime constant |
| `manifest_declares_all_expected_installed_files` | All expected file dests are present in the manifest (binaries, PAM, systemd, dbus, polkit, GNOME, KDE, manifests, PAM scripts, README) |
| `manifest_declares_all_expected_directories` | All expected dir paths are present |
| `pam_default_socket_matches_layout` | `PAM_DEFAULT_SOCKET` const matches `layout.daemon_socket` |
| `static_guard_rejects_reference_constants_in_defaults` | No reference-hardware constants in fallback defaults |
| `layout_source_root_uses_target_release_binaries` | Source root maps to `target/release/` |
| `layout_staged_root_rebases_paths_under_destdir` | DESTDIR rebasing works for all paths |
| `layout_alternate_libexec_follows_manifest` | Alternate `dest` (e.g. `/usr/libexec/`) is followed |
| `parse_all_dests_returns_none_for_unreadable_manifest` | Graceful fallback |
| `parse_all_dests_parses_files_and_dirs` | `ManifestDests` correctly parses files + dirs |

### `namtor-pam` (1 new test)

| Test | Verifies |
|---|---|
| `default_socket_matches_layout_contract` | `DEFAULT_SOCKET` const == `namtor_layout::PAM_DEFAULT_SOCKET` |

### Existing `namtor-cli` diagnostics tests

All existing diagnostics tests (including `static_guard_rejects_reference_constants_in_defaults`,
`layout_staged_root_rebases_paths_under_destdir`, `layout_source_root_uses_target_release_binaries`,
`layout_alternate_libexec_follows_manifest`, `doctor_reports_absent_daemon_in_destdir_fixture`,
`from_layout_carries_all_layout_paths`) continue to pass via the
`pub use namtor_layout as install_layout` re-export.

## Verification commands

```sh
cargo test -p namtor-layout
cargo test --workspace -- --test-threads=1
cargo clippy --workspace -- -D warnings
```

## Architecture

```
packaging/install-manifest.toml  (source of truth: installed file paths)
              │
              ▼
    namtor-layout (parses manifest, derives paths)
       │          │
       │          ├── namtor-daemon (env var fallbacks)
       │          └── namtor-cli (clap defaults, diagnostics, enroll)
       │
       └── PAM_DEFAULT_SOCKET const ←── namtor-pam (cross-check test)
                                       (PAM can't depend on namtor-layout
                                        at runtime — size/dep constraint)
```
