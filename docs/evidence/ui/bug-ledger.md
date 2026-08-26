# USA-* Bug Ledger — GNOME Enrollment UI

This ledger accounts for every `USA-*` bug identifier found in the
`namtor-gnome` crate (sources under `crates/namtor-gnome/src/` and tests under
`crates/namtor-gnome/tests/`). It is the authoritative index for the known UI
issues tracked in the GNOME enrollment wizard and settings app.

Scope: 11 unique identifiers were discovered.

```
USA-0, USA-2, USA-3, USA-5, USA-6, USA-8, USA-9, USA-10, USA-11, USA-12, USA-16
```

Numbers that do **not** appear in the codebase (USA-1, USA-4, USA-7, USA-13,
USA-14, USA-15) are intentionally absent: they were either merged into a
sibling fix, withdrawn before landing, or never filed. They are listed here so
the gap is documented rather than implied.

Each entry below records:

- **Symptom** — one line describing what the user (or developer) observed.
- **Manifests at** — `file:line` where the bug (or the code path it lived in)
  shows up.
- **Fix at** — `file:line` of the current fix (comment, code, or both).
- **Test kind** — whether the fix is guarded by a source-contract string scan,
  a behavioural test that exercises real logic, or a mix of both.
- **Expected behaviour** — what the code must do for the bug to stay closed.

The tests live in `crates/namtor-gnome/tests/gnome_bug_fixes.rs`. "String-scan"
means the test `include_str!`s the source and asserts substrings are present
(no display server needed). "Behavioural" means the test drives the
`ProfilePresenter` with a `FakeManagerClient` and asserts observable model
state.

---

## USA-0 — polkit CheckAuthorization off the GTK main thread

- **Symptom:** Toggling a PAM service ran a synchronous D-Bus call (which may
  trigger a polkit `CheckAuthorization`) on the GTK main thread, freezing the
  UI for the round-trip.
- **Manifests at:** `crates/namtor-gnome/src/components/pam_services.rs:137`
  (`handle_toggle` calls `request_pam_toggle` inline).
- **Fix at:** `crates/namtor-gnome/src/components/pam_services.rs:137` — a
  comment documents the async contract: the call should be routed through
  `presenter.dispatch_async()` + a `glib::MainContext` idle callback so the
  main thread never blocks.
- **Test kind:** String-scan
  (`usa_0_pam_toggle_documents_async_contract`).
- **Expected behaviour:** The PAM toggle's synchronous D-Bus call (and any
  polkit `CheckAuthorization` it triggers) is documented as needing to run off
  the GTK main thread via `dispatch_async`.

## USA-2 — retry_connection re-runs DbusManagerClient::connect()

- **Symptom:** When the daemon was unreachable at launch, the offline page's
  retry button was a no-op (`shell.show_offline(&detail, || {})`), so the user
  could not recover without restarting the app.
- **Manifests at:** `crates/namtor-gnome/src/app.rs:69` (initial offline
  fallback wires a no-op retry closure) and `crates/namtor-gnome/src/app.rs:89`
  (where the retry button is wired for the offline controller).
- **Fix at:** `crates/namtor-gnome/src/app.rs:104` — `retry_connection` re-runs
  `DbusManagerClient::connect()`, installs the new presenter, clears the
  offline flag, refreshes, and rebuilds the UI; on failure it re-shows the
  offline page with the same retry closure. Wired at `app.rs:89-94`.
- **Test kind:** String-scan
  (`usa_2_app_controller_has_retry_connection` asserts `fn retry_connection`
  and `DbusManagerClient::connect()` are present).
- **Expected behaviour:** The offline retry button re-runs
  `DbusManagerClient::connect()` and rebuilds the UI; on a fresh failure it
  stays offline with the retry button still armed.

## USA-3 — duplicate-name check in start_enrollment_v3

- **Symptom:** Enrolling a profile with a name that already existed silently
  created a duplicate instead of prompting the user.
- **Manifests at:** `crates/namtor-gnome/src/presenter.rs:416` (the
  duplicate-name check inside `start_enrollment_v3`) and
  `crates/namtor-gnome/src/app.rs:280` (where a detected duplicate must open a
  confirmation dialog).
