# RC Package Lifecycle Matrix

SPDX-License-Identifier: GPL-3.0-only

Status: **WITHHELD** (matrix defined; no live RC packages built yet)

This document defines the release-candidate package lifecycle matrix that
the release workflow (`.github/workflows/release.yml`) executes at tag time.
It records the matrix definition and test protocol. Live results land as
`rc-package-matrix-<tag>-results.json` alongside this file when a `v*` tag
triggers the release pipeline.

The matrix covers the full lifecycle a Nam-tor package goes through: install,
model provision, daemon bring-up, optional PAM enablement, password fallback
preservation, upgrade migration, failure rollback, and uninstall/reinstall.
Every cell starts `pending` and only flips to `pass` when the release workflow
records measured evidence for that exact distro and arch.

## Matrix definition

10 cells: five distros times two arches. Distros and arches match the
package definitions in [`package-matrix.json`](package-matrix.json) and the
`packaging/` definitions referenced there.

| # | Distro | Arch | Package format | Definition |
|---|--------|------|----------------|------------|
| 1 | arch | x86_64 | pkg.tar.zst | `packaging/arch/PKGBUILD` |
| 2 | arch | aarch64 | pkg.tar.zst | `packaging/arch/PKGBUILD` |
| 3 | fedora40 | x86_64 | rpm | `packaging/fedora/namtor.spec` |
| 4 | fedora40 | aarch64 | rpm | `packaging/fedora/namtor.spec` |
| 5 | fedora41 | x86_64 | rpm | `packaging/fedora/namtor.spec` |
| 6 | fedora41 | aarch64 | rpm | `packaging/fedora/namtor.spec` |
| 7 | debian12 | x86_64 | deb | `packaging/debian/control` |
| 8 | debian12 | aarch64 | deb | `packaging/debian/control` |
| 9 | debian13 | x86_64 | deb | `packaging/debian/control` |
| 10 | debian13 | aarch64 | deb | `packaging/debian/control` |

aarch64 builds run via QEMU on an x86_64 runner, matching the build tool
notes in `package-matrix.json`.

## Per-cell lifecycle gates

Each cell records nine lifecycle steps. A cell's `result` is `pass` only when
every step records `pass` for that distro and arch.

| Step | What it checks |
|------|----------------|
| `install` | Package installs cleanly with the distro's native tool (`pacman -U` / `dnf install` / `apt-get install`). No missing dependencies, no file conflicts. |
| `model_provision` | `onnxruntime` is installable in the target distro and `ctypes.CDLL('libonnxruntime.so.1')` loads. Models are not packaged; they fetch at runtime per `/usr/share/namtor/model-manifest.toml` after licence acceptance (`NAMTOR_ACCEPT_LICENCE=1`). This gate proves the model runtime exists, not that weights are present. |
| `daemon_socket` | `systemd-analyze verify namtor-daemon.service` passes, `namtor-daemon --version` runs, and `namtor-cli status` reports `stream: closed` after socket-activated start. Confirms the daemon comes up cold and the PAM socket is reachable. |
| `pam_enable` | Running `pam-install.sh` against a test PAM file inserts the marked `# namtor-begin` / `# namtor-end` block. Optional in the lifecycle: the package never invokes the PAM scripts automatically. The gate proves the script works on that distro's PAM layout, not that the admin has enabled it. |
| `password_fallback` | After `pam_enable`, a password-fallback auth line (`pam_unix` / `pam_systemd_home` / `common-auth` / `system-auth` / `password-auth`) remains below the namtor block. The installer refuses unsafe ordering that would break this (spec §8.3). |
| `upgrade_migration` | Reinstalling the package (`pacman -U` / `dnf reinstall` / `apt-get install --reinstall`) produces a byte-identical tree and re-running `pam-install.sh` is idempotent: the old block is replaced in place, original backup preserved. |
| `failure_rollback` | Simulate a failed install (interrupt after backup, broken module, missing dependency). `pam-uninstall.sh --restore-backup` restores the checksum-verified pre-namtor state. No partial edits left behind. |
| `uninstall_reinstall` | Full removal (`pacman -Rns` / `dnf remove` / `apt-get purge`) leaves no files behind, then a fresh install brings the system back to a working state. Confirms the manifest's declared file set is complete. |
| `result` | Aggregate: `pass` only when all eight steps above pass. Otherwise `fail` with the failing step named. |

