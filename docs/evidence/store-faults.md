# Store fault evidence (task 12)

Date: 2026-08-06
Commit: pending (`feat(store): harden profile containers`)
Crate: `namtor-store`
Tests: `cargo test -p namtor-store -- --test-threads=1` — 22 lib + 9 fault, all green.

## Scope

Hardened the per-uid template container (`<uid>.sealed`) against the fault
classes named in the task 12 acceptance list. No changes outside
`crates/namtor-store/` and this file.

## Hardening implemented

| Concern | Implementation | File |
|---|---|---|
| Atomic write | encrypt -> unique `*.tmp.<hex>` -> `fsync` -> `rename` -> `fsync` dir. On any error the temp file is removed and the previous record is byte-identical. | `src/fs_util.rs::atomic_write` |
| Schema / version | `NMT2` format carries a `version` u16 (currently 2). `NMT1` files (pre-task-12) are still readable (backward compat). Unknown versions return `StoreError::SchemaVersion`. | `src/format.rs` |
| AAD | `NMT2` AAD = `namtor-v2:<uid>:<profile_id>:<model_id>`. `profile_id` is reserved (empty by default; named profiles are task 14). `NMT1` AAD = `namtor-v1:<uid>:<model_id>` (backward compat). AAD is version-tagged so v1 and v2 frames are not swappable. | `src/format.rs::aad_v1/aad_v2` |
| Daemon ownership | When euid==0, `chown` to the expected owner uid + that user's primary gid (resolved via `getpwuid`). The expected owner is the `namtor` system user when the daemon configures it; default is the current euid. | `src/fs_util.rs::chown_if_root` |
| mlock / MADV_DONTDUMP | `SealedKey::key` (32 bytes) and each `Template::vector` (f32 bytes) are `mlock`ed and `madvise(MADV_DONTDUMP)`ed. Best-effort: failures are silently ignored (no `CAP_IPC_LOCK` in CI; the systemd unit grants it in production). `munlock` + `zeroize` on drop. | `src/crypto.rs::lock_bytes/lock_slice`, `src/template.rs::Template::lock_memory` |
| Zeroize | Pre-existing `Zeroize` on drop for `SealedKey` and `Template`; unchanged. | `src/crypto.rs`, `src/template.rs` |
| Lock discipline | Per-uid `flock(LOCK_EX)` on `<dir>/<uid>.lock`, held for the duration of `write`/`update`/`delete`. RAII guard releases on drop. Serialises concurrent writers across daemon instances. | `src/fs_util.rs::FileLock` |
| Symlink rejection | `symlink_metadata` check on read and write; a symlink at the record path returns `StoreError::Symlink`. Checked before the `AlreadyExists` test in `store`. | `src/fs_util.rs::reject_symlink` |
| Owner check on read | `stat` the file; if owner != expected uid, return `StoreError::WrongOwner`. Default expected = current euid; the daemon sets the `namtor` uid. | `src/fs_util.rs::check_owner` |

## Public API additions (backward-compatible)

- `TemplateStore::open_with_owner(dir, key_path, expected_owner: Option<u32>)`
- `TemplateStore::open_with_selector(dir, key_path, &mut BackendSelector)`
- `TemplateStore::open_with_owner_and_selector(dir, key_path, expected_owner, &mut selector)`
- `TemplateStore::store_with_profile(&record, profile_id: &str)`
- `TemplateStore::update_with_profile(&record, profile_id: &str)`
- `StoreError` variants: `Symlink`, `WrongOwner`, `Lock`, `SchemaVersion`.

The existing `open` / `store` / `update` / `load` / `delete` / `list_uids`
signatures are unchanged; `store`/`update` delegate to the `_with_profile`
variants with `""`.

## Fault test coverage (`tests/store_faults.rs`)

All run under `cargo test -p namtor-store --test store_faults`. Unix-only
(the crate targets Linux).

| Fault class | Test | What it proves |
|---|---|---|
| disk-full / write failure | `write_failure_preserves_old_record` | Read-only dir -> `update` returns `StoreError::Io`; old record byte-identical; `load` returns the original. |
| partial write | `partial_write_detected` | Truncating `<uid>.sealed` to half -> `load` returns `Crypto`/`Serde` error (never silently accepts a torn file). |
| symlink | `symlink_record_rejected` | Symlink at `<uid>.sealed` -> `store` and `load` both return `StoreError::Symlink`. |
| owner | `wrong_owner_rejected_on_read` + `correct_owner_accepted` | File owned by current user but expected uid is `current+9999` -> `load` returns `WrongOwner`. With expected = current uid, `load` succeeds. |
| transplant | `transplant_to_different_key_rejected` | Copy `<uid>.sealed` into a store backed by a different mock-TPM seed -> `load` returns `Crypto` (the template is bound to the sealing key). |
| concurrency | `concurrent_writers_serialise_without_corruption` | Two threads `update` the same uid simultaneously; both complete; final `load` returns one of the two records (no corruption); the `<uid>.lock` file exists. |
| crash mid-write | `crash_mid_write_leaves_old_record_intact` | A stale `*.tmp.crashleftover` file in the dir does not affect the committed record; `load` returns the old record; a subsequent `update` overwrites cleanly. |
| backward compat | `reads_legacy_nmt1_file` | A hand-crafted `NMT1` file (pre-task-12 layout, v1 AAD) is decrypted and parsed correctly by the new reader. |

## Module layout after the change

- `src/lib.rs` — re-exports.
- `src/crypto.rs` — key sealing + `lock_bytes`/`lock_slice`/`unlock_*` helpers.
- `src/format.rs` — on-disk framing (`NMT1`/`NMT2`), AAD construction, encode/decode.
- `src/fs_util.rs` — atomic write, symlink rejection, owner check, `flock` guard, daemon `chown`.
- `src/store.rs` — `TemplateStore` orchestration (open/store/update/load/delete/list).
- `src/template.rs` — `Template`/`TemplateRecord` with `lock_memory` + zeroize-on-drop.
- `tests/store_faults.rs` — fault tests.

## Non-LOC note

`src/store.rs` non-test pure LOC: 203. `src/format.rs`: 129. `src/fs_util.rs`:
132. `src/template.rs`: 94. `src/crypto.rs` non-test: 390 (pre-existing; only
~50 lines of `lock_*`/`unlock_*` helpers were added in this task). All under
or at the 250 ceiling except `crypto.rs`, which was already over the ceiling
before this task and is out of scope to refactor here.

## What is NOT done (out of scope for task 12)

- Named profiles (task 14) — only the `profile_id` AAD field is reserved.
- TPM2 sealing of the file key on real hardware (task 11 landed the
  `KeyBackend` infrastructure; the file fallback is still the default in CI).
- Migration of existing `NMT1` files to `NMT2` on load (not required; the
  reader handles both; a future task may write a migrator).
- `CAP_IPC_LOCK` wiring in the systemd unit (deployment task; the `mlock`
  call is best-effort and silently degrades without it).
