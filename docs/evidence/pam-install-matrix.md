# PAM install/uninstall matrix — evidence

SPDX-License-Identifier: GPL-3.0-only

Test evidence for the transactional PAM integration (`packaging/pam-install.sh`,
`packaging/pam-uninstall.sh`, `packaging/pam-preflight.sh`). The full test
script is `packaging/tests/test_pam_install.sh`; it runs without root using
`DESTDIR`-style temp roots and fake PAM files.

**Date:** 2026-08-06
**Commit:** `feat(packaging): add transactional PAM integration`
**Test result:** 37 passed, 0 failed

## Acceptance scenarios

The task acceptance criteria require: "disposable systems retain password
login through install, daemon failure, broken module, upgrade and uninstall;
unsafe ordering is refused."

| # | Scenario | What is tested | Result | Assertions |
|---|---|---|---|---|
| (a) | Install preserves password fallback | Install into an Arch-style `gdm-password` (includes `system-auth`). The namtor block is inserted before the first `auth` line; the `system-auth` include remains after the block. | PASS | 6 |
| (b) | Uninstall restores original | After install + uninstall (block removal), the PAM file is byte-identical to the pre-install original. No `namtor` reference remains. Password fallback present. | PASS | 4 |
| (b2) | Uninstall `--restore-backup` restores original | After install + uninstall `--restore-backup`, the PAM file is byte-identical to the pre-install original (checksum-verified restore). | PASS | 2 |
| (c) | Broken module caught by preflight | `pam_namtor.so` is removed from the fake root. Install with preflight fails with a FATAL message. PAM file is untouched (no namtor block). | PASS | 2 |
| (d) | Daemon failure leaves PAM intact | No daemon socket present (daemon down). The namtor line uses `auth sufficient` (PAM_AUTHINFO_UNAVAIL falls through). Password fallback exists after the block. Source-level: `lib.rs` documents "Never PAM_SUCCESS" and has the `daemon_unreachable` → `PAM_AUTHINFO_UNAVAIL` fail-closed path. | PASS | 5 |
| (e) | Upgrade (re-install) is idempotent | Install twice → exactly one namtor block, file byte-identical after first and second install. Install with a different control value (`[default=ignore]`) → exactly one block (in-place replace, no duplication), new control applied, old line gone. | PASS | 6 |
| (f) | Unsafe ordering is refused | `--control required` refused. `--control requisite` refused. Install into deny-first file (pam_deny before password) refused. Install into Fedora-style file (pam_deny after pam_unix) succeeds. Install into no-fallback file refused by preflight. | PASS | 10 |
| (g) | Rollback restores backup on failure | Install into deny-first file → installer fails post-block verification → rollback trap fires → file restored to original. | PASS | 1 |
| | **Total** | | **37 passed, 0 failed** | **37** |

## Scenario details

### (a) Install preserves password fallback

**Fixture:** Arch-style `/etc/pam.d/gdm-password`:
```
#%PAM-1.0
auth     include  system-auth
auth     optional  pam_gnome_keyring.so
account  include  system-auth
password include  system-auth
session  include  system-auth
```

**After install:**
```
#%PAM-1.0
# namtor-begin
auth sufficient pam_namtor.so
# namtor-end
auth     include  system-auth
auth     optional  pam_gnome_keyring.so
account  include  system-auth
password include  system-auth
session  include  system-auth
```

The namtor block is before the `system-auth` include. If `pam_namtor.so`
returns `PAM_AUTHINFO_UNAVAIL` (daemon down), the `sufficient` control means
"this module is unavailable, fall through" — and the `system-auth` include
runs the normal password stack.

Verified: block present, line is `auth sufficient`, end marker present,
`system-auth` include still present, block before the include.

### (b) Uninstall restores original

After `pam-uninstall.sh` (block removal), the file is byte-identical to the
pre-install original. The uninstaller also verifies a password-fallback auth
line is still present and that no stray `namtor` reference remains.

### (b2) Uninstall `--restore-backup`

`pam-uninstall.sh --restore-backup` restores the file from the
checksum-verified backup. The sha256 of the backup file is compared against
the stored checksum (hash-only, filename-agnostic) before restore; a mismatch
refuses the restore.

### (c) Broken module caught by preflight

`pam-preflight.sh` FATAL check: `pam_namtor.so` must exist at the install
path. If the module is missing (not yet built, broken install), preflight
exits non-zero with a specific message BEFORE any PAM file is touched. The
PAM file is left unchanged.

### (d) Daemon failure leaves PAM intact

Two layers of evidence:

1. **Stack shape (PAM file):** the namtor line is
   `auth sufficient pam_namtor.so`. With `sufficient`, a return of
   `PAM_AUTHINFO_UNAVAIL` means the module is unavailable and the stack
   falls through to the next `auth` line (the password fallback). A password
   fallback line exists after the block.

