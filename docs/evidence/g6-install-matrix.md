# G6 — Install Matrix Gate Suite

SPDX-License-Identifier: GPL-3.0-only

Spec reference: §12 (G6 acceptance gates), §7 (installation), §8 (PAM integration).
Task: 46 — Run clean-install, upgrade, rollback, and uninstall matrix.
Date: 2026-08-08.

## Scope

G6 covers the full package lifecycle across the declared distro/arch
matrix: clean install, model provision, daemon socket activation, optional
PAM enablement, password fallback preservation, upgrade migration, failure
rollback, and uninstall/reinstall.

## Status: Arch package built and tested; Fedora/Debian WITHHELD

The Arch package definition (`packaging/arch/PKGBUILD`) is committed and
builds cleanly. Fedora and Debian package definitions are committed but
have not been built and tested on live containers. The PAM transactional
install lifecycle is tested in CI without root (37 assertions, 8 scenarios).

## Package matrix

10 cells: five distros times two arches. Defined in
`docs/evidence/rc-package-matrix.md` and `package-matrix.json`.

| # | Distro | Arch | Format | Build status |
|---|--------|------|--------|---|
| 1 | arch | x86_64 | pkg.tar.zst | **built** |
| 2 | arch | aarch64 | pkg.tar.zst | WITHHELD (no aarch64 builder) |
| 3 | fedora40 | x86_64 | rpm | WITHHELD (no live container) |
| 4 | fedora40 | aarch64 | rpm | WITHHELD |
| 5 | fedora41 | x86_64 | rpm | WITHHELD |
| 6 | fedora41 | aarch64 | rpm | WITHHELD |
| 7 | debian12 | x86_64 | deb | WITHHELD |
| 8 | debian12 | aarch64 | deb | WITHHELD |
| 9 | debian13 | x86_64 | deb | WITHHELD |
| 10 | debian13 | aarch64 | deb | WITHHELD |

The release workflow (`.github/workflows/release.yml`) is defined and
committed. It builds the full matrix at tag time (`v*` tag push) and
records per-cell results as `rc-package-matrix-<tag>-results.json`. It has
not been executed against a published tag.

## PAM transactional lifecycle

`packaging/pam-install.sh`, `packaging/pam-uninstall.sh`,
`packaging/pam-preflight.sh` are tested by `packaging/tests/test_pam_install.sh`
without root using DESTDIR-style temp roots. Full evidence in
`docs/evidence/pam-install-matrix.md`.

**Result: 37 passed, 0 failed across 8 scenarios.**

| Scenario | What is tested | Result |
|---|---|---|
| (a) Install preserves password fallback | Arch-style gdm-password; namtor block before system-auth include | PASS |
| (b) Uninstall restores original | Byte-identical to pre-install original | PASS |
| (b2) Uninstall --restore-backup | Checksum-verified restore | PASS |
| (c) Broken module caught by preflight | Missing pam_namtor.so -> FATAL, file untouched | PASS |
| (d) Daemon failure leaves PAM intact | `auth sufficient` falls through to password | PASS |
| (e) Upgrade (re-install) idempotent | Exactly one block, byte-identical, in-place replace | PASS |
| (f) Unsafe ordering refused | required/requisite denied, pam_deny before block denied | PASS |
| (g) Rollback restores backup on failure | EXIT trap fires, file restored | PASS |

Key safety invariants:

- The installer never accepts `required` / `requisite` / `[default=bad]` /
  `[default=die]` control fields (would turn daemon outage into lockout).
- The installer refuses insertion after `pam_deny.so` or with `pam_deny.so`
  between the block and the fallback.
- The uninstaller removes only the marked block and verifies password login
  is intact afterward.
- `auth sufficient pam_namtor.so` means `PAM_AUTHINFO_UNAVAIL` falls
  through to password. This is the lockout-prevention guarantee.

## Install layout

`packaging/install-manifest.toml` defines the authoritative install layout
(task 34). Logical paths and distro mappings for binaries, PAM, models,
quirks, D-Bus, polkit, and both UIs are generated from one manifest.
`docs/evidence/install-manifest.json` records the deterministic empty-DESTDIR
staging tree. Upgrade avoids Text-file-busy via atomic binary replacement.

## Systemd hardening

`packaging/systemd/namtor-daemon.service` (task 36):

- `namtor` system user, `namtor-auth` group.
- Minimal privileges: read/write paths limited to model/TPM/camera/D-Bus.
- `TimeoutStopSec=2` with `ExecStopPost` binary that forces emitter off
  even after SIGKILL.
- Socket activation: `namtor-daemon.socket` for the PAM Unix socket.

`systemd-analyze verify` passes (committed in the unit). Negative
permission tests (absent groups/TPM/camera) fail actionably. Owner-gated:
live `systemd-analyze` on an installed system is WITHHELD.

## Model provenance

Model acquisition and licensing (task 35): models fetch at runtime per
`/usr/share/namtor/model-manifest.toml` after licence acceptance
(`NAMTOR_ACCEPT_LICENCE=1`). Tamper/truncation/tensor/network/licence
failures leave no partial model. `docs/evidence/trust/` records the
provenance chain. Offline cache + atomic ownership/rollback.

## What is WITHHELD

| Item | Reason |
|---|---|
| Fedora 40/41 package build + lifecycle | No live Fedora container in this environment |
| Debian 12/13 package build + lifecycle | No live Debian container |
| aarch64 builds (all distros) | No aarch64 builder (QEMU cross-build defined but not executed) |
| Live RC package lifecycle (install -> enroll -> verify -> upgrade -> rollback -> uninstall) | Needs root + reference hardware + containers |
| Signed packages | WITHHELD: no signing keys |
| `systemd-analyze verify` on installed system | Needs root install |

## Summary

| Sub-gate | Status | Evidence |
|---|---|---|
| Arch x86_64 package build | built | `packaging/arch/PKGBUILD` |
| Fedora/Debian packages | WITHHELD | Definitions committed; no live containers |
| aarch64 builds | WITHHELD | No aarch64 builder |
| PAM transactional lifecycle | passed (37/37 CI) | `pam-install-matrix.md` |
| Install layout determinism | passed | `install-manifest.json` |
| Systemd hardening | implemented | `namtor-daemon.service`; live verify WITHHELD |
| Model provenance | implemented | `docs/evidence/trust/` |
| Signed packages | WITHHELD | No signing keys |

The Arch package builds and the PAM lifecycle is proven in CI. Fedora,
Debian, aarch64, and signed packages are WITHHELD pending live containers,
an aarch64 builder, and signing keys.
