# Enrollment storage fault evidence (task 5)

Date: 2026-08-07
Commit: `fix(store): make enrollment ownership atomic`
Crates: `namtor-store`, `namtor-cli`
Tests: `cargo test -p namtor-cli --test enrollment_storage_faults -p namtor-store -- --test-threads=1` — 10 enrollment-fault + 9 store-fault, all green. Full workspace suite green (`--skip concurrent_connections_serialise --test-threads=1`).

## Problem

`namtor-cli enroll` run as root wrote store artefacts owned by root, but the
daemon (`namtor-daemon`) runs as the `namtor` system user and opens the store
with `expected_owner = current euid = namtor`. The result: after a
root-initiated enrollment the daemon could not read what enrollment wrote.

Three artefacts were root-owned after a root enrollment:

1. The sealing key file (`/var/lib/namtor/templates.key`) — created by
   `SealedKey::load_or_create_with` with mode 0600 and never chowned.
2. The per-uid lock file (`<templates>/<uid>.lock`) — created by
   `FileLock::acquire` and never chowned.
3. The sealed record (`<templates>/<uid>.sealed`) — chowned to `namtor` only
   *after* the atomic write succeeded, inside `run_enroll`. A chown failure
   left a root-owned record on disk (partial state).

The templates directory itself was pre-created by `tmpfiles.d` as
`namtor:namtor` in production, but if enrollment created it (first run before
tmpfiles, or a test/dev path) it was left root-owned.

## Fix

Enrollment now resolves an `EnrollmentIdentity` that carries the store owner
uid, opens the store with that owner, and the store chowns every artefact at
the point it is created — not after the fact.

### `namtor-store` (`crates/namtor-store/src/fs_util.rs`, `store.rs`)

| Artefact | When chowned | Where |
|---|---|---|
| Templates directory | At `open_with_owner_and_selector`, after `create_dir_all` | `store.rs` via `ensure_dir_owner` |
| Sealing key file | At `open_with_owner_and_selector`, after `SealedKey::load_or_create_with` | `store.rs` via `chown_if_root` |
| Per-uid lock file | Inside `FileLock::acquire_with_owner`, after `flock` | `fs_util.rs` |
| Sealed record (temp file) | Inside `atomic_write`, before `rename` | `fs_util.rs` (pre-existing) |

`chown_if_root` is a no-op when euid != 0 (tests, self-enrollment); when root
it chowns to `expected_owner` + that user's primary gid (resolved via
`getpwuid`) and **hard-fails** on `chown` error rather than leaving a
root-owned artefact.

The sealed-record temp file is now created with mode 0600 explicitly
(`OpenOptions::mode(0o600)`) so the final record has the same mode regardless
of the process umask.

### `namtor-cli` (`crates/namtor-cli/src/enroll.rs`)

- `resolve_identity(user: Option<&str>)` resolves the enrolled uid, username,
  and the store owner uid in one step:
  - **Self-enrollment** (no `--user`): enrolled uid = current euid; store
    owner = current euid (or `namtor` if root, so a root self-enrollment also
    hands the store to `namtor`). Works unprivileged.
  - **Other-user enrollment** (`--user NAME`): requires root; enrolled uid =
    NAME's uid; store owner = `namtor` system user, which must exist
    (`MissingStoreOwner` error if the sysusers.d entry is not installed).
- `commit_enrollment(templates_dir, key_path, identity, record)` opens the
  store with `open_with_owner(Some(identity.store_owner_uid))` and calls
  `update`. The store handles all chowning; `commit_enrollment` does no
  post-hoc chown.
- `run_enroll` now calls `resolve_identity` (hard-fails on
  `IdentityError`), then `TemplateStore::open_with_owner`, then `update`.
  The old post-write `chown` block is gone.

### Identity errors

| Variant | Meaning | User-facing message |
|---|---|---|
| `UnknownUser(name)` | `--user` named a non-existent account | `user '<name>' not found` |
| `MissingStoreOwner` | root enrollment but `namtor` user not installed | `system user 'namtor' not found; install the namtor package (sysusers.d) before enrolling` |
| `NotRootForOtherUser` | `--user` given but process is not root | `enrolling another user requires root` |

## Fault coverage (`crates/namtor-cli/tests/enrollment_storage_faults.rs`)

All run under `cargo test -p namtor-cli --test enrollment_storage_faults`.
Unix-only. 10 tests.

| Fault class | Test | What it proves |
|---|---|---|
| missing identity (unknown user) | `unknown_target_user_fails_actionably` | `resolve_identity(Some("…xyzzy"))` returns `UnknownUser`; message names the user. |
| missing identity (not root) | `non_root_cannot_enroll_other_user` | `resolve_identity(Some("root"))` as non-root returns `NotRootForOtherUser`. |
| self-enrollment identity | `self_enrollment_identity_uses_current_euid` | No `--user`: enrolled uid and store owner both equal current euid. |
| re-enrollment | `re_enrollment_overwrites_and_daemon_reads` | A second `commit_enrollment` for the same uid overwrites the first; the daemon store (same owner) loads the updated quality. |
| disk full / write failure | `write_failure_preserves_old_enrollment_and_cleans_temps` | Read-only dir → `commit_enrollment` returns `StoreError::Io`; old record byte-identical; no `*.tmp.*` leftover. |
| symlink | `symlink_record_rejected_by_enrollment` | Symlink at `<uid>.sealed` → `commit_enrollment` returns `StoreError::Symlink`. |
| corrupt key | `corrupt_key_fails_open_actionably` | Garbage key file → `commit_enrollment` returns `Crypto`/`Io` (no panic, no silent re-key). |
| interrupted write | `interrupted_write_detected_on_daemon_read` | Truncated `<uid>.sealed` → daemon `load` returns `Crypto`/`Serde` (never silently accepts a torn file). |
| daemon reads root-initiated enrollment | `daemon_reads_enrollment_with_matching_owner` | Store opened with the same expected owner as enrollment loads the record. |
| daemon rejects mismatched owner | `daemon_rejects_enrollment_with_mismatched_owner` | Store opened with a *different* expected owner → `WrongOwner` on load. |

The chown-when-root path is a no-op in these tests (they run unprivileged).
The tests exercise the *logic* — owner selection, atomicity, symlink
rejection, corrupt-key handling, and the cross-process owner contract. Real
root behaviour (root enrolls → `namtor` daemon reads) is covered by the
owner-run HIL script in task 7; this task proves the construction is correct.

## What is NOT done (out of scope for task 5)

- TPM2 sealing of the file key on real hardware (task 11).
- The HIL script that exercises a real root→`namtor` enrollment + daemon read
  on installed hardware (task 7).
- Migration of existing root-owned artefacts from pre-task-5 installs (an
  operator re-runs enrollment as root after upgrading; new artefacts are
  `namtor`-owned).
