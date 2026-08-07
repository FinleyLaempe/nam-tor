# Manager v1 Wire Contract — Evidence

> **Status: FROZEN.** This is the single source of truth for the
> `org.namtor.Manager` D-Bus interface. Both the GNOME and KDE clients generate
> their bindings from [`dbus-contract.xml`](dbus-contract.xml). The Rust
> constant `namtor_daemon::manager::contract::INTROSPECTION_XML` is kept
> byte-identical to that file by the `contract_xml_matches_evidence_file` test.

- Spec reference: `docs/superpowers/specs/2026-08-04-nam-tor-design.md` §9.1
- Plan task: 8 (Freeze one versioned manager wire contract)
- Commit: `feat(api): freeze manager v1 contract`

## Single source of truth

| Artifact | Role |
|---|---|
| `docs/evidence/dbus-contract.xml` | Frozen introspection XML (human-readable, pretty-printed) |
| `crates/namtor-daemon/src/manager/contract.rs` | Rust constant `INTROSPECTION_XML` loaded from the XML file via `include_str!`; method/signal/property/error name modules; `API_VERSION`; `negotiate_version` |
| `crates/namtor-daemon/src/dbus.rs` | Runtime types + in-memory reference service; `introspect_xml()` delegates to the frozen contract; `ProfileApi` trait mirrors the frozen method set 1:1 |

The `contract_xml_matches_evidence_file` test fails CI if the XML file and the
Rust constant drift. There is no second copy: `dbus.rs::introspect_xml()`
returns `contract::INTROSPECTION_XML`, which is the file.

## Version

`API_VERSION = 1`, exposed as the read-only `ApiVersion` property (`u`).

`negotiate_version(client_version)` is the only version handshake. It succeeds
iff `client_version == API_VERSION`; any other version fails safely with
`IncompatibleVersion { requested, supported }` (D-Bus error
`org.namtor.Error.IncompatibleVersion`). Strict negotiation guarantees a client
is never silently served a different contract than the one it asked for. Future
v2 may introduce backward-compat; v1 does not.

## Methods (11)

| Method | In | Out | Polkit action | Semantics |
|---|---|---|---|---|
| `ListProfiles` | — | `a(ssbtt)` | — | Profiles owned by the caller only |
| `StartEnrollment` | `u` uid, `s` name | `s` session | `enroll-self` / `enroll-other` | Creates and arms a session; emits `StatusChanged` when emitter transitions to armed |
| `CancelEnrollment` | `s` session | — | — | Disarms; no profile written; no `EnrollmentCompleted` |
| `CommitProfile` | `s` session | `s` profile | — | Finalises a session into a profile; emits `EnrollmentCompleted` + `ProfileChanged` (+ `StatusChanged` on disarm) |
| `ReviewProfile` | `u` uid, `s` id | `(sbs)` review | `configure` | Coarse re-review; returns `{profile_id, quality_label, needs_reenroll}` |
| `RenameProfile` | `u` uid, `s` id, `s` name | — | `configure` | Relabel |
| `EnableProfile` | `u` uid, `s` id, `b` enabled | — | `configure` | Toggle |
| `DeleteProfile` | `u` uid, `s` id | — | `delete` | Remove |
| `MergeProfiles` | `u` uid, `s` src, `s` dst | — | `configure` | Fold `src` into `dst`, remove `src`; emits `ProfileChanged` for `dst` |
| `ReplaceProfile` | `u` uid, `s` id, `s` name | — | `configure` | Re-name in place, re-running the store write path (distinct from a pure relabel) |
| `GetStatus` | — | `(ua(ssbtt)b)` | — | Daemon status snapshot |

### Merge / replace / review semantics

- **MergeProfiles(src, dst):** the source profile is removed; its display name,
  enabled flag, and creation timestamp are copied onto the destination. The
  destination id is stable. Emits `ProfileChanged(uid, dst)`. Used to consolidate
  two enrollments of the same person into one canonical profile.
- **ReplaceProfile(id, name):** re-names an existing profile in place and
  re-runs the store write path (the store generation counter increments). This
  is distinct from `RenameProfile`, which is a pure metadata relabel with no
  store rewrite. Both take the same wire arguments; the distinction is in the
  store side-effect, which is why both exist on the wire.
- **ReviewProfile(uid, id):** a coarse re-review of an existing profile. Returns
  a `(sbs)` struct: the profile id, a three-bucket `QualityLabel`
  (`low`/`medium`/`high`), and a `needs_reenroll` boolean. Never returns a
  numeric score, threshold, or embedding. The UI uses this to prompt the user
  to re-enroll when quality has degraded, without ever surfacing the raw
  biometric reason.

