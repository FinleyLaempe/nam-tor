# Manager Store Integration — Task 10 Evidence

**Date:** 2026-08-08  
**Commit:** `feat(manager): persist profile lifecycle`  
**Task:** Back manager profiles with encrypted TemplateStore

## Summary

Replaced the in-memory `HashMap<(u32, String), InMemoryProfile>` in
`ManagerService` with durable transactions against the encrypted
`TemplateStore`. Profile lifecycle (commit, rename, enable, delete, merge,
replace, last-used) now persists across daemon restarts. The UI has no direct
store access — every mutation goes through the `ProfileApi` trait.

## Architecture

### ProfileStore trait (`crates/namtor-daemon/src/manager/store.rs`)

A `Send + Sync` trait abstracting the encrypted store so the manager can be
tested without holding a real `TemplateStore`:

- `list_profiles(uid)` — read all profiles for a uid
- `create_profile(uid, profile, templates)` — append, fail on duplicate
- `get_profile(uid, profile_id)` — read a single profile
- `rename_profile(uid, profile_id, new_name)` — update display name
- `enable_profile(uid, profile_id, enabled)` — toggle
- `delete_profile(uid, profile_id)` — remove
- `combine_profiles(uid, src_id, dst_id)` — fold src into dst atomically
- `merge_profile(uid, profile, templates)` — replace in place
- `touch_last_used(uid, profile_id, ts_us)` — update last-used timestamp

### Implementations

| Impl | Use | Backing |
|---|---|---|
| `TemplateProfileStore` | Production (daemon bin) | Real `Arc<TemplateStore>` (AES-256-GCM, TPM2-sealed key) |
| `NullProfileStore` | Bus-service tests (D-Bus mechanics) | No-op: reads return empty, writes return `StoreError::Lock` |

### Concurrency model

- `ManagerService` holds `Arc<dyn ProfileStore>` + `store_lock: Mutex<()>`.
- Every store operation holds `store_lock` for its duration, serializing
  concurrent D-Bus calls through the manager.
- The store's per-uid `flock` (in `FileLock::acquire_with_owner`) serializes
  cross-process access (e.g., CLI enrollment + daemon manager).
- `write_container` is atomic: encrypt to temp, `fsync`, `rename`, `fsync`
  dir. A crash mid-write leaves the previous record byte-identical.

### Error mapping (`map_store_error`)

| `StoreError` | `ApiError` | D-Bus error |
|---|---|---|
| `ProfileNotFound(id)` | `NotFound` | `org.namtor.Error.NotFound` |
| `ProfileExists(id)` | `Exists` | `org.namtor.Error.Exists` |
| `NotFound(uid)` | `NotFound` | `org.namtor.Error.NotFound` |
| `AlreadyExists(uid)` | `Exists` | `org.namtor.Error.Exists` |
| `Crypto` / `Tpm2` / `Lock` / `WrongOwner` / `Symlink` / `SchemaVersion` | `State` | `org.namtor.Error.State` |
| `Io` | `Io` → `State` | `org.namtor.Error.State` |
| `ProfileUidMismatch` | `State` | `org.namtor.Error.State` |

### Store additions (`namtor-store`)

1. **`ProfileRecord::last_used_us`** — new `#[serde(default)]` field.
   Backward-compatible: old containers deserialize with `last_used_us = 0`.
   Surfaced as `ProfileInfo.last_used` (seconds) on the D-Bus surface.

2. **`TemplateStore::combine_profiles(uid, src_id, dst_id)`** — atomic
   container-level combine: read container → find src + dst → remove src →
   append src templates to dst → single `write_container`. A missing dst
   returns `ProfileNotFound` before any mutation, leaving src intact.

3. **`TemplateStore::touch_last_used(uid, profile_id, ts_us)`** — update
   last-used timestamp for the verify path (not yet wired; task 11+).

### ManagerService changes (`dbus.rs`)

- `profiles: Mutex<HashMap<...>>` → `store: Arc<dyn ProfileStore>`
- `new(polkit)` → uses `NullProfileStore` (bus-service test compat)
- `new_with_store(polkit, store)` → production constructor
- `commit_profile` → `store.create_profile(uid, record, vec![])` (templates
  populated by task 11 enrollment capture)
