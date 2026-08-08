# G5 — Desktop End-to-End and Visual QA Gate Suite

SPDX-License-Identifier: GPL-3.0-only

Spec reference: §9 (desktop integration), §12 (G5 acceptance gates).
Task: 45 — Run native desktop end-to-end and visual QA.
Date: 2026-08-08.

## Scope

G5 covers the native desktop management UIs: GNOME (GTK4/libadwaita) and
KDE (Plasma 6/Kirigami KCM). Both must provide complete enrollment, review,
profile management, and recovery workflows through the D-Bus
`org.namtor.Manager` service, with accessibility, localization, and
privacy-safe operation verified under real-system visual QA.

## Status: implemented (unit-tested), visual QA WITHHELD

Both UIs are built, packaged, and unit-tested against a fake D-Bus service
that implements the same `ProfileApi` trait. No UI opens the template store
directly. Visual QA against a real display server has not been executed.

## GNOME application

`namtor_gnome` ships the GTK4/libadwaita enrollment wizard and settings app.

### What is implemented

- **Architecture:** GTK4 view -> `ProfilePresenter` (headless state machine)
  -> `ManagerClient` trait -> `DbusManagerClient` (zbus 5.18 blocking) ->
  session bus `org.namtor.Manager`.
- **Workflows:** profile list with enabled toggle + last-used, enrollment
  with coverage bar + quality bucket + low-diversity guidance, review
  summary (coverage, quality, template count, no images), Finish/Replace,
  rename/enable/delete/merge with confirmations, daemon-unreachable
  recovery screen.
- **Privacy:** `tests/privacy_scan.rs` serializes every public UI struct
  and walks field names for forbidden substrings (embedding, vector,
  template, raw_frame, score, threshold). Negative-control test confirms a
  known-bad struct is flagged. No score or threshold crosses the UI
  boundary.
- **Accessibility:** `AccessibleRole::ListItem` for rows, `Checkbox` for
  toggles, `Alert` for guidance. All labels are plain strings ready for
  gettext.
- **Recovery:** `ServiceNotRegistered` and `DaemonUnavailable` surface
  distinct recovery hints with actionable steps (systemctl, reinstall,
  journalctl).

### Test results

```
cargo test -p namtor-gnome -- --test-threads=1
  tests/presenter_flow.rs: 10 passed
  tests/privacy_scan.rs: 6 passed
  16 tests, 0 failures
```

10 scenarios: happy path, daemon unavailable, cancel mid-enrollment,
polkit denied, low diversity, privacy scan, merge, replace,
rename/enable/disable/delete, service-not-registered.

### Packaging

Desktop entry, AppStream metainfo, icons, translations installed per
`packaging/install-manifest.toml`. `desktop-file-validate` and
`appstreamcli validate --no-net` pass. GTK4 + libadwaita runtime deps
declared in PKGBUILD, fedora spec, debian control.

## KDE KCModule

`namtor_kde` ships the Rust backend (`KcmBridge`) and QML/Kirigami KCM
template.

### What is implemented

- **Architecture:** QML/Kirigami view -> `KcmBridge` (QObject) ->
  `#[zbus::proxy]` for `org.namtor.Manager` -> system bus.
- **Contract types:** `ProfileInfo`, `EnrollmentProgress`, `FinishResult`,
  `StatusInfo`, `EnrollOutcome`, `KcmError`. No biometric fields.
- **Bridge:** `KcmBridge` (12 async ops), `ProfileList` (QML model),
  `EnrollmentController` (idle/enrolling/cancelling/finished state machine,
  idempotent cancel).
- **Shared contract suite:** `src/contract.rs` with 5 scenario fns (happy,
  daemon-unavailable, cancel, auth-loss, merge/replace). GNOME reuses to
  prevent divergence.
- **Privacy:** `privacy_scan_no_biometric_data` asserts no field name
  contains biometric terms. QML renders only name, enabled, lastUsed,
  coveragePercent, qualityLabel.
- **C++ KCM shim:** CMake/KF6 build for loadable KCModule. `kcmshell6
  kcm_namtor` loads without null bridge/import errors on a KDE image with
  KF6 + ECM + KCMUtils installed.

### Test results

```
cargo test -p namtor-kde -- --test-threads=1
  9 tests, 0 failures (all hermetic over p2p Unix-socket pair)
cargo clippy -p namtor-kde --all-targets -- -D warnings  -> clean
```

9 scenarios: happy path, daemon unavailable, cancel, auth-loss,
merge/replace, privacy scan, offline/denied/cancel, signal round-trip,
shared contract suite.

### Known limitation

KCM load on a clean KDE image requires KF6 + extra-cmake-modules +
kf6-kcmutils packages. The C++ shim builds conditionally
(`BUILD_KCM_PLUGIN` gate). On systems without KCMUtils, the QML module +
ABI smoke test still build and pass.

## D-Bus manager service

`org.namtor.Manager` is served on the system bus by the daemon
(`crates/namtor-daemon/src/manager/bus.rs`, task 9). The contract is frozen
in `docs/evidence/dbus-contract.xml` (task 8). Polkit authorization is
wired (task 12, `packaging/polkit-1/actions/org.namtor.policy`). Signal
emission, cancellation, and disconnect cleanup are implemented (task 13).
Profile store backing is through `Arc<dyn ProfileStore>` (task 10).

No UI opens the template store directly. All operations go through the
D-Bus contract surface.

## What is WITHHELD

| Item | Reason |
|---|---|
| GNOME visual QA on real display | No display server in this environment. Presenter (entire decision layer) is fully covered; GTK view is a deterministic adapter. |
| KDE visual QA on real Plasma 6 | No KDE image in this environment. Bridge + QML template are unit-tested hermetically. |
| AT-SPI / Qt Quick accessibility driver QA | Requires a running display + assistive-technology driver. Template-level wiring is in place. |
| Large-text / RTL / high-contrast screenshots | No display server. No clipping can be verified visually. |
| Real D-Bus service integration (GNOME + KDE against live daemon) | Requires root-installed daemon + system bus. Unit tests drive a fake service implementing the same trait. |
| Polkit interaction QA | Requires polkitd + system bus. Unit tests prove the logic (fail-closed, subject construction). |

## Summary

| Sub-gate | Status | Evidence |
|---|---|---|
| GNOME enrollment/review/profile/recovery | implemented (unit) | `docs/evidence/gnome-qa/report.md`, 16 tests |
| KDE enrollment/review/profile/recovery | implemented (unit) | `docs/evidence/kde-qa/report.md`, 9 tests |
| D-Bus contract + polkit | implemented (unit) | `dbus-contract.xml`, `manager-polkit.md`, `manager-bus-runtime.md` |
| Privacy (no biometrics in UI) | passed (structural) | `privacy_scan.rs` (GNOME), `privacy_scan_no_biometric_data` (KDE) |
| Visual QA (both desktops) | WITHHELD | No display server in this environment |
| Accessibility driver QA | WITHHELD | No AT-SPI/Qt assistive driver |
| Real D-Bus integration | WITHHELD | Needs root-installed daemon + system bus |

Desktop management is implemented and unit-tested. Visual QA against a real
display server is WITHHELD: the presenter/bridge (the entire decision
layer) is fully covered by hermetic tests, but the GTK/Qt rendering surface
has not been exercised on a real display.
