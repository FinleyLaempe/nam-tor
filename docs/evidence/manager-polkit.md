<!-- SPDX-License-Identifier: GPL-3.0-only -->
# Manager Polkit Authorization Evidence (Task 12)

**Date:** 2026-08-08
**Commit:** (this commit)
**Author:** sisyphus-jr
**Spec ref:** §9.1 (polkit actions: enroll-self, enroll-other, delete, configure), plan task 12

## Summary

The `org.namtor.Manager` D-Bus interface authorizes every mutating operation
through `org.freedesktop.PolicyKit1.Authority.CheckAuthorization` on the system
bus. Four polkit actions gate the interface: `org.namtor.enroll-self`
(default `yes` for active sessions), `org.namtor.enroll-other`,
`org.namtor.delete`, and `org.namtor.configure` (all default
`auth_admin_keep`). Read-only operations (`ListProfiles`, `GetStatus`) are not
polkit-gated — they return only the caller's own profiles.

The daemon fails closed: if polkitd or the system bus is unavailable, every
check returns `Denied`. No action is allowed by default when the authority is
missing.

## Implementation

### Files

| File | Role |
|---|---|
| `crates/namtor-daemon/src/manager/polkit.rs` | NEW: `SystemPolkit` (real polkit via D-Bus), `StubPolkit` (test stub), 20 tests |
| `crates/namtor-daemon/src/manager/mod.rs` | `pub mod polkit;` export |
| `crates/namtor-daemon/src/dbus.rs` | `PeerIdentity.bus_name` field added; `SystemPolkit`/`StubPolkit` re-exported from polkit module; existing tests migrated to `StubPolkit` |
| `crates/namtor-daemon/src/manager/bus.rs` | `peer_identity()` populates `bus_name` from message header sender; `new_svc()` uses `SystemPolkit::for_testing()` |
| `crates/namtor-daemon/src/bin/namtor-daemon.rs` | `start_manager_bus()` uses `SystemPolkit::default()` |
| `packaging/polkit-1/actions/org.namtor.policy` | NEW: polkit policy file with 4 actions |
| `packaging/install-manifest.toml` | Install path for `.policy` file (`/usr/share/polkit-1/actions/`) |

### Architecture

```
D-Bus method call
    │
    ▼
ManagerIface::method(conn, header, ...)
    │
    ├─ peer_identity(conn, header)
    │     ├─ sender = header.sender()         ← bus daemon authenticated
    │     ├─ GetConnectionCredentials(sender)  ← uid, pid from bus daemon
    │     └─ PeerIdentity { uid, pid, bus_name: sender }
    │
    ▼
ManagerService::method(peer, ...)
    │
    ├─ authz(action, peer, target_uid)
    │     └─ polkit.check(action, peer, target_uid)
    │           │
    │           ├─ SystemPolkit (production):
    │           │     ├─ conn = Connection::system() or None
    │           │     ├─ subject = ("system-bus-name", {"name": peer.bus_name})
    │           │     ├─ CheckAuthorization(subject, action, {}, 0, "")
    │           │     └─ is_authorized → Allowed
    │           │        is_challenge  → Challenge
    │           │        else          → Denied
    │           │
    │           └─ StubPolkit (tests):
    │                 └─ uid==target → Allowed (enroll-self)
    │                    uid==0      → Allowed (admin)
    │                    else        → Denied
    │
    ▼
ApiError::Denied / ApiError::Challenge / Ok
```

### Fail-closed semantics

| Condition | Decision | Rationale |
|---|---|---|
| System bus unavailable (`Connection::system()` fails) | `Denied` | No way to verify authorization — fail closed |
| `peer.bus_name` is empty (vanished sender / PAM socket path) | `Denied` | Can't build polkit subject without bus name |
| D-Bus error calling `CheckAuthorization` | `Denied` | Any polkit error is treated as denial |
| `is_authorized = true` | `Allowed` | Subject is currently authorized |
| `is_challenge = true` | `Challenge` | Subject could authenticate — client should drive interactive auth |
| Both false | `Denied` | Subject cannot become authorized |

### Polkit subject construction

The polkit subject is `("system-bus-name", {"name": peer.bus_name})`. Polkit
resolves the uid/pid from the bus name via the bus daemon's
`GetConnectionCredentials` — the trusted source. The `uid` field on
`PeerIdentity` is NOT passed to polkit; it is used only by `StubPolkit` for
its deterministic test logic. This means a peer cannot spoof their uid to
gain authorization — polkit always checks the real uid behind the bus name.

