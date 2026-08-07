<!-- SPDX-License-Identifier: GPL-3.0-only -->
# Manager Bus Runtime Evidence (Task 9)

**Date:** 2026-08-08
**Commit:** (this commit)
**Author:** sisyphus-jr
**Spec ref:** §9.1 (D-Bus manager service), plan task 9

## Summary

The `org.namtor.Manager` D-Bus interface is served on the **system bus** at
object path `/org/namtor/Manager` by `namtor-daemon`. The service starts in
parallel with the PAM Unix socket listener — the PAM path is never blocked by
D-Bus. If the bus is unavailable or the well-known name is already owned, the
daemon logs a warning and continues serving PAM (PAM is critical, D-Bus is
management convenience).

## Implementation

### Files

| File | Role |
|---|---|
| `crates/namtor-daemon/src/manager/bus.rs` | zbus interface wrapper (`ManagerIface`), `ManagerBusService` (start/shutdown), p2p + session test helpers, 8 tests |
| `crates/namtor-daemon/src/manager/mod.rs` | `pub mod bus;` export |
| `crates/namtor-daemon/src/dbus.rs` | `ManagerDBusError` enum (zbus `DBusError` derive), `Type` derives on wire structs, `ProfileApi` trait |
| `crates/namtor-daemon/src/manager/contract.rs` | Frozen wire contract (Task 8 — unchanged) |
| `crates/namtor-daemon/src/bin/namtor-daemon.rs` | Daemon binary: starts bus service, installs SIGTERM/SIGINT handlers, accept loop with shutdown check |
| `crates/namtor-daemon/Cargo.toml` | `zbus = { version = "5.18", features = ["p2p"] }`, nix `signal` feature |
| `packaging/dbus-1/org.namtor.Manager.service` | D-Bus system-bus service activation file |
| `packaging/install-manifest.toml` | Install path for the service file (`/usr/share/dbus-1/system-services/`) |

### Lifecycle

1. **Startup:** `run()` creates `Arc<ManagerService<SystemPolkit>>`, calls
   `ManagerBusService::start_system()`. On success, the bus service handle is
   held through the accept loop. On failure (`NameTaken` or bus unavailable),
   the daemon logs `warn!` and continues — `start_manager_bus()` returns
   `None`.

2. **Serving:** The PAM listener loops on `accept_one()`. The bus service runs
   on a zbus internal executor thread; it does not interact with the accept
   loop. The two paths are fully independent.

3. **SIGTERM/SIGINT:** `install_signal_handlers()` installs a `SigAction` for
   `SIGTERM` and `SIGINT` that sets a static `AtomicBool`. The blocking
   `accept()` syscall returns `EINTR` when the signal arrives. The accept loop
   checks the flag and breaks. `bus_service.shutdown()` releases the bus name
   and drops the connection. The function returns `Ok(())` → exit code 0.

4. **Duplicate owner:** `allow_name_replacements(false)` is set on the zbus
   builder. zbus 5.x's default `RequestNameFlags` include `AllowReplacement |
   ReplaceExisting | DoNotQueue`. Without disabling `AllowReplacement`, a
   second instance would replace the first. With it disabled, the second
   instance gets `RequestNameReply::Exists` → `zbus::Error::NameTaken` →
   `BusServiceError::NameTaken`. The daemon logs the warning and exits without
   disrupting the existing owner.

### Privacy

No biometric material crosses the D-Bus surface. The frozen contract
(`contract::INTROSPECTION_XML`) is privacy-scanned by
`privacy_scan_rejects_biometric_tokens` (Task 8). The wire types are
privacy-scanned by `no_biometrics_in_api_structs` (Task 8). The `ManagerIface`
wrapper delegates to `ManagerService<SystemPolkit>` which returns only
`ProfileInfo`, `EnrollmentProgress`, `ReviewOutcome`, and `StatusReport` —
none of which carry embeddings, scores, thresholds, templates, or frames.

## Test Results

### p2p tests (no external bus needed)

| Test | Result |
|---|---|
| `p2p_introspection_matches_frozen_contract` | PASS — live introspection declares all 11 frozen methods, 4 signals, and the `ApiVersion` property |
| `p2p_introspection_contains_manager_interface` | PASS — live XML contains `org.namtor.Manager` interface name and key method/signal names |

p2p tests use `zbus::connection::socket::Channel::pair()` with
`authenticated_socket` to skip SASL handshake (avoids the deadlock where the
server's `build()` blocks waiting for the client's auth before the client is
created).

### Session-bus tests (require `DBUS_SESSION_BUS_ADDRESS`)

| Test | Result | CI behavior |
|---|---|---|
| `duplicate_owner_gets_name_taken` | PASS (dev machine with session bus) | Skips cleanly in headless CI (no `DBUS_SESSION_BUS_ADDRESS`) |
| `shutdown_releases_name_and_drops_cleanly` | PASS (dev machine with session bus) | Skips cleanly in headless CI |

The session-bus tests are guarded by `session_bus_available()` which checks
`DBUS_SESSION_BUS_ADDRESS`. Without it, the test prints a skip message and
returns. With it, the test connects to the real session bus. The inner
`if first.is_err() { return; }` guard handles the case where the env var is
set but the bus is unreachable.

### Non-bus tests

| Test | Result |
|---|---|
| `api_version_property_is_frozen` | PASS — `ApiVersion` property returns `contract::API_VERSION` (1) |
| `api_error_maps_to_dbus_error_variants` | PASS — all 7 `ApiError` variants map to `ManagerDBusError` variants |

### Full daemon lib suite

```
cargo test -p namtor-daemon --lib -- --test-threads=1
test result: ok. 191 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out
```

## WITHHELD: installed `gdbus introspect --system`

The installed system-bus introspection check (`gdbus introspect --system
--dest org.namtor.Manager --object-path /org/namtor/Manager`) is **WITHHELD**
— it requires root to install the systemd unit, create the `namtor` user, and
start the daemon on the system bus. This is owner-gated (like the Task 7 HIL
script). The p2p introspection tests above prove the same property (live
introspection matches the frozen contract) without requiring installation.

## D-Bus service activation file

`packaging/dbus-1/org.namtor.Manager.service` is installed to
`/usr/share/dbus-1/system-services/org.namtor.Manager.service`. When a D-Bus
client calls a method on `org.namtor.Manager` and no process owns the name,
`dbus-daemon` starts `namtor-daemon` via this file. Under normal operation,
systemd has already started the daemon via `namtor-daemon.service`, so this
file is a fallback for direct bus activation (e.g. debugging without systemd).

The file declares `User=namtor` so dbus-daemon starts the daemon as the
`namtor` user, matching the systemd unit's `User=namtor`.

## zbus 5.x findings

1. **Default `RequestNameFlags`** in zbus 5.18 are `AllowReplacement |
   ReplaceExisting | DoNotQueue` (set via `#[bitflags(default = ...)]` on
   `RequestNameFlags`). Without `allow_name_replacements(false)`, a second
   daemon instance replaces the first — the opposite of the desired
   duplicate-owner-rejects behavior.

2. **`Builder::name()` just stores the name** — the actual `RequestName` call
   happens in `Builder::build()`. The `NameTaken` error comes from `build()`,
   not `name()`. The error mapping must be on `build()`.

3. **p2p `unix_stream` deadlocks** if the server's `build()` is called before
   the client exists — the server blocks on the SASL handshake. Use
   `authenticated_socket` (skips SASL) with `Channel::pair()` instead.
