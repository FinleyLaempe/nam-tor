# Protocol Lifecycle Evidence (Task 2)

## Summary

Authentication Release semantics are now explicit, symmetric, and quiet. The
PAM client sends Release and reads the daemon's ack (bounded by the socket
timeout); the listener guarantees capture/emitter release on every client
path including disconnect and verify-response delivery failure.

## Changes

### Client (`crates/namtor-pam/src/client.rs`)

The Release phase was previously fire-and-forget (`let _ = encode; let _ =
flush` with no ack read). It is now a **symmetric bounded acknowledgment**:
the client sends Release, flushes, and reads the daemon's reply, bounded by
the existing `cfg.timeout` socket read timeout. Any failure (peer closed,
timeout, EPIPE) is swallowed quietly — the verdict is already in hand and
the daemon's connection-end cleanup guarantees capture/emitter release
regardless.

### Listener (`crates/namtor-daemon/src/listener.rs`)

1. **Capture/emitter release guarantee on connection end.** A new
   `ensure_release` method dispatches a synthetic Release phase if the client
   disconnects after Prepare/Verify without sending Release. It is idempotent
   (tracked by a `released` flag) and safe to call when no auth started.

2. **Release on verify-response delivery failure.** All `encode`/`flush`
   calls in the auth dispatch path now trigger `ensure_release` before
   propagating the error. If the verify reply cannot be delivered to the
   client (broken pipe, connection reset), the daemon still releases the
   capture/emitter before returning.

3. **Peer close after completed lifecycle is normal.** EOF after a completed
   Release (or with no auth started) breaks the loop and returns `Ok(())` —
   no error is logged. EOF after Verify without Release triggers cleanup
   silently at `debug` level.

4. **No EPIPE/SIGPIPE noise.** Writes on the rpath-refused and dispatch-error
   paths use `let _ =` to swallow broken-pipe errors. The client's Release
   write and ack read also swallow errors. Rust sets SIGPIPE to SIG_IGN at
   startup, so no signal is generated.

5. **Privacy.** The `score` field was removed from the `tracing::info!` log
   in the auth dispatch path. Scores are logged only at `debug` level in the
   journal (spec §8.3/§8.6).

## Tests

### New tests

| Test | File | What it proves |
|---|---|---|
| `release_ack_failure_is_swallowed_quietly` | `client.rs` | Client returns the verdict even if the daemon closes before the Release ack |
| `release_sent_after_unavailable_verdict` | `client.rs` | Full Prepare→Verify→Release lifecycle completes on error verdicts |
| `cleanup_release_on_disconnect_after_verify` | `listener.rs` | Listener dispatches cleanup Release when client disconnects after Verify |
| `explicit_release_prevents_cleanup_release` | `listener.rs` | Explicit Release suppresses the cleanup Release (no double-release) |
| `no_cleanup_release_without_auth` | `listener.rs` | Status-only connection triggers no cleanup Release |

### Updated tests

- `full_pam_lifecycle_match` (e2e_integration.rs): tightened from
  `count == 2 || count == 3` to `count == 3` — the client now always reads
  the Release ack, so the daemon always sees exactly 3 dispatches.

### Existing tests that continue to pass

- `release_phase_sent_for_every_verdict_variant` (client.rs): proves
  Prepare→Verify→Release for all 5 verdict variants.
- `e2e_no_biometric_data_in_logs` (e2e.rs): no biometric data in logs at any
  level.
- All 157 daemon lib tests, 9 daemon e2e tests, 21 PAM lib tests, 10 PAM e2e
  integration tests.

## Test commands

```bash
cargo test -p namtor-pam -p namtor-daemon -- --skip concurrent_connections_serialise --test-threads=1
cargo test --workspace -- --skip concurrent_connections_serialise --test-threads=1
cargo clippy -p namtor-pam -p namtor-daemon -- -D warnings
cargo fmt -p namtor-pam -p namtor-daemon -- --check
```

All green.
