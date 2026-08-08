# Manager Disconnect Cleanup — Task 13 Evidence

**Date:** 2026-08-08
**Task:** 13. Deliver signals, cancellation and disconnect cleanup
**Status:** Implemented and tested

## Scope

Strengthen the `org.namtor.Manager` service's signal sequencing, disconnect
cleanup, and cancel semantics so a vanishing D-Bus caller cannot strand
enrollment sessions, accumulate late signals, or commit after cancellation.

## Changes

### `crates/namtor-daemon/src/manager/session.rs`

- **`peer_bus_name` field added to `EnrollmentCapture`**: stores the D-Bus
  unique connection name (`:1.42`) so `handle_disconnect_by_bus_name` can
  cancel sessions when a peer vanishes from the bus. Empty for the PAM socket
  path (no bus name). Constructor updated; all callers updated.
- **`cancel()` verified idempotent**: calling it twice is a no-op —
  `zeroize_reservoir` on an empty reservoir is a no-op, `capture.take()`
  returns `None` the second time, `phase` is already `Cancelled`.
  `cancel_is_idempotent` test locks this.

### `crates/namtor-daemon/src/dbus.rs`

- **`handle_disconnect` strengthened**: now drains the signal queue
  (`std::mem::take`) before cancelling sessions. Late `EnrollmentProgress`
  signals queued during capture but not yet emitted are dropped — they do not
  accumulate forever. A `StatusChanged` is pushed if the emitter transitioned
  from armed to disarmed.
- **`handle_disconnect_by_bus_name(&self, bus_name: &str)` added**: cancels
  sessions whose `peer_bus_name` matches. Used by the bus service's
  `NameOwnerChanged` monitor. No-op for empty `bus_name` (PAM path).
- **`disconnect_sessions` private helper**: factored common logic (drain
  signals, find matching sessions, cancel, disarm, push StatusChanged) shared
  by `handle_disconnect` and `handle_disconnect_by_bus_name`.
- **Cancel idempotency** (`cancel_enrollment`): already returned `Ok(())` for
  unknown session IDs; `cancel_enrollment_twice_is_idempotent` test locks it.
- **Late commit prevention**: `commit_profile` after `cancel` returns
  `SessionNotFound` because the session was removed from the map.
  `commit_after_cancel_returns_session_not_found` test locks it.

### `crates/namtor-daemon/src/manager/bus.rs`

- **`emit_drained` added to `StartEnrollment` and `CancelEnrollment`**: both
  now take `#[zbus(signal_emitter)]` and call `emit_drained` after the service
  call. Previously, `StatusChanged` signals queued by start/cancel sat in the
  queue until the next mutation method flushed them — a real signal-delivery
  bug.
- **`monitor_disconnects` method on `ManagerBusService`**: spawns a background
  thread that subscribes to `org.freedesktop.DBus.NameOwnerChanged` via the
  blocking `DBusProxy`. When a unique name's `old_owner` is non-empty and
  `new_owner` is `None` (peer vanished), it calls
  `handle_disconnect_by_bus_name(old_owner)` and emits remaining signals
  (the `StatusChanged` for emitter disarm) via `emit_drained_blocking`.
- **`emit_drained_blocking` free function**: drains the signal queue and
  emits each signal via the blocking connection's `emit_signal` method. Used
  by the monitoring thread which does not have a `SignalEmitter`.

### `crates/namtor-daemon/src/bin/namtor-daemon.rs`

- **`start_manager_bus` returns `(ManagerBusService, Arc<Svc>)`**: so the
  daemon can pass the `Arc<Svc>` to `monitor_disconnects`.
- **`monitor_disconnects` called on startup**: best-effort; logs a warning if
  the monitor fails to start (bus unavailable). The `JoinHandle` is held
  until shutdown.

## Signal Sequencing

The full enrollment lifecycle emits signals in this order:

1. `StartEnrollment` → `StatusChanged` (emitter armed)
2. `pump_enrollment` (daemon loop) → `EnrollmentProgress` (per frame)
3. `CommitProfile` → `EnrollmentCompleted`, `ProfileChanged`, `StatusChanged`
   (emitter disarmed) — in that order.
4. `CancelEnrollment` → `StatusChanged` (emitter disarmed)
5. `RenameProfile` / `EnableProfile` / `DeleteProfile` / `MergeProfiles` /
   `ReplaceProfile` / `ReviewProfile` → `ProfileChanged`

`sequenced_signals_on_full_enrollment_lifecycle` test asserts that
`EnrollmentCompleted` precedes `ProfileChanged` in the commit drain.

## Disconnect Cleanup

When a D-Bus peer vanishes (connection closed, process killed):

1. `NameOwnerChanged` signal received by the monitor thread.
2. `handle_disconnect_by_bus_name(old_owner)` called on `ManagerService`.
3. All sessions owned by that bus name are cancelled:
   - `EnrollmentCapture::cancel()` zeroizes the reservoir, stops the capture
     source, drops the embedder, transitions to `Cancelled` phase.
   - Session removed from the `sessions` map.
   - `active_per_uid` entry removed.
4. Emitter disarmed.
5. Late signals (queued `EnrollmentProgress` from before the disconnect)
   dropped.
6. `StatusChanged` (emitter disarmed) emitted to remaining subscribers.

`kill_client_during_enrollment_bounded_teardown` test proves the teardown is
bounded: session map empties, `active_per_uid` clears, emitter disarms, and
subsequent `pump_enrollment` returns `SessionNotFound`.

## Privacy

`all_signal_types_carry_no_biometrics` test scans all 4 signal types
(`EnrollmentProgress`, `EnrollmentCompleted`, `ProfileChanged`,
`StatusChanged`) for forbidden tokens: `embedding`, `template`, `pixel`,
`score`, `0x`, `nonce`, `key`, `threshold`, `vec`. No biometric material
crosses the signal surface.

## Test Summary

| Test | What it proves |
|---|---|
| `cancel_is_idempotent` (session.rs) | `cancel()` twice is a no-op |
| `peer_bus_name_stored_and_accessible` (session.rs) | Bus name stored on session |
| `cancel_enrollment_twice_is_idempotent` (dbus.rs) | Wire-level cancel twice → `Ok(())` |
| `commit_after_cancel_returns_session_not_found` | Late commit blocked |
| `kill_client_during_enrollment_bounded_teardown` | Bounded teardown on disconnect |
| `disconnect_drops_late_progress_signals` | Late signals dropped, not queued |
| `handle_disconnect_by_bus_name_cancels_session` | Bus-name disconnect cancels |
| `handle_disconnect_by_bus_name_noop_for_empty` | Empty bus name is a no-op |
| `handle_disconnect_by_bus_name_noop_for_unknown` | Unknown bus name is a no-op |
| `all_signal_types_carry_no_biometrics` | Privacy scan on all 4 signal types |
| `sequenced_signals_on_full_enrollment_lifecycle` | Signal ordering on full lifecycle |

## Verification

```
cargo test -p namtor-daemon -- --test-threads=1  → 259 passed, 1 ignored
cargo test --workspace -- --test-threads=1       → all green
cargo clippy --workspace -- -D warnings          → clean
cargo fmt --all -- --check                       → clean
```

## WITHHELD

- Real bus-level `NameOwnerChanged` delivery requires a running system bus
  with a real client connecting and disconnecting. Unit tests prove the
  service-level logic (cancel, drain, idempotency); bus-level integration
  is WITHHELD pending a HIL environment with `dbus-daemon`.
