# Diagnostics Portability — Derived Production Paths

**Task:** 6 — Remove production path and hardware constants from diagnostics
**Commit:** `fix(cli): derive production diagnostics`
**Date:** 2026-08-07
**Crate:** `namtor-cli`
**Spec:** §8.5 operator diagnostics

## Problem

`DiagnosticConfig::production()` (diagnostics.rs:193-211) embedded
reference-hardware constants:

- `device_path: /dev/video2`
- `vid_pid: 3277:0018`
- `daemon_bin: /usr/libexec/namtor-daemon`
- `cli_bin: /usr/bin/namtor-cli`
- `models_dir: /var/lib/namtor/models`
- `templates_dir: /var/lib/namtor/templates`
- `key_path: /var/lib/namtor/templates.key`
- `daemon_socket: /run/namtor/auth.sock`
- `quirks_dirs: /usr/share/namtor/quirks/devices`

These encode a single machine's device numbering, a single distro's libexec
layout, and a single install root. On a renumbered camera, a `/usr/libexec/`
distro, a DESTDIR staging root, or a source checkout, the doctor reported
the wrong paths.

## Fix

Two new modules, no reference constant in the default path:

### `install_layout.rs` — install-layout contract

`LayoutPaths::resolve(root, manifest_path, InstallRoot)` parses
`packaging/install-manifest.toml` and returns concrete paths for the
daemon/cli binaries, models/store/socket directories, and quirks search
order. Three `InstallRoot` variants:

| Variant | Binary source | Runtime paths | Manifest |
|---|---|---|---|
| `Installed` | manifest `dest` (absolute) | `/var/lib/namtor/...`, `/run/namtor/...` | `/usr/share/namtor/install-manifest.toml` |
| `Staged` | `<root>/<dest>` (rebased) | `<root>/var/lib/namtor/...` | `<root>/usr/share/namtor/install-manifest.toml` |
| `Source` | `target/release/<basename>` | `<root>/var/lib/namtor/...` | `packaging/install-manifest.toml` |

The daemon and CLI `dest` paths are read from the manifest's `[[files]]`
entries — a distro that ships `namtor-daemon` under `/usr/libexec/` changes
its manifest `dest` and the doctor follows automatically. No Rust source
change needed.

`rebase_abs(root, dest)` strips the leading `/` from absolute dest paths
before joining under `root` so `PathBuf::join` doesn't discard the base —
the subtle bug that would silently break DESTDIR staging.

### `diagnostics.rs` — derived `production()`

`DiagnosticConfig::production()` now:

1. Calls `LayoutPaths::resolve("/", "/usr/share/namtor/install-manifest.toml", Installed)`.
2. Calls `discover_device(&layout.quirks_dirs)` which builds a
   `SysfsInventory` over `/sys`, enumerates IR cameras, and selects one via
   `select_camera_from_inventory` (the same path the daemon uses). The
   selected camera's `ir_node.dev_path` and `usb.vid:pid` become the
   doctor's `device_path` / `vid_pid`.
3. Returns `DiagnosticConfig::from_layout(&layout, device_path, vid_pid)`.

No reference constant appears in the default path. The fallback when sysfs
is empty or no IR camera is found is `("/dev/video0", "0000:0000")` — a
placeholder that produces an actionable E001, not a panic.

`DiagnosticConfig` remains a plain struct with public fields; tests
construct it directly via `from_layout` or field assignment. The existing
`full_ok_config` test helper is unchanged.

## Static guard

`REFERENCE_CONSTANTS` in `install_layout.rs` lists the three reference
constants that must never appear in defaults:

```rust
pub const REFERENCE_CONSTANTS: &[&str] = &[
    "/dev/video2",
    "3277:0018",
    "/usr/libexec/namtor-daemon",
];
```

`static_guard_rejects_reference_constants_in_defaults` resolves
`LayoutPaths` with an unreadable manifest (forcing the fallback dests) and
asserts none of these strings appear in the resulting paths. The test
deliberately does NOT call `production()` — on the reference dev machine
`production()` legitimately discovers `/dev/video2` and `3277:0018` from
sysfs, which is correct behaviour. The guard checks the compile-time
fallback defaults, not the runtime discovery.

## Acceptance tests (7 new, 53 total in namtor-cli)

| Test | Acceptance criterion |
|---|---|
| `static_guard_rejects_reference_constants_in_defaults` | No reference constant in fallback defaults |
| `layout_source_root_uses_target_release_binaries` | Source root → `target/release/` binaries |
| `layout_staged_root_rebases_paths_under_destdir` | DESTDIR paths rebased under root |
| `layout_alternate_libexec_follows_manifest` | Alternate libexec `dest` followed |
| `doctor_reports_absent_daemon_in_destdir_fixture` | Absent daemon in DESTDIR → W004 |
| `doctor_renumbered_device_reports_correct_path` | Renumbered device path reported |
| `from_layout_carries_all_layout_paths` | All layout paths propagated to config |

## Verification

```
cargo test -p namtor-cli -- --test-threads=1     # 53 passed
cargo clippy -p namtor-cli -- -D warnings        # clean
cargo fmt -p namtor-cli -- --check               # clean
```

## Residual risk

- `production()` reads `/usr/share/namtor/install-manifest.toml` at runtime;
  if the manifest is absent, `LayoutPaths` falls back to
  `/usr/lib/namtor/namtor-daemon` (the manifest's declared default dest, not
  a reference constant). The doctor reports the fallback path and an
  actionable error.
- `discover_device` reads `/sys` at runtime; on a machine without the
  reference camera it returns the `/dev/video0` / `0000:0000` fallback. The
  doctor reports E001 with the fallback path.
- The privacy scanner's `clean report` test string still contains
  `/dev/video2` as example output text; this tests the scanner accepts a
  clean report, not the diagnostics derivation, and is not a default.