## Test protocol

The release workflow runs each cell through this ordered sequence. A failure
at any step halts that cell and records the failing step; remaining steps
stay `pending`.

1. **Install** the freshly built package into a clean container for the
   target distro and arch. Record the native install command's exit status
   and any dependency resolution.
2. **Model provision** — install `onnxruntime` through the distro's package
   manager and load `libonnxruntime.so.1` via `ctypes`. This is the model
   runtime gate from `package-matrix.json`; it blocks publication if it
   fails.
3. **Daemon/socket** — run `systemd-analyze verify namtor-daemon.service`,
   start the socket-activated unit, and call `namtor-cli status`. Expect
   `stream: closed` on a cold camera (spec §5.5).
4. **Optional PAM enable** — run `pam-install.sh` against a copy of the
   distro's default PAM file under `DESTDIR`. The package ships the scripts
   under `/usr/share/namtor` but never invokes them; this step proves they
   work on the distro's PAM layout.
5. **Password fallback** — verify a fallback auth line survives below the
   namtor block. The installer's preflight refuses if it would be lost, so
   this gate double-checks the preflight result rather than trusting it.
6. **Upgrade/migration** — reinstall the package and re-run `pam-install.sh`.
   The tree must be byte-identical to the first install (determinism proof
   from `install-manifest.json`), and the PAM block must be replaced in
   place with the original backup preserved.
7. **Failure rollback** — break the install mid-transaction (interrupt after
   backup, plant a broken `pam_namtor.so`, drop a dependency). Run
   `pam-uninstall.sh --restore-backup`. The PAM file must match its
   checksum-verified backup; no partial namtor edits may remain.
8. **Uninstall/reinstall** — remove the package fully and confirm no files
   are left behind (the manifest declares every installable path). Reinstall
   from scratch and confirm the system returns to a working state.

## Recovery console test

The recovery console test proves that a broken Nam-tor install cannot lock
the user out of their machine. Password fallback is the guarantee: the PAM
block uses `auth sufficient pam_namtor.so`, so if the module returns
`PAM_AUTHINFO_UNAVAIL` (daemon down, timeout, any error), PAM falls through
to password auth (spec §8.3). The transactional PAM scripts preserve this
invariant by construction:

- The installer never accepts `required` / `requisite` / `[default=bad]` /
  `[default=die]` control fields, which would turn a daemon outage into a
  lockout.
- The installer refuses insertion after `pam_deny.so` or with `pam_deny.so`
  between the block and the fallback.
- The uninstaller removes only the marked block and verifies password login
  is intact afterward.

The recovery console test records `tested: false` and `result: "pending"`
here because no live RC packages exist yet. At tag time the release workflow
runs `pam-uninstall.sh --restore-backup` against a deliberately broken
install and confirms the system returns to password-only auth without
manual intervention.

See [`pam-install-matrix.md`](pam-install-matrix.md) for the full
transactional PAM safety contract and the 37 assertions across 8 scenarios
that the PAM scripts pass in CI without root.

## WITHHELD status

No live RC packages have been built. Every cell in
[`rc-package-matrix.json`](rc-package-matrix.json) records `result: "pending"`
and the recovery console records `tested: false`. This file defines the
matrix and protocol; it does not claim a pass.

The package definitions (`packaging/arch/PKGBUILD`,
`packaging/fedora/namtor.spec`, `packaging/debian/control`), the release
workflow (`.github/workflows/release.yml`), and the matrix schema are
committed and wired into the pipeline. Live results are produced at tag time
by the release workflow's evidence job, which writes
`rc-package-matrix-<tag>-results.json` alongside this file with the per-cell
pass/fail recorded against the steps above.

A `pass` here means the cell is wired into the release pipeline. A `pass` in
the tag-time results file means the cell actually ran green on a real
container for that distro and arch. The two are deliberately separate.