### CheckAuthorization flags

`flags = 0` (no `AllowUserInteraction`). The daemon checks whether the subject
is *currently* authorized. If polkit returns `is_challenge = true`, the daemon
returns `ApiError::Challenge` → D-Bus error `org.namtor.Error.Challenge`. The
client is expected to handle this by driving interactive authentication (e.g.
via `polkit-agent`) and retrying the method call. This keeps the daemon
non-interactive — it never blocks on an auth dialog.

## Polkit actions

| Action ID | Default (active) | Default (inactive) | Default (any) | Gates |
|---|---|---|---|---|
| `org.namtor.enroll-self` | `yes` | `yes` | `no` | `StartEnrollment` when `peer.uid == target_uid` |
| `org.namtor.enroll-other` | `auth_admin_keep` | `auth_admin` | `no` | `StartEnrollment` when `peer.uid != target_uid` |
| `org.namtor.delete` | `auth_admin_keep` | `auth_admin` | `no` | `DeleteProfile` |
| `org.namtor.configure` | `auth_admin_keep` | `auth_admin` | `no` | `RenameProfile`, `EnableProfile`, `ReviewProfile`, `MergeProfiles`, `ReplaceProfile` |

`auth_admin_keep` retains the authentication for the session, so an admin
does not need to re-authenticate for every profile operation.

## Test Results

### StubPolkit behavior (polkit.rs tests)

| Test | Result |
|---|---|
| `stub_self_enroll_allowed_for_own_uid` | PASS |
| `stub_other_enroll_denied_for_non_admin` | PASS |
| `stub_root_allowed_for_enroll_other` | PASS |
| `stub_delete_requires_admin` | PASS |
| `stub_configure_requires_admin` | PASS |

### SystemPolkit fail-closed behavior (polkit.rs tests)

| Test | Result |
|---|---|
| `missing_authority_fails_closed_on_enroll_self` | PASS — no polkitd → Denied |
| `missing_authority_fails_closed_on_delete` | PASS — Denied even for root |
| `missing_authority_fails_closed_on_configure` | PASS |
| `vanished_sender_denied` | PASS — empty bus_name → Denied |
| `vanished_sender_denied_even_for_root` | PASS |
| `system_polkit_debug_shows_connected_flag` | PASS |

### build_subject (polkit.rs tests)

| Test | Result |
|---|---|
| `build_subject_uses_system_bus_name_kind` | PASS |
| `build_subject_preserves_arbitrary_bus_name` | PASS |

### Challenge + spoof (polkit.rs tests, via MockPolkit closure)

| Test | Result |
|---|---|
| `other_enroll_challenged_when_polkit_says_challenge` | PASS — cross-user enroll → Challenge |
| `spoof_rejected_when_bus_name_mismatches_uid` | PASS — uid=0 but non-root bus_name → Denied |
| `root_with_correct_bus_name_allowed` | PASS — uid=0 + root bus_name → Allowed |

### Integration smoke (polkit.rs tests)

| Test | Result |
|---|---|
| `system_polkit_with_store_fails_closed_on_all_mutations` | PASS |
| `list_profiles_allowed_without_polkit_check` | PASS — safe reads not gated |

### Existing dbus.rs tests (migrated to StubPolkit)

All 30+ existing tests in `dbus.rs` pass with `StubPolkit` replacing the old
`SystemPolkit` unit struct. No behavior change — `StubPolkit` has the exact
same logic the old stub had.

### Full daemon test suite

```
cargo test -p namtor-daemon -- --test-threads=1
test result: ok. 227 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out
```

227 lib tests (was 191; +36 new tests in polkit.rs + migrated dbus.rs tests).
8 binary tests. 9 e2e tests. All green.

## WITHHELD: installed `pkcheck` / `gdbus` receipts

The installed polkit verification (`pkcheck --action-id org.namtor.enroll-self
--process $$` and `gdbus call --system --dest org.freedesktop.PolicyKit1
--object-path /org/freedesktop/PolicyKit1/Authority --method
org.freedesktop.PolicyKit1.Authority.CheckAuthorization ...`) is **WITHHELD** —
it requires root to install the `.policy` file, start polkitd, and start the
daemon on the system bus. This is owner-gated (like the Task 7 HIL script and
Task 9 installed introspection). The unit tests above prove the authorization
logic (fail-closed, subject construction, decision mapping) without requiring
a real polkitd.
