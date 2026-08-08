# Transactional PAM install lifecycle — evidence

SPDX-License-Identifier: GPL-3.0-only

Task 37: Certify transactional PAM install, upgrade and removal.

**Date:** 2026-08-08
**Commit:** `feat(packaging): transactional PAM install lifecycle`
**Verification:** `cargo test -p namtor-pam` (41 passed, 0 failed); `bash packaging/tests/test_pam_install.sh` (37 passed, 0 failed)

## Scope

Certify that the PAM module install is transactional, that upgrade preserves
existing enrollments, and that removal cleans up — all tested without root.

Three scripts form the lifecycle (added in Task 34, hardened in Task 37):

| Script | Role |
|---|---|
| `packaging/pam-preflight.sh` | Pre-install checks (root, target file, .so exists, password fallback). FATAL exits before any PAM file is touched. |
| `packaging/pam-install.sh` | Transactional install: backup + checksum → insert marked block → atomic rename → post-verify → rollback on failure. |
| `packaging/pam-uninstall.sh` | Transactional removal: remove marked block (or restore from backup) → verify password fallback intact. |

## Install flow

```
pam-preflight.sh (FATAL checks)
    ↓
make_backup (cp -a + sha256)
    ↓
trap rollback_if_needed EXIT
    ↓
remove existing namtor block (if upgrade)
    ↓
mktemp in TARGET_DIR (same-directory → atomic rename)
    ↓
write new content to temp
    ↓
mv -f temp → target (rename(2), atomic on same filesystem)
    ↓
verify password fallback after block
    ↓
verify no pam_deny between block and fallback
    ↓
trap - EXIT (success)
```

### Atomicity guarantee

The PAM file edit is atomic via `rename(2)`. The temp file is created in the
**same directory** as the target (`mktemp "${TARGET_DIR}/.namtor.pam.XXXXXX"`),
not in `/tmp`. This ensures `mv` is a same-filesystem rename — truly atomic.
A cross-filesystem `mv` (temp in `/tmp`, target in `/etc`) falls back to
copy+unlink, which is NOT atomic and can leave the PAM file half-written if
interrupted. This was hardened in Task 37.

The rollback trap (`rollback_if_needed`) fires on any non-zero exit after
`make_backup`. It restores the checksum-verified backup and cleans up any
leaked temp file (`NAMTOR_TMP`).

### What the installer never touches

- `/var/lib/namtor/templates/` — encrypted enrollment containers
- `/var/lib/namtor/models/` — ONNX model store
- `/run/namtor/` — socket directory
- Stock PAM lines (only the marked `# namtor-begin` ... `# namtor-end` block is inserted/removed)

## Upgrade flow

Re-running `pam-install.sh` on an already-installed system:

1. Detects existing `# namtor-begin` ... `# namtor-end` block.
2. Removes it (restoring the file to its pre-namtor state).
3. Verifies password fallback is intact after block removal.
4. Inserts a new block.

The result is byte-identical to the first install (no duplication, no leftover
lines). The original backup (from the first install) is preserved — not
overwritten — so the pre-namtor state is always recoverable.

**Enrollment preservation:** the install/upgrade scripts only edit the PAM
stack file. They never read, write, or delete anything under
`/var/lib/namtor/templates/`. Enrollment containers (per-uid AES-256-GCM
encrypted template stores managed by `namtor-store`) survive every install,
upgrade, and uninstall cycle unchanged. This is asserted by the Rust test
`upgrade_preserves_templates` (see below).

## Removal flow

`pam-uninstall.sh` removes only the marked block. After removal it verifies:
1. A password-fallback auth line is still present.
2. No stray `namtor` reference remains.

Options:
- `--restore-backup`: restore the file from the checksum-verified backup.
- `--purge-backup`: remove the backup files after successful uninstall.

**Templates are not removed by uninstall.** The uninstaller only edits the PAM
stack file. Enrollment data in `/var/lib/namtor/templates/` persists — a user
who uninstalls and reinstalls can still authenticate with their existing
enrollment. Full template removal is a separate `namtor-cli unenroll` operation
that requires explicit user action.

## Safety guarantees (spec §3.4, §8.3)

| Guarantee | Mechanism |
|---|---|
| Never `auth required` | `check_control()` rejects `required`, `requisite`, `mandatory`, `[default=bad]`, `[default=die]`. Only `sufficient` or `[default=ignore]` accepted. |
| Password fallback after block | Post-insertion grep for `pam_unix`/`pam_systemd_home`/`common-auth`/`system-auth`/`password-auth`. Refuses if absent. |
| No `pam_deny` before block | Pre-insertion scan of lines before insertion point. |
| No `pam_deny` between block and fallback | Post-insertion awk scan. |
| Backup + rollback | `cp -a` + sha256 before any edit; EXIT trap restores on failure. |
| Checksum-verified restore | Backup sha256 compared before restore; tampered backup refused. |
| Atomic rename | Same-directory temp + `mv -f` (rename(2)). |
| Temp cleanup on failure | `NAMTOR_TMP` tracked and cleaned by rollback trap. |

## Tests

### Rust integration tests (`crates/namtor-pam/tests/pam_lifecycle.rs`)

5 tests, run via `cargo test -p namtor-pam --test pam_lifecycle`. No root
required — all use `DESTDIR` temp roots.

| Test | Given / When / Then |
|---|---|
| `install_adds_namtor_block` | Given a fake root with an Arch-style PAM file. When `pam-install.sh` runs. Then the namtor block is inserted before the first auth line. |
| `uninstall_removes_namtor_block` | Given a fake root with the block installed. When `pam-uninstall.sh` runs. Then the block is removed and the file matches the pre-install original. |
| `upgrade_preserves_templates` | Given a fake root with enrolled templates. When install → upgrade (re-install) → uninstall runs. Then templates are byte-identical throughout. |
| `install_leaves_no_temp_files` | Given a successful install. When scanning the target PAM directory. Then no `.namtor.pam.*` temp files leak. |
| `failed_install_leaves_no_temp_files` | Given an install into a deny-first file (fails post-insertion). When the rollback trap fires. Then no temp files leak. |

### Shell test matrix (`packaging/tests/test_pam_install.sh`)

37 assertions across 8 scenarios. See [`docs/evidence/pam-install-matrix.md`](../pam-install-matrix.md)
for the full matrix.

### Full `cargo test -p namtor-pam` result

```
running 24 tests (unit, src/lib.rs)        ... ok. 24 passed
running 10 tests (e2e_integration.rs)      ... ok. 10 passed
running  2 tests (g5_structural.rs)        ... ok.  2 passed
running  5 tests (pam_lifecycle.rs)        ... ok.  5 passed

test result: ok. 41 passed; 0 failed; 0 ignored
```

## Files

| File | Change |
|---|---|
| `packaging/pam-install.sh` | Hardened: same-directory temp file for atomic `rename(2)`; temp cleanup in rollback trap. |
| `crates/namtor-pam/tests/pam_lifecycle.rs` | NEW: 6 Rust integration tests for install/uninstall/upgrade lifecycle. |
| `docs/evidence/packaging/pam-install.md` | NEW: this evidence doc. |