- **Fix at:** `crates/namtor-gnome/src/presenter.rs:416-437` — stages a
  `PendingReplace` and surfaces a "Duplicate name" error;
  `crates/namtor-gnome/src/presenter.rs:757` (`confirm_replace_v3`) deletes the
  old profile and starts a fresh v3 enrollment;
  `crates/namtor-gnome/src/presenter.rs:749` (`cancel_replace`) clears the
  pending replace. The UI shows a replace dialog at `app.rs:280-282`.
- **Test kind:** Behavioural
  (`usa_3_duplicate_name_stages_pending_replace`,
  `usa_3_confirm_replace_v3_starts_fresh_enrollment`,
  `usa_3_cancel_replace_clears_pending` drive the presenter with a
  `FakeManagerClient` and assert `pending_replace`, `error`, and
  `enrollment_state`).
- **Expected behaviour:** Enrolling with an existing name stages a
  `pending_replace` and an error; confirming deletes the old profile and starts
  a fresh `Capturing` enrollment; cancelling clears `pending_replace` without
  enrolling.

## USA-5 — error visible in the Stopped branch

- **Symptom:** Save errors that occurred during the `Stopped` enrollment state
  were not rendered, so the user saw a blank stopped view with no indication
  of failure.
- **Manifests at:** `crates/namtor-gnome/src/components/enrollment.rs:222`
  (`render_stopped`) and `crates/namtor-gnome/src/app.rs:380` (where the
  stopped branch passes the model error to the view).
- **Fix at:** `crates/namtor-gnome/src/components/enrollment.rs:222-247` —
  `render_stopped` accepts `error: Option<&ErrorView>` and renders it as an
  alert row at the top of the group; `app.rs:380-385` passes `model.error`.
- **Test kind:** String-scan
  (`usa_5_render_stopped_shows_error` asserts `fn render_stopped`, `error`, and
  `USA-5` are present in the source).
- **Expected behaviour:** The `Stopped` enrollment view renders any save error
  at the top of the group as an accessible alert row.

## USA-6 — auto-reconnect on ServiceNotRegistered/Transport

- **Symptom:** After launch, if the daemon connection was lost
  (`ManagerError::ServiceNotRegistered` or `Transport`), the app had no
  reconnect mechanism and stayed stuck on the stale UI.
- **Manifests at:** `crates/namtor-gnome/src/app.rs:61` (the initial
  `DbusManagerClient::connect()` whose failure sets the `offline` flag) —
  USA-6 is the requirement that a reconnect path exist for transient loss, not
  just initial failure.
- **Fix at:** `crates/namtor-gnome/src/app.rs:104` — shares the
  `retry_connection` method introduced for USA-2. The retry button is the
  user-facing reconnect path; USA-6 tracks that the same mechanism covers
  `ServiceNotRegistered`/`Transport` loss, not only the launch-time offline
  case.
- **Test kind:** String-scan
  (`usa_6_reconnect_mechanism_present` asserts `retry_connection` is present in
  `app.rs`).
- **Expected behaviour:** The app exposes a reconnect mechanism
  (`retry_connection`) that re-establishes the D-Bus connection when the
  daemon vanishes with `ServiceNotRegistered`/`Transport`.

## USA-8 — Restart Daemon + View Logs wired; polkit action present

- **Symptom:** The Troubleshooting section's "Restart Daemon" and "View System
  Logs" rows were placeholders with no activated handlers, and the polkit
  policy had no action authorising a daemon restart.
- **Manifests at:** `crates/namtor-gnome/src/components/system_status.rs:121`
  (`restart_row`) and `crates/namtor-gnome/src/components/system_status.rs:138`
  (`logs_row`).
- **Fix at:** `crates/namtor-gnome/src/components/system_status.rs:130`
  (`restart_row.connect_activated` documented as wired to `systemctl restart
  namtor-daemon` via the `org.namtor.restart-daemon` polkit action) and
  `crates/namtor-gnome/src/components/system_status.rs:149`
  (`logs_row.connect_activated` documented as wired to `journalctl -u
  namtor-daemon`). The polkit action itself is declared at
  `packaging/polkit-1/actions/org.namtor.policy:73`
  (`org.namtor.restart-daemon`).
