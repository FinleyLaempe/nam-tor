# Concurrency Flake Evidence (Task 49)

## Summary

The `concurrent_connections_serialise_on_camera_lock` test deadlocked
under the parallel test executor (>60s hang). Root cause: the test
joined the accept threads before closing the client sockets, so
`accept_one()` blocked forever in `decode()` waiting for the next
request on a connection the test was still holding open.

## Root cause

`accept_one()` loops over requests on a single connection until the
client closes (EOF). The test:

1. Spawned two threads, each calling `accept_one()`.
2. Connected two clients, sent Verify on each, read both replies.
3. Called `t1.join()` / `t2.join()` **while s1 and s2 were still open**.
4. `s1` / `s2` dropped at end of function scope — after the joins.

The accept threads were blocked in `decode(&mut reader)` waiting for
the next request. Since the test held the sockets open, no EOF arrived,
and `join()` waited forever.

## Fix

Drop `s1` and `s2` before joining the threads. This sends EOF to the
accept threads, causing `decode` to return `UnexpectedEof`, breaking
the loop, and `accept_one()` returns. The join then completes.

```rust
drop(s1);
drop(s2);
// ... assert elapsed ...
t1.join().expect("t1").expect("accept1");
t2.join().expect("t2").expect("accept2");
```

## Verification

### Before fix

```
cargo test -p namtor-daemon --lib -- --test-threads=4
→ listener::tests::concurrent_connections_serialise_on_camera_lock
  has been running for over 60 seconds (HANG)
```

### After fix

- Single run (test-threads=1): passes in 0.40s
- Single run (test-threads=4): passes in 0.41s
- 100 repeated runs (test-threads=4): 100/100 pass, no hang, no leaked thread/socket
- Full daemon lib suite WITHOUT `--skip concurrent_connections_serialise`:
  165 passed, 0 failed, 1 ignored
- Full daemon e2e suite: 9 passed
- Full PAM suite: 35 passed

### Pre-existing parallel flakiness (not this task)

`acl::tests::resolve_gid_missing_group_errors` and
`licence::tests::no_record_no_env_errors` occasionally fail under
parallel execution due to shared `/tmp` / env-var state. They pass
single-threaded and are unrelated to the camera-lock test. These are
pre-existing and outside this task's scope.

## Test commands

```bash
# No skip flag needed
cargo test -p namtor-daemon --lib -- --test-threads=1
cargo test -p namtor-daemon --lib -- --test-threads=4
```

All green.
