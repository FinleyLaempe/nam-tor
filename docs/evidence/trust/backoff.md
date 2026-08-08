# Persistent per-uid authentication backoff — evidence

Spec ref: §8.4 (rate limiting), G5 rate-limit gate.
Implementation: `crates/namtor-daemon/src/backoff.rs`, `crates/namtor-daemon/src/dispatch.rs`.
Commit: `feat(daemon): persistent per-uid auth backoff`.

## Design

Each failed authentication attempt (`Verdict::NoMatch`) advances an
exponential backoff schedule keyed by `target_uid`. Successful auth
(`Verdict::Match`) resets the per-uid state. The schedule is:

```
delay(n) = min(base_ms * multiplier^n, max_ms)
```

Default config (`BackoffConfig::default()`):

| parameter | value |
|---|---|
| `base_ms` | 1000 |
| `max_ms` | 300 000 (5 minutes) |
| `multiplier` | 2.0 |

So the first failure locks for 2 s, then 4 s, 8 s, 16 s, … capped at 5 min.
The cap is the **maximum lockout bound** required by the gate.

## Per-uid, not global

`BackoffStore` is a `HashMap<u32, BackoffState>`. A failure for uid A never
touches uid B's entry, and `is_locked` / `record_success` are keyed by uid.
A success on uid B does not unlock uid A (asserted by
`backoff::tests::untrusted_reset_does_not_unlock_other_uid`).

## Persistence across daemon restarts

`Instant` is monotonic and non-serialisable, so only the **attempt count**
per uid is persisted (the durable signal of an in-progress bruteforce). The
lock window (`backoff_until`) is process-local and restarts with the daemon.
This is the conservative choice: a restart shortens a current lock but never
erases the attempt history that drives the schedule.

Persistence is atomic: write to a sibling `.<name>.tmp`, `fsync`, `rename`.
A crash after the temp write but before rename leaves the old file intact.

Corruption policy:
- Missing file → `Ok` (normal first-boot state, empty store).
- File exists but unparseable → `Err` (treated as tampering; operator must
  inspect rather than silently resetting an in-progress bruteforce).

Default on-disk path: `/var/lib/namtor/backoff.json`
(overridable via `NAMTOR_BACKOFF_PATH`).

## Dispatcher wiring (`dispatch.rs`)

On `AuthPhase::Verify`, before touching the camera or worker:

1. `if backoff.is_locked(uid, now)` → return `Verdict::RateLimited` with
   reason `"backoff active"`. **A probe during lockout does NOT extend the
   schedule** — otherwise an attacker polling while locked could widen the
   window indefinitely.
2. Otherwise run the verify engine.
3. Bookkeeping (only genuine auth outcomes move the schedule):
   - `Match` → `record_success(uid)` (resets).
   - `NoMatch` → `record_failure(uid, now)` (advances).
   - `Unavailable` / `Refused` → no change (infrastructure failure must not
     lock out a user whose camera glitched or whose rhost was set).
4. If a `backoff_path` is configured, `save()` after each mutation.

## PAM integration

`namtor-pam` maps `Verdict::RateLimited → PAM_MAXTRIES` and logs the stable
journald line `pam_namtor: rate_limited → PAM_MAXTRIES` (no biometric data,
per §8.6). `PAM_MAXTRIES` is the correct PAM code for "rate-limited / max
attempts exceeded"; the PAM stack then applies its own retry behaviour.

The `VerifyRunner` trait abstracts `VerifyEngine::verify` so the dispatcher's
backoff wiring is testable with a canned outcome instead of a real ONNX
pipeline.

## Test coverage

### `backoff.rs` (unit, 12 tests)

| test | proves |
|---|---|
| `record_failure_locks` | a failure locks the uid |
| `record_success_unlocks` | a success clears the lock |
| `exponential_growth` | 2 s, 4 s, 8 s for attempts 1, 2, 3 |
| `max_cap_enforced` | delay capped at `max_ms` after 20 failures |
| `per_uid_isolation` | locking uid 1000 leaves 1001 unlocked |
| `save_load_preserves_state` | attempt count survives save+load |
| `corrupt_state_errors` | corrupt JSON → `Err` (not silent reset) |
| `missing_file_is_ok` | missing file → `Ok` (first boot) |
| `clock_rollback_still_locked` | querying earlier than `backoff_until` stays locked |
| `untrusted_reset_does_not_unlock_other_uid` | success on B does not unlock A |
| `save_leaves_no_temp` | atomic rename leaves no `.tmp` |
| `restart_continues_schedule` | loaded store continues from persisted attempts |

### `dispatch.rs` (integration, 9 tests)

| test | proves |
|---|---|
| `verify_returns_rate_limited_when_locked` | locked uid → `RateLimited`, verify engine not invoked |
| `no_match_records_failure_and_persists` | `NoMatch` records + persists `attempts == 1` to disk |
| `restart_continues_schedule` | loaded store + 1 failure → 8000 ms (attempts 3), not 2000 ms |
| `match_resets_backoff` | `Match` after lock expiry → `attempts == 0`, unlocked |
| `per_uid_isolation_in_dispatch` | 1001 failure does not change 1000's persisted attempts |
| `max_lockout_bounded` | 8 failures → in-memory remaining ≤ `max_ms` |
| `unavailable_does_not_advance_backoff` | `Unavailable` does not move the schedule |
| `prepare_dispatches_to_daemon` | backoff wiring does not interfere with Prepare |
| `prepare_advances_to_warming` | lifecycle still reaches Warming |

### `namtor-pam` (existing, unchanged)

`map_verdict(RateLimited) == PAM_MAXTRIES` is asserted in
`crates/namtor-pam/src/lib.rs` tests; the e2e test
`crates/namtor-pam/tests/e2e_integration.rs` drives a `RateLimited` verdict
through the full socket lifecycle.

## Verification commands

```
cargo test -p namtor-daemon --lib -- --test-threads=1   # 296 passed
cargo test -p namtor-pam                                 # 35 passed
cargo clippy --workspace -- -D warnings                  # clean
cargo fmt --check                                        # clean
```

## Residual risk / out of scope

- TPM2 sealing of the backoff file is not implemented (Task 26); the file is
  plain JSON owned by the `namtor` user. Root can reset it, but root can
  re-enroll anyway (see threat model).
- The lock window restarts with the daemon (only `attempts` is durable). A
  daemon restart mid-lockout shortens the current lock but the next failure
  re-derives the delay from the persisted attempt count.
- `Unavailable` does not advance backoff by design (don't lock out a user on
  infrastructure failure); an attacker who can force `Unavailable` cannot
  thereby lock a user out, but also cannot be rate-limited via this path.