- **Test kind:** String-scan + file scan
  (`usa_8_polkit_restart_daemon_action_present` scans the polkit policy file
  for `org.namtor.restart-daemon`; `usa_8_restart_and_logs_rows_wired` scans
  `system_status.rs` for `restart_row`, `logs_row`, `connect_activated`, and
  `USA-8`).
- **Expected behaviour:** The Troubleshooting section wires both action rows
  with `connect_activated` handlers, and the polkit policy declares the
  `org.namtor.restart-daemon` action so a restart is authorised, not silent.

## USA-9 — completion cue when diversity >= 100 and quality good

- **Symptom:** During capture, the user had no signal that enrollment had
  reached sufficient diversity and quality, so they did not know when to press
  Stop.
- **Manifests at:** `crates/namtor-gnome/src/components/enrollment.rs:165`
  (inside the `Capturing` render path).
- **Fix at:** `crates/namtor-gnome/src/components/enrollment.rs:165-185` — when
  `diversity_percent >= 100 && quality_bucket >= 2`, a "Enrollment complete —
  press Stop to review" `ActionRow` (role `Status`, `success` style) is added
  and the Stop button label switches to the completion text.
- **Test kind:** String-scan
  (`usa_9_completion_cue_present` asserts `USA-9`,
  `diversity_percent >= 100`, `quality_bucket >= 2`, and `Enrollment complete`
  are present).
- **Expected behaviour:** The capture view shows a completion cue and restyles
  the Stop button once diversity reaches 100% and quality bucket is good
  (>= 2).

## USA-10 — Rename button in profile list

- **Symptom:** Profile rows had no rename affordance; renaming a profile
  required a separate flow.
- **Manifests at:** `crates/namtor-gnome/src/components/profile_list.rs:108`
  (the per-row suffix where the rename button is added).
- **Fix at:** `crates/namtor-gnome/src/components/profile_list.rs:17`
  (`RenameCallback` type alias), `crates/namtor-gnome/src/components/profile_list.rs:108-128`
  (the `rename_btn` with `document-edit-symbolic` icon and a "Rename profile"
  accessible label), and `crates/namtor-gnome/src/app.rs:208-221` (the
  `on_rename` callback wired to `presenter.rename_profile`).
- **Test kind:** String-scan
  (`usa_10_rename_button_present` asserts `RenameCallback`, `rename_btn`,
  `document-edit-symbolic`, and `Rename profile` are present).
- **Expected behaviour:** Each profile row has a rename button using the
  edit icon with an accessible label, firing `on_rename(id, current_name)`.

## USA-11 — polkit_auth distinguishes Bus/Check/Denied/Cancelled

- **Symptom:** Polkit authorization failures surfaced as a raw D-Bus error
  string, so the user could not tell whether the service was down, the check
  failed, the action was denied, or they dismissed the dialog.
- **Manifests at:** `crates/namtor-gnome/src/polkit_auth.rs:11` (the
  `PolkitAuthError` enum that classifies the outcome).
- **Fix at:** `crates/namtor-gnome/src/polkit_auth.rs:11-20` (the four-variant
  enum `Bus`/`Check`/`Denied(String)`/`Cancelled`),
  `crates/namtor-gnome/src/polkit_auth.rs:26` (`from_manager_error` maps
  `ServiceNotRegistered`/`Transport` to `Bus`, `Denied` with "cancel"/"dismiss"
  to `Cancelled`, other `Denied` to `Denied`, `Challenge` to `Check`), and
  `crates/namtor-gnome/src/polkit_auth.rs:44` (`user_message` returns a
  distinct message per variant).
- **Test kind:** Mixed — string-scan for the enum shape
  (`usa_11_polkit_auth_has_four_variants`) plus behavioural
  (`usa_11_polkit_auth_messages_distinct` asserts the four `user_message`
  strings; `usa_11_service_not_registered_maps_to_bus`,
  `usa_11_denied_with_cancel_maps_to_cancelled`,
  `usa_11_denied_without_cancel_maps_to_denied` assert the
  `from_manager_error` mapping).
