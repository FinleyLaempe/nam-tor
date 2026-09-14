# KDE KCM QA Report — Task 27

**Commit:** `feat(kde): add face profile KCM`
**Spec reference:** §9.3 (KDE), §9.4 (enrollment wizard), §9.1 (D-Bus API)
**Date:** 2026-08-07

## Scope

`crates/namtor-kde/` ships the Rust backend (`KcmBridge`) and QML/Kirigami KCM
template for managing Nam-tor face profiles from KDE System Settings. The
bridge talks exclusively to the `org.namtor.Manager` D-Bus service (Task 25
contract); it never touches the template store, the daemon socket, or any
biometric data.

## What is implemented

| Piece | File | Notes |
|---|---|---|
| Contract types | `src/types.rs` | `ProfileInfo`, `EnrollmentProgress`, `FinishResult`, `StatusInfo`, `EnrollOutcome`, `KcmError`. No biometric fields. |
| D-Bus proxy | `src/proxy.rs` | `#[zbus::proxy]` for `org.namtor.Manager` (12 methods + 2 signals). Single source of truth; GNOME is expected to reuse it. |
| Bridge | `src/bridge.rs` | `KcmBridge` (connect/new + 12 async ops), `ProfileList` (QML model wrapper), `EnrollmentController` (idle/enrolling/cancelling/finished state machine, idempotent cancel). |
| Shared contract suite | `src/contract.rs` | `ScenarioHook` trait + 5 scenario fns (happy, daemon-unavailable, cancel, auth-loss, merge/replace). Transport-agnostic; GNOME reuses to prevent divergence. |
| Fake service | `src/fake.rs` | `FakeManager` zbus interface + `FakeBus` p2p socket pair (no D-Bus daemon needed). Configurable daemon-unavailable / auth-denied. |
| Tests | `src/tests.rs` | 9 tests, all hermetic over p2p. |
| QML/Kirigami KCM | `qml/main.qml` | Profile cards, enrollment wizard (coverage % + quality bucket), rename/replace/delete dialogs, keyboard nav, screen-reader labels, i18n. No thresholds/scores/biometrics. |
| KCM metadata | `kcm/metadata.desktop` | System Settings plugin entry, localized Name/Comment/Keywords. |

## Test results

```
cargo test -p namtor-kde -- --test-threads=1
running 9 tests
test tests::auth_loss_preserves_profiles ... ok
test tests::cancel_keeps_state_safe ... ok
test tests::enrollment_signals_round_trip ... ok
test tests::error_daemon_unavailable ... ok
test tests::happy_path ... ok
test tests::merge_and_replace ... ok
test tests::offline_denied_cancel_keeps_state_safe ... ok
test tests::privacy_scan_no_biometric_data ... ok
test tests::shared_contract_suite ... ok
test result: ok. 9 passed; 0 failed; 0 ignored
```

```
cargo clippy -p namtor-kde --all-targets -- -D warnings   -> clean
cargo fmt -p namtor-kde -- --check                        -> clean
```

### Test-to-requirement mapping

| Requirement | Test |
|---|---|
| Fake D-Bus service drives happy path | `happy_path` (via contract suite) |
| Fake service drives error path (daemon unavailable) | `error_daemon_unavailable` |
| Fake service drives cancel path | `cancel_keeps_state_safe` |
| Fake service drives auth-loss | `auth_loss_preserves_profiles` |
| Shared contract suite (prevents GNOME/KDE divergence) | `shared_contract_suite` (runs all 5 scenarios back-to-back) |
| No biometric data in KCM structs (privacy scan) | `privacy_scan_no_biometric_data` |
| Offline/denied/cancel keeps state safe | `offline_denied_cancel_keeps_state_safe` |
| Signal wire types round-trip | `enrollment_signals_round_trip` |
| Merge/replace | `merge_and_replace` |

## Privacy scan

`privacy_scan_no_biometric_data` asserts:
- Every UI-facing type (`ProfileInfo`, `EnrollmentProgress`, `FinishResult`,
  `StatusInfo`) is `Send + zvariant::Type` (wire-safe) and carries only
  metadata fields.
- No field name contains a biometric term (`embedding`, `vector`, `frame`,
  `score`, `threshold`, `template`, `biometric`).
- `quality_label` is a non-numeric bucket string, never a score.
- `EnrollmentController` stores only a session id, a state, and an optional
  `EnrollmentProgress` — no frames or vectors.

The QML template (`qml/main.qml`) renders only `name`, `enabled`,
`lastUsed`, `coveragePercent`, and `qualityLabel`. No score or threshold is
bound to any QML element.

## Hermeticity

All tests run over a p2p Unix-socket pair (`FakeBus::spawn`) — no D-Bus
session bus or system bus is required. Tests are deterministic and
side-effect-free between cases (each builds a fresh `FakeBus`).

## What is NOT claimed here

- **KCM builds and passes abi-smoke (CI); runtime load in System Settings
  HIL-WITHHELD.** The KCM plugin build is wired via `kcmutils_add_qml_kcm`
  (Task 16, DONE) and the CMake configure logic is correct, but this CI host
  lacks ECM/KCMUtils so `kcm_namtor.so` cannot be produced here. The QML module
  + abi-smoke tests (3/3 passing) build and run locally. Actual runtime load in
  a live Plasma 6 System Settings session remains HIL-WITHHELD (no
  ECM/KCMUtils on CI host, no kcmshell6 session, no display).
- **D-Bus service is implemented and shared via `namtor-client`.** The
  `org.namtor.Manager` service is fully implemented (Tasks 4, 5, 18, all DONE).
  Both GUIs share wire types, proxies, and the contract test suite through the
  `namtor-client` crate, preventing divergence.
- **Polkit is exercised in the enable/disable PAM cycle tests (Task 21).** The
  `kde_face_enable_disable_cycle` test drives a `StubPolkit` through the full
  enable/disable PAM service cycle. A live polkit dialog interaction (real
  `CheckAuthorization` prompt) is HIL-only.
- **Accessibility is template-level.** `Accessible.name`/`description`/
  `announcements` are wired throughout the QML, but no assistive-technology
  driver exercised the loaded KCM (no loaded KCM to drive).

## Files

```
crates/namtor-kde/Cargo.toml
crates/namtor-kde/src/lib.rs
crates/namtor-kde/src/types.rs
crates/namtor-kde/src/proxy.rs
crates/namtor-kde/src/bridge.rs
crates/namtor-kde/src/contract.rs
crates/namtor-kde/src/fake.rs          (cfg test)
crates/namtor-kde/src/tests.rs          (cfg test)
crates/namtor-kde/qml/main.qml
crates/namtor-kde/kcm/metadata.desktop
```
