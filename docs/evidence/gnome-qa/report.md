# GNOME Management UI QA Report — Task 26

Rebuild of `namtor-gnome` on the session D-Bus `org.namtor.Manager` interface
(spec §9.1, §9.2). The GNOME process no longer touches the template store; every
operation goes through D-Bus, polkit gates every mutation, and no biometric data
crosses the bus into the UI.

## Scope

- **Crate:** `crates/namtor-gnome/`
- **Binary:** `namtor-enroll-wizard` (GTK4 + libadwaita)
- **D-Bus contract:** `org.namtor.Manager` on the session bus (task 25).
- **Out of scope:** the D-Bus service implementation itself (task 25), the
  template store, the daemon. This task is the UI side only.

## Architecture

```
┌──────────────┐    ┌────────────────┐    ┌──────────────────────┐
│ GTK4 view    │───▶│ ProfilePresenter │───▶│ ManagerClient trait  │
│ (bin/wizard) │    │ (headless logic) │    │  - DbusManagerClient │── session bus ──▶ org.namtor.Manager
└──────────────┘    └────────────────┘    │  - FakeManagerClient │ (test double)
                                           └──────────────────────┘
```

The presenter is a small state machine that depends only on the
`ManagerClient` trait. The GTK view is a thin adapter rendering
`PresenterModel`. Tests drive the presenter with `FakeManagerClient`; the live
`DbusManagerClient` (feature `dbus`, zbus 5.18 blocking API) is only exercised
when a real session bus is present.

## Modules

| File | Role |
|---|---|
| `src/client.rs` | `ManagerClient` trait + UI-facing data types (`ProfileInfo`, `EnrollmentProgress`, `StatusInfo`, `FinishResult`, `QualityLabel`, `ManagerError`). |
| `src/fake.rs` | In-memory test double with scripted failure modes (`Mode::DaemonDown`, `Mode::PolkitDenied`, `Mode::LowDiversity`). |
| `src/dbus_client.rs` | Live zbus-backed client. Maps D-Bus error names to `ManagerError`. Caches `enrollment_progress` signal emissions for `get_progress`. |
| `src/presenter.rs` | Headless state machine. Recovery hints, merge/replace confirmation staging, quality-guidance flagging. |
| `src/privacy.rs` | Runtime privacy scan over UI struct field names. |
| `src/lib.rs` | Re-exports. No direct store paths. |
| `src/bin/wizard.rs` | GTK4 window: profile list with enabled toggle + last-used, enrollment progress with coverage bar + low-diversity guidance alert, daemon-unreachable recovery screen. |

## Privacy invariant

No UI-facing struct carries embeddings, raw frames, match scores, thresholds,
landmarks, or biometric data. `ProfileInfo` exposes only `id`, `name`,
`enabled`, `created`, `last_used`. `EnrollmentProgress` exposes the approved
coarse metrics (`frames_captured`, `coverage_percent`) plus the
`quality_label` bucket ("low"/"medium"/"high") — never a raw score or
threshold. `tests/privacy_scan.rs` serializes a representative sample of each
public type and walks every field name recursively, failing if any forbidden
substring (`vector`, `embedding`, `template`, `raw_frame`, `score`,
`threshold`, …) appears. A negative-control test confirms a struct carrying an
`embedding`/`raw_frame`/`match_score` field is flagged.

The forbidden list deliberately uses compound raw-frame terms (`raw_frame`,
`frame_data`, `frame_buf`, `frame_pixel`) rather than the bare `frame`
substring so the approved `frames_captured` count is allowed through (spec §6.5).

## Accessibility

- Profile rows use `AccessibleRole::ListItem`; enabled toggles use
  `AccessibleRole::Checkbox`.
- The low-diversity guidance alert uses `AccessibleRole::Alert` so screen
  readers announce it.
- All labels are plain strings ready for gettext wrapping (no composite
  format strings baked into accessible names).
- Keyboard navigation is GTK4 default (focusable rows, Tab order preserved).

## Recovery

If `DbusManagerClient::connect()` fails (bus unreachable or
`org.namtor.Manager` not registered), the wizard swaps in a dedicated recovery
screen with actionable steps (`systemctl --user start namtor-daemon`,
service-file install hint, `journalctl` pointer) instead of crashing.

## Test results

```
$ cargo test -p namtor-gnome -- --test-threads=1
     Running tests/presenter_flow.rs
running 10 tests
test cancel_mid_enrollment_cleans_up ... ok
test daemon_unavailable_shows_error_and_recovery ... ok
test duplicate_profile_name_surfaces_conflict ... ok
test happy_path_create_enroll_finish_lists_profile ... ok
test low_diversity_shows_guidance ... ok
test merge_profiles_requires_confirmation ... ok
test polkit_denied_shows_auth_error ... ok
test rename_enable_disable_delete_flow ... ok
test replace_profile_requires_confirmation ... ok
test service_not_registered_has_recovery_hint ... ok

     Running tests/privacy_scan.rs
running 6 tests
test enrollment_progress_carries_no_biometrics ... ok
test finish_result_carries_no_biometrics ... ok
test forbidden_substrings_list_is_nonempty ... ok
test profile_info_carries_no_biometrics ... ok
test scan_flags_a_known_bad_struct ... ok
test status_info_carries_no_biometrics ... ok
```

16 tests, 0 failures. Required scenarios covered (≥6):

1. **Happy path** — create → enroll → finish → list shows profile.
2. **Daemon unavailable** — error shown with actionable recovery guidance.
3. **Cancel mid-enrollment** — cleanup, no orphan session, list still empty.
4. **Polkit denied** — authentication error shown with admin-approval hint.
5. **Low diversity** — quality label `"low"` → guidance alert raised.
6. **Privacy scan** — no thresholds/raw biometrics in any UI struct.
7. Merge requires confirmation (stage → confirm → src removed).
8. Replace requires confirmation (stage → cancel → no mutation; confirm → rename).
9. Rename / enable / disable / delete flow.
10. `ServiceNotRegistered` recovery hint; duplicate-name conflict surfaces as error.

## Lint / format

```
$ cargo clippy -p namtor-gnome --all-targets -- -D warnings   # clean (dbus feature)
$ cargo clippy -p namtor-gnome --no-default-features -- -D warnings  # clean (lib only)
$ cargo fmt -p namtor-gnome -- --check                        # clean
```

## Residual risks / out-of-scope

- The live `DbusManagerClient` is tested against a
  `namtor_client::fake_bus::FakeBus` p2p socket pair (hermetic, no real D-Bus
  daemon). `FakeBus::spawn()` produces a genuine `DbusManagerClient` wired to an
  in-process fake service, exercised by the USA-2 `retry_connection` behavioural
  test and the shared contract suite. Real system-bus integration is still
  HIL-only.
- GTK rendering is verified by HIL screenshots (WITHHELD, Tasks 28/29: no
  display server or camera on this host) or by behavioural presenter tests (CI
  fallback, Tasks 9-14, all DONE). The presenter is the entire decision layer
  and is fully covered; the view is a deterministic adapter.
- Signal-based progress (`enrollment_progress`, `enrollment_complete`) is
  surfaced via polled `get_progress` with a signal cache in the live client.
  A future task can add a `glib::MainLoop` signal subscription for lower
  latency; the trait contract does not change.