- `merge_profiles` → `store.combine_profiles(uid, src, dst)`
- `replace_profile` → `store.get_profile` → update name → `store.merge_profile`
- All other methods delegate to the trait

### Daemon binary (`bin/namtor-daemon.rs`)

`start_manager_bus` now opens the real `TemplateStore` at `store_dir` +
`key_path`, wraps it in `TemplateProfileStore`, and passes it to
`new_with_store`. If the store fails to open, the bus service is not started
(PAM path continues).

## Test coverage

### New store tests (`namtor-store/src/store.rs`)

| Test | Verifies |
|---|---|
| `combine_folds_src_into_dst` | src templates appended to dst, src removed |
| `combine_missing_dst_preserves_src` | missing dst → error, src intact (rollback) |
| `combine_missing_src_errors` | missing src → error |
| `combine_same_id_errors` | self-merge rejected |
| `touch_last_used_updates_field` | field written and read back |
| `touch_last_used_missing_profile_errors` | missing profile → error |
| `last_used_defaults_to_zero_on_deserialize` | backward compat with old containers |

### New store-adapter tests (`namtor-daemon/src/manager/store.rs`)

| Test | Verifies |
|---|---|
| `template_store_adapter_round_trip` | create + list |
| `template_store_adapter_combine` | combine via adapter |
| `null_store_returns_empty_and_errors` | null store behavior |
| `map_store_error_variants` | error mapping covers all variants |

### New manager integration tests (`namtor-daemon/src/dbus.rs`)

| Test | Verifies |
|---|---|
| `restart_persistence_across_manager_instances` | Profile survives drop+recreate of ManagerService with same store |
| `duplicate_profile_id_rejected` | `ProfileExists` error on duplicate |
| `wrong_uid_isolated` | uid 2000 sees no profiles from uid 1000 |
| `corrupt_store_errors_gracefully` | Corrupted sealed file → `State` error, no panic |
| `concurrent_mutation_serialized` | 20 concurrent rename+enable calls, no corruption |
| `merge_rollback_on_missing_dst` | Missing dst → error, src preserved |
| `merge_missing_src_errors` | Missing src → error |
| `rename_persists_across_restart` | Rename survives restart |
| `enable_disable_persists_across_restart` | Enable/disable survives restart |
| `delete_persists_across_restart` | Delete survives restart |
| `merge_persists_across_restart` | Merge (src removed, dst remains) survives restart |
| `replace_persists_across_restart` | Replace survives restart |
| `null_store_list_returns_empty` | Null store compat |
| `null_store_commit_errors` | Null store commit fails gracefully |

### Existing tests preserved

All 17 pre-existing dbus tests (self_enroll, delete_requires_admin,
disconnect_mid_enrollment, no_biometrics_in_api_structs, status,
commit_emits_signals, mutations_emit_profile_changed, merge, cancel,
review, round_trip, introspection, api_error, quality_label) pass
unchanged against the new store-backed implementation.

## Verification

```
cargo test --workspace -- --test-threads=1   → all green (500+ tests)
cargo clippy --workspace -- -D warnings       → clean
cargo fmt --all -- --check                    → clean
```

## Privacy invariants

- No biometric material (embedding, template, score, threshold) crosses the
  `ProfileStore` trait or the D-Bus surface.
- `ProfileInfo` carries only: id, name, enabled, created, last_used.
- The `no_biometrics_in_api_structs` test still passes.
- The `privacy_scan_rejects_biometric_tokens` contract test still passes.
- `commit_profile` writes empty templates (task 11 will populate them); the
  profile record carries only metadata (name, uid, model_id, timestamps).

## Out of scope (per task constraints)

- Enrollment capture sessions (task 11) — `commit_profile` persists the
  profile record with empty templates; template population is task 11.
- Polkit wiring (task 12) — `SystemPolkit` stub remains.
- Bus service logic (task 9) — `bus.rs` is unchanged; `NullProfileStore`
  keeps bus-service tests compiling without a real store.
- Frozen contract (task 8) — `contract.rs` is unchanged.