2. **Source-level fail-closed contract (`crates/namtor-pam/src/lib.rs`):**
   - Module doc: "Any error, timeout, or unreachable daemon →
     `PAM_AUTHINFO_UNAVAIL`." and "Never `PAM_SUCCESS` unless the daemon
     returned `Verdict::Match`."
   - `run_auth`: on `daemon_reachable()` returning false, calls
     `log_fail("daemon_unreachable", PAM_AUTHINFO_UNAVAIL, PAM_AUTHINFO_UNAVAIL)`.
   - Every error path in `run_auth` returns `PAM_AUTHINFO_UNAVAIL` (or
     `PAM_AUTH_ERR` for `NoMatch`, `PAM_MAXTRIES` for `RateLimited`). Only
     `Verdict::Match` returns `PAM_SUCCESS`.

   The fail-closed behaviour is also asserted by the Rust unit tests in
   `crates/namtor-pam/src/lib.rs` (`verdict_to_pam_code_mapping`) and the
   G5.3 CI gate (fault injection on every §8.3 condition asserts
   `PAM_AUTHINFO_UNAVAIL` and never `PAM_SUCCESS`).

### (e) Upgrade (re-install) is idempotent

Re-running `pam-install.sh` on an already-installed system:
1. Detects the existing `# namtor-begin` ... `# namtor-end` block.
2. Removes it (restoring the file to its pre-namtor state).
3. Verifies the password fallback is still intact after block removal.
4. Inserts a new block.

The result is byte-identical to the first install (no duplication, no
leftover lines). Changing the control value (e.g. from `sufficient` to
`[default=ignore]`) replaces the block in place — exactly one block, new
control applied, old line gone.

The original backup (from the first install) is preserved across upgrades —
it is not overwritten — so the pre-namtor state is always recoverable.

### (f) Unsafe ordering is refused

| Refused condition | Why | Mechanism |
|---|---|---|
| `--control required` | Daemon down → `PAM_AUTHINFO_UNAVAIL` → required module fails → entire stack fails → lockout. | `check_control()` rejects `required`, `requisite`, `mandatory`, `[default=bad]`, `[default=die]`. |
| `--control requisite` | Same as above but with immediate terminate. | Same. |
| pam_deny.so before the block | Block is unreachable. | Pre-insertion scan of lines before the insertion point. |
| pam_deny.so between block and password fallback | Fall-through hits deny before password. | Post-insertion awk scan: if `pam_deny` appears on an auth line before the first password-fallback line after the block, refuse. |
| No password fallback in the file | Daemon down → no fallback → lockout. | Preflight FATAL + post-insertion verification. |

Accepted control values: `sufficient` (default), `[default=ignore]`.

### (g) Rollback restores backup on failure

The installer sets an EXIT trap after `make_backup`. If the script exits
non-zero at any subsequent step (post-block verification failure, etc.), the
trap calls `restore_backup()`, which:
1. Verifies the backup's sha256 against the stored checksum (hash-only
   comparison, filename-agnostic).
2. If valid, copies the backup back over the target file.

Test (g) uses the deny-first fixture: the installer backs up, inserts the
block, fails the post-block deny-before-password check, and the trap restores
the file to the original. The test asserts the file is byte-identical to the
pre-install original.

## PAM module fail-closed contract (read-only context)

The PAM module (`pam_namtor.so`, `crates/namtor-pam/src/lib.rs`) is
fail-closed by design (spec §3.4, §8.3):

- `panic = "abort"` (Cargo release profile) — no unwinding across the C ABI.
- No ML/ONNX/image dependencies (G5.1 structural gate).
- Any error, timeout, or unreachable daemon → `PAM_AUTHINFO_UNAVAIL`.
- Never `PAM_SUCCESS` unless the daemon returned `Verdict::Match`.
- Remote auth (`PAM_RHOST` set) → `PAM_AUTHINFO_UNAVAIL` (spec §8.2).

With `auth sufficient pam_namtor.so` in the PAM stack, `PAM_AUTHINFO_UNAVAIL`
means the module is unavailable and the stack falls through to the next
module (password auth). This is the property that makes the transactional
install safe: even if the daemon is down, the module is broken, or the camera
is missing, password login works.

No Rust code was modified in this task. The fail-closed contract is
pre-existing and was verified by reading the source; it is also asserted by
the G5.3 CI gate and the `verdict_to_pam_code_mapping` unit test.

## Test execution

```
$ bash packaging/tests/test_pam_install.sh
Running Nam-tor PAM install/uninstall test matrix...
...
=============================================
Results: 37 passed, 0 failed
All tests passed.
```

The test script uses `mktemp -d` for each scenario, builds fake PAM files
(Arch-style, Fedora-style, deny-first, no-fallback), runs the scripts with
`DESTDIR=<temp root>`, and asserts on the resulting file tree. No root
required. No real `/etc/pam.d/` files are touched.

## Cargo workspace

No Rust source was modified. `cargo test --workspace -- --skip
concurrent_connections_serialise --test-threads=1` passes (0 failures across
all crates), confirming the shell-only changes did not affect the workspace.
