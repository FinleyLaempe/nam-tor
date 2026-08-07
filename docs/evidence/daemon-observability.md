# Daemon Observability Evidence (Task 4)

## Summary

The daemon initialises `tracing` once at startup and emits structured
events that truthfully reflect state. The "listening" event fires only
after bind/fd adoption succeeds; denied bind and bad-fd paths exit
without it. All events are privacy-safe: no biometric data, scores,
embeddings, gate names, or thresholds at `info+` level.

## Changes

### Tracing initialisation (`src/bin/namtor-daemon.rs`)

- `init_tracing()` called from `main()` before `run()`. Uses
  `tracing_subscriber::fmt` with `EnvFilter` (defaults to
  `info,namtor_daemon=info,namtor_pam=warn`). Called exactly once.
- All `eprintln!` calls in the binary replaced with `tracing::info!` /
  `tracing::warn!` / `tracing::error!`.

### Truthful startup state (`src/startup.rs`)

New `startup` module with `bind()` and `from_fd()` functions that wrap
`Listener::bind` / `Listener::from_fd`. The "listening" event is emitted
**only after** the bind/adoption succeeds. A denied bind or bad fd
returns `Err` without any listening event — journald never records a
false ready state.

### Stable safe events

| Event | Level | Fields | When |
|---|---|---|---|
| `namtor-daemon starting` | info | version | binary entry |
| `spawning worker` | info | worker path, mock_infer | before worker spawn |
| `opening template store` | info | store path | before store open |
| `camera enabled` / `camera disabled` | info | camera dev or "auto" | factory decision |
| `adopting socket-activation listener` | info | fd | before fd adoption |
| `listening` | info | path | **after** bind/fd success |
| `emitter ready` | info | protection level | camera open |
| `auth phase complete` | info | peer_uid, target_uid, verdict | after dispatch |
| `accept error` | warn | error | accept loop failure |
| `daemon startup failed` | error | error | binary exit failure |

### `live_capture.rs`

- `eprintln!` calls replaced with `tracing::warn!` / `tracing::info!`.
- Emitter activation failure and post-resume re-open failure use `warn`.
- Protection level logged at `info` (enum value only — no biometric data).

### Privacy

- `score` removed from the `auth phase complete` `info` event (done in
  Task 2). Score stays in the `debug`-level journal log in `dispatch.rs`
  (spec §8.3 allows it there).
- `log_auth_outcome` helper verified by
  `log_auth_outcome_carries_no_biometric_data` test: captures tracing
  fields and asserts no `score`, `0.8734`, `embedding`, `threshold`,
  `energy`, or `template` tokens.
- The existing `e2e_no_biometric_data_in_logs` test continues to pass.

## Tests

### New tests (`startup` module)

| Test | What it proves |
|---|---|
| `bind_succeeds_and_returns_listener` | Bind success returns a listener |
| `bind_fails_on_unwritable_directory` | Bind failure returns Err (no listening event) |
| `from_fd_succeeds_with_valid_listener` | fd adoption success returns a listener |
| `from_fd_fails_with_non_listener_fd` | Non-listener fd returns Err |
| `log_auth_outcome_carries_no_biometric_data` | Auth-outcome event has no score/embeddings/thresholds |

### Existing tests that continue to pass

- `e2e_no_biometric_data_in_logs` (daemon e2e): no biometric data in logs.
- All 10 listener tests, 9 daemon e2e tests, 164 daemon lib tests.

## Test commands

```bash
cargo test -p namtor-daemon --lib startup -- --skip concurrent --test-threads=1
cargo test -p namtor-daemon --test e2e -- --test-threads=1
cargo clippy -p namtor-daemon -- -D warnings
cargo fmt -p namtor-daemon -- --check
```

All green.