## Signals (4)

| Signal | Args | When |
|---|---|---|
| `EnrollmentProgress` | `s` session, `(susu)` progress | Live enrollment update (progress = session_id, frames_captured, quality_label, coverage_percent) |
| `EnrollmentCompleted` | `s` session, `s` profile | `CommitProfile` succeeded |
| `ProfileChanged` | `u` uid, `s` id | Any profile created/renamed/enabled/deleted/merged/replaced/reviewed |
| `StatusChanged` | `(ua(ssbtt)b)` status | Daemon status changed (emitter arming, profile list) |

## Property (1)

| Property | Type | Access |
|---|---|---|
| `ApiVersion` | `u` | read |

## Errors (8)

All under `org.namtor.Error.*`:

| Name | When |
|---|---|
| `Denied` | polkit refused |
| `Challenge` | polkit requires interactive auth |
| `NotFound` | profile id unknown |
| `Exists` | duplicate profile name |
| `SessionNotFound` | enrollment session id unknown |
| `SessionBusy` | target uid already has an active session |
| `State` | daemon in a state that rejects the call |
| `IncompatibleVersion` | version negotiation failed |

## Privacy

The `privacy_scan_rejects_biometric_tokens` test asserts the contract XML
contains none of: `frame`, `embedding`, `score`, `threshold`, `template`,
`pixel`, `vec`, `key`, `nonce`, `biometric` (case-insensitive). The contract
surface carries only: names, ids, booleans, coarse quality labels, frame
**counters** (not frames), coverage **percentages** (integer 0-100, never a
fractional threshold), and timestamps.

The `dbus::tests::no_biometrics_in_api_structs` test asserts the same over the
serialized Rust types (`ProfileInfo`, `EnrollmentProgress`, `ReviewOutcome`,
`StatusReport`).

## Binding generation

Both clients generate from `docs/evidence/dbus-contract.xml`:

- **KDE:** `crates/namtor-kde/src/proxy.rs` uses `#[zbus::proxy]` with
  `assume_defaults = true`; zbus maps snake_case Rust method names to the
  PascalCase wire names in the frozen XML.
- **GNOME:** `crates/namtor-gnome/src/dbus_client.rs` calls methods by wire
  name via `zbus::Connection::call_method`.

> **Note (drift):** the GNOME/KDE clients were built against a Task 25 draft
> whose method set differed (`CreateProfile`/`GetProgress`/`FinishEnrollment`/
> `Status`). The frozen v1 contract renames and reshapes these
> (`StartEnrollment`/`CommitProfile`/`GetStatus`, progress via signal). The
> clients compile against their own `ManagerClient`/proxy traits and do not
> depend on `namtor-daemon`'s `ProfileApi`; reconciling them to the frozen
> names is their own task (the bus service, task 9, is also not implemented
> here). The frozen XML is the authority they will regenerate from.

## Out of scope (deferred)

- Bus service implementation (zbus wiring) — task 9
- Store backing for profiles — task 10
- Enrollment session capture — task 11
- Polkit wiring — task 12

## Test evidence

```
cargo test -p namtor-daemon --lib manager::contract -- --test-threads=1
  14 passed; 0 failed

cargo test -p namtor-daemon --lib dbus -- --test-threads=1
  17 passed; 0 failed

cargo test --workspace -- --test-threads=1
  all green (no regressions)

cargo clippy --workspace --all-targets -- -D warnings
  clean

cargo fmt --all -- --check
  clean

scripts/check-spdx.sh
  passed
```

Contract test inventory (14):

1. `contract_xml_matches_evidence_file` — file ≡ Rust constant
2. `contract_has_one_manager_interface` — exactly one `org.namtor.Manager`
3. `contract_defines_exactly_the_required_methods` — exact 11-method set
4. `contract_defines_exactly_the_required_signals` — exact 4-signal set
5. `contract_exposes_api_version_property` — read-only `u` `ApiVersion`
6. `method_signatures_are_stable` — per-method arg signatures
7. `merge_replace_review_semantics_present` — distinct methods + signatures
8. `signal_signatures_are_stable` — per-signal arg signatures
9. `privacy_scan_rejects_biometric_tokens` — no biometric tokens in XML
10. `negotiate_supported_version_succeeds` — v1 negotiates
11. `negotiate_higher_version_fails_safely` — v2 rejected
12. `negotiate_lower_version_fails_safely` — v0 rejected
13. `error_names_are_namespaced` — all under `org.namtor.Error.*`
14. `extract_names_helper_works` — test helper sanity