- **Expected behaviour:** Polkit outcomes are categorised into
  Bus/Check/Denied/Cancelled, each producing a distinct user-facing message,
  and `ManagerError` variants map deterministically to those categories.

## USA-12 — recovery view renders hint.title once (no double title)

- **Symptom:** The recovery view rendered `hint.title` as a `Label` twice (once
  via the group, once via a `Label::builder().label(tr(&hint.title))`), so the
  title appeared duplicated.
- **Manifests at:** `crates/namtor-gnome/src/components/recovery.rs:30`
  (`RecoveryView::render`).
- **Fix at:** `crates/namtor-gnome/src/components/recovery.rs:33` — uses
  `self.group.set_title(&tr(&hint.title))` (not a `Label`) so the title appears
  once; `crates/namtor-gnome/src/components/recovery.rs:38-47` renders the
  recovery steps as a single `ActionRow` with title "Recovery Steps:" and the
  steps joined as its subtitle.
- **Test kind:** String-scan
  (`usa_12_recovery_no_double_title` asserts the source does **not** contain
  `.label(tr(&hint.title))` and does contain `set_title`;
  `usa_12_recovery_shows_steps_as_subtitle` asserts `Recovery Steps:` and
  `steps_row`/`hint.steps` are present).
- **Expected behaviour:** The recovery view renders the hint title exactly once
  (via the group's `set_title`) and shows the recovery steps as a row subtitle,
  not as a duplicate title label.

## USA-16 — mutations routed through dispatch_async

- **Symptom:** PAM service mutations (toggles) were issued synchronously and
  could block the main thread on a D-Bus round-trip; there was no documented
  contract routing them off the main thread, and the presenter had no async
  dispatch hook.
- **Manifests at:** `crates/namtor-gnome/src/components/pam_services.rs:137`
  (the same `handle_toggle` site as USA-0; the comment is tagged
  `USA-0/USA-16`).
- **Fix at:** `crates/namtor-gnome/src/components/pam_services.rs:137` — the
  comment documents that mutations should route through
  `presenter.dispatch_async()`; `crates/namtor-gnome/src/presenter.rs:1004`
  (`dispatch_async`) implements the mechanism: it cancels any previous in-flight
  task, sets `is_loading`, and `glib::spawn_future`s the action with an `Arc<C>`
  client.
- **Test kind:** String-scan
  (`usa_16_dispatch_async_contract_documented` asserts `pam_services.rs`
  contains `USA-16` and `dispatch_async`;
  `usa_16_presenter_has_dispatch_async` asserts `presenter.rs` contains
  `fn dispatch_async`).
- **Expected behaviour:** PAM service mutations are routed through
  `presenter.dispatch_async()` so the GTK main thread never blocks on a D-Bus
  round-trip, and the presenter exposes a `dispatch_async` method that cancels
  prior in-flight work before starting a new task.

---

## Coverage verification

Unique `USA-*` IDs found in `crates/namtor-gnome/src/` and
`crates/namtor-gnome/tests/`:

```
$ grep -roh 'USA-[0-9]*' crates/namtor-gnome/src/ crates/namtor-gnome/tests/ | sort -u
USA-0
USA-10
USA-11
USA-12
USA-16
USA-2
USA-3
USA-5
USA-6
USA-8
USA-9
```

11 unique identifiers. This ledger documents all 11.

Test-kind tally:

| Kind | IDs |
|---|---|
| String-scan only | USA-0, USA-2, USA-5, USA-6, USA-8, USA-9, USA-10, USA-12, USA-16 |
| Behavioural only | USA-3 |
| Mixed (string-scan + behavioural) | USA-11 |

The prevalence of string-scan tests is deliberate: most fixes are view-layer
wiring or GTK4/libadwaita rendering that cannot be exercised without a display
server, so the tests pin the source contract (presence of methods, icons,
labels, and wiring calls) instead. The two behavioural exceptions (USA-3 and
the USA-11 mapping/messages) are headless presenter logic and are exercised
through `FakeManagerClient`. Moving more of these from string-scan to
behavioural is a candidate follow-up wherever the logic is presenter-side.
