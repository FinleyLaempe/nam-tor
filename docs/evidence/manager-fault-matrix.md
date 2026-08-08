# Manager Bus Boundary — Fault Injection Matrix

Task 14: fault-inject and fuzz the `org.namtor.Manager` D-Bus boundary.

Date: 2026-08-08
Commit: `test(manager): harden bus boundary`
Scope: `crates/namtor-daemon/src/dbus.rs` — `ManagerService` + `ProfileApi` impl.

## Wire bounds

| Bound | Limit | Rejection | Test |
|---|---|---|---|
| Profile display name | `MAX_NAME_LEN = 128` chars | `ApiError::State` | `oversized_name_rejected_on_start`, `oversized_name_rejected_on_rename`, `oversized_name_rejected_on_replace` |
| Empty name | 0 chars | `ApiError::State` | `empty_name_rejected_on_start` |
| Profile/session ID | `MAX_ID_LEN = 256` chars | `ApiError::State` | `oversized_id_rejected_on_commit` |
| Empty ID | 0 chars | `ApiError::State` | `empty_id_rejected_on_commit`, `empty_id_rejected_on_cancel`, `empty_id_rejected_on_review`, `empty_id_rejected_on_delete`, `empty_id_rejected_on_enable`, `empty_id_rejected_on_merge` |
| Concurrent sessions | `MAX_SESSIONS = 64` | `ApiError::State` | `too_many_concurrent_sessions_rejected` |
| Boundary name (exactly 128) | accepted | `Ok` | `boundary_name_length_accepted` |
| Boundary ID (exactly 256) | accepted | `Ok` (idempotent cancel) | `boundary_id_length_accepted_on_cancel` |

## State transition fuzz

| Fault class | Injection | Expected behaviour | Test |
|---|---|---|---|
| Stale session ID on pump | `pump_enrollment("sess-nonexistent")` | `SessionNotFound` | `stale_session_id_rejected_on_pump` |
| Stale session ID on commit | `commit_profile("sess-nonexistent")` | `SessionNotFound` | `stale_session_id_rejected_on_commit` |
| Rapid start/cancel cycles (50x) | 10 uids, 50 iterations | No session leak; emitter disarmed | `rapid_start_cancel_cycles_do_not_leak_sessions` |
| Rapid start/commit cycles (10x) | Same uid, 10 profiles | 10 profiles persisted; no leak | `rapid_start_commit_cycles_accumulate_profiles` |
| Concurrent list_profiles | 2 threads, 2 uids | Each sees own profile only | `concurrent_list_profiles_for_different_uids` |
| Cancel after shutdown | `shutdown()` then `cancel_enrollment` | Noop (`Ok(())`) | `cancel_after_shutdown_is_noop` |
| Pump after shutdown | `shutdown()` then `pump_enrollment` | `SessionNotFound` | `pump_after_shutdown_returns_session_not_found` |
| Cancel unknown session | `cancel_enrollment("unknown")` | `Ok(())` (idempotent) | `cancel_unknown_session_is_noop` (pre-existing) |
| Cancel twice | Start, cancel, cancel | `Ok(())` both times | `cancel_enrollment_twice_is_idempotent` (pre-existing) |
| Commit after cancel | Start, cancel, commit | `SessionNotFound` | `commit_after_cancel_returns_session_not_found` (pre-existing) |

## Fault injection

| Fault class | Injection | Expected behaviour | Test |
|---|---|---|---|
| Camera failure during pump | `FlakySource` errors after 2 frames | `ApiError::State`; emitter disarmed; session removed; store intact | `camera_failure_during_pump_tears_down_session` |
| Camera failure during commit | `FlakySource` errors immediately | `ApiError::State`; emitter disarmed; session removed; store intact | `camera_failure_during_commit_tears_down_session` |
| Embed source failure during pump | `FailingEmbedSource` always errors | `ApiError::State`; emitter disarmed; session removed; store intact | `embed_failure_during_pump_tears_down_session` |
| Store I/O error (denied delete) | Non-admin tries delete | `ApiError::Denied`; store intact | `store_io_error_preserves_session_for_retry` |
| Authorization race (polkit flip) | `MockFlippingPolkit`: allows 1, then denies | Enrollment session stays armed; store unchanged | `authorization_race_denied_mid_session_preserves_store` |
| Corrupt store (pre-existing) | Corrupt `.sealed` file | `ApiError::State` or `Io` | `corrupt_store_errors_gracefully` (pre-existing) |
| Null store commit (pre-existing) | `NullProfileStore` | `ApiError::State` | `null_store_commit_errors` (pre-existing) |

## Shutdown

| Scenario | Expected behaviour | Test |
|---|---|---|
| Shutdown during active enrollment | Sessions cancelled; emitter disarmed; no profile written | `shutdown_cancels_active_enrollment` |
| Shutdown with no sessions | Noop; emitter disarmed | `shutdown_with_no_sessions_is_noop` |
| Shutdown preserves committed profiles | Committed profile survives; in-progress session cancelled | `shutdown_preserves_committed_profiles` |

## Disconnect cleanup (pre-existing, task 13)

| Scenario | Expected behaviour | Test |
|---|---|---|
| Disconnect during enrollment | Emitter disarmed; session cancelled; no profile | `disconnect_during_enrollment_cancels_and_disarms` |
| Disconnect drops late signals | `EnrollmentProgress` dropped; `StatusChanged` survives | `disconnect_drops_late_progress_signals` |
| Disconnect by bus name | Session cancelled for matching bus name | `handle_disconnect_by_bus_name_cancels_session` |
| Disconnect by empty bus name | Noop | `handle_disconnect_by_bus_name_noop_for_empty` |
| Disconnect by unknown bus name | Noop | `handle_disconnect_by_bus_name_noop_for_unknown` |

## Implementation changes

1. **Wire bounds:** `validate_name()` / `validate_id()` reject empty and oversized strings with `ApiError::State` before any state mutation. Called at the top of every `ProfileApi` method that accepts a name or ID.

2. **Session cap:** `start_enrollment` rejects when `sessions.len() >= MAX_SESSIONS (64)`.

3. **Error-path cleanup:** `teardown_session()` is a shared helper that removes the session, cancels it, clears the active-per-uid entry, and disarms the emitter. Used by `pump_enrollment` and `commit_profile` error paths to prevent emitter/session leaks on camera/embed/store faults.

4. **`shutdown()` method:** cancels all live sessions, disarms emitter, drops queued signals. Called on daemon shutdown.

5. **`pump_enrollment` error path:** on `Err` from `s.pump()`, calls `teardown_session` before returning the error — prevents a dead session from keeping the emitter armed.

6. **`commit_profile` error path:** on `pump_until_exhausted` or `finish` error, calls `teardown_session` — same cleanup as pump.

## Invariants verified

- Every fault returns a typed `ApiError` variant (no panics).
- Every fault path closes the emitter (`emitter_armed == false` after fault).
- Every fault path removes the session from the `sessions` map.
- Every fault path clears the `active_per_uid` entry.
- Store integrity is preserved: no partial profile written on any fault path.
- `store_gen` is only incremented on successful store writes.

## Test count

- 30 new fault tests in `dbus::tests`.
- 288 total daemon lib tests pass (was 259 after task 13).
- Workspace: all green. Clippy clean. Fmt clean. SPDX clean.
