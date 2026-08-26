# Source-String-Scan Test Audit (GUI Crates)

Audit of every test that scans source text (`src.contains(...)`,
`include_str!(...).contains(...)`, or grep-based content assertion) instead of
exercising runtime behaviour. Produced by Wave 1 task 2 of the
`namtor-ui-native-unification` plan. Task 14 (Wave 3) consumes this list as its
input and replaces every GNOME entry with a behavioural test.

## Why string-scan tests are insufficient

A `src.contains("USA-0")` assertion passes as long as the literal token appears
anywhere in the source file: in a comment, a dead branch, a string literal. It
does not prove the code runs, the branch is reachable, or the widget is wired.
It tests the text of the program, not its behaviour. A regression that deletes
the call but keeps the comment stays green. These tests are CI-grade scaffolding
for fixes that were never behaviourally verified, and they are the first thing
Task 14 deletes.

## Summary

| Location | Pattern | Match count |
|---|---|---|
| `crates/namtor-gnome/tests/gnome_bug_fixes.rs` | `src.contains` | 30 |
| `crates/namtor-gnome/tests/shell_navigation.rs` | `src.contains` | 34 |
| `crates/namtor-gnome/tests/presenter_flow.rs` | `source.contains` (include_str) | 1 (not in grep count) |
| `crates/namtor-gnome/tests/wizard_integration.rs` | `*.contains` (include_str, varied names) | 14 (not in grep count) |
| `crates/namtor-kde/tests/abi_smoke.rs` | none (behavioural) | 0 |
| `packaging/tests/test_doc_audit.sh` | `grep -qF` | 6 distinct checks |
| `packaging/tests/test_release_ledger.sh` | `grep -qi` | 3 distinct checks |
| **Grep-verified total** (`src.contains\|include_str.*contains`) | | **64** |

The grep-verified total (64) is the count returned by
`grep -rn 'src.contains\|include_str.*contains' crates/namtor-gnome/tests/ crates/namtor-kde/tests/ | wc -l`.
The `presenter_flow.rs` and `wizard_integration.rs` tests use different variable
names (`source`, `wizard`, `app`, `enrollment`, `review`, `profile_list`) and
place `include_str!` on a separate line from `.contains()`, so they do not match
the narrow grep pattern. They are still source-string-scan tests and are listed
below. The KDE `abi_smoke.rs` was audited and found to be behavioural (it calls
FFI functions and asserts return values), so it has no entry in the replacement
table.

## Replacement task map

| USA / area | Wave 3 task | Task description |
|---|---|---|
| USA-0 (async PAM toggle) | Task 10 | Route polkit PAM toggle through `dispatch_async` |
| USA-2 (retry connection) | Task 11 | `retry_connection` re-runs `connect()` and rebuilds UI |
| USA-3 (duplicate name) | Task 12 | Duplicate-name check stages `PendingReplace` |
| USA-5 (stopped error) | Task 14 | Replace with behavioural test |
| USA-6 (auto reconnect) | Task 11 + Task 14 | Reconnect mechanism + behavioural test |
| USA-8 (restart/logs rows) | Task 14 | Replace with behavioural test |
| USA-9 (completion cue) | Task 13 + Task 14 | Framing guidance + behavioural test |
| USA-10 (rename dialog) | Task 9 | Rename dialog with `AdwEntryRow` |
| USA-11 (polkit variants) | Task 14 | Replace string-scan; keep existing behavioural tests |
| USA-12 (recovery double title) | Task 14 | Replace with behavioural test |
| USA-16 (dispatch_async mutations) | Task 10 + Task 14 | Async dispatch + behavioural test |
| shell structure / a11y / RTL | Task 14 | Replace all structural source-contract scans |
| dbus_client system bus | Task 7 + Task 14 | Delete `dbus_client.rs`; behavioural client test |
| wizard placeholder / wiring | Task 14 | Replace with behavioural test |
| packaging doc audit | Not replaced | Legitimate doc-content enforcement (see note) |
| packaging release ledger | Not replaced | Legitimate doc-content enforcement (see note) |

Task 14 is the catch-all: its acceptance criterion is
`grep -rn 'src.contains\|include_str.*contains' crates/namtor-gnome/tests/`
returns 0 matches. Every GNOME entry below is removed by Task 14.

---

## A. `crates/namtor-gnome/tests/gnome_bug_fixes.rs` (30 matches)

Every entry in this section uses `include_str!` to load a source file into a
`src` binding, then asserts `src.contains(...)`. The variable is always named
`src`, so every match line carries the literal `src.contains` pattern.

| # | File:line | Assertion | What it checks | Why insufficient | Replaced by |
|---|---|---|---|---|---|
| 1 | gnome_bug_fixes.rs:27 | `src.contains("USA-0")` | pam_services.rs mentions the USA-0 token | Token can live in a comment with no async code | Task 10 + Task 14 |
| 2 | gnome_bug_fixes.rs:31 | `src.contains("dispatch_async")` | pam_services.rs mentions `dispatch_async` | String presence does not prove the toggle routes through it | Task 10 + Task 14 |
| 3 | gnome_bug_fixes.rs:46 | `src.contains("fn retry_connection")` | app.rs declares `retry_connection` | Method can exist as a no-op stub | Task 11 + Task 14 |
| 4 | gnome_bug_fixes.rs:50 | `src.contains("DbusManagerClient::connect()")` | app.rs references `connect()` | Reference can be in a comment or dead branch | Task 11 + Task 14 |
| 5 | gnome_bug_fixes.rs:182 | `src.contains("fn render_stopped")` | enrollment.rs declares `render_stopped` | Method signature does not prove it renders the error | Task 14 |
| 6 | gnome_bug_fixes.rs:186 | `src.contains("error") && src.contains("USA-5")` | enrollment.rs mentions `error` and `USA-5` | Two loose tokens do not prove an error parameter is wired | Task 14 |
| 7 | gnome_bug_fixes.rs:202 | `src.contains("retry_connection")` | app.rs mentions `retry_connection` | Same as #3; token presence only | Task 11 + Task 14 |
| 8 | gnome_bug_fixes.rs:228 | `src.contains("restart_row") && src.contains("connect_activated")` | system_status.rs wires restart row | Both tokens can coexist without being on the same row | Task 14 |
| 9 | gnome_bug_fixes.rs:232 | `src.contains("logs_row") && src.contains("connect_activated")` | system_status.rs wires logs row | Same flaw as #8 | Task 14 |
| 10 | gnome_bug_fixes.rs:236 | `src.contains("USA-8")` | system_status.rs mentions USA-8 | Comment token only | Task 14 |
| 11 | gnome_bug_fixes.rs:251 | `src.contains("USA-9")` | enrollment.rs mentions USA-9 | Comment token only | Task 13 + Task 14 |
| 12 | gnome_bug_fixes.rs:255 | `src.contains("diversity_percent >= 100")` | enrollment.rs has the threshold expression | Expression can be unreachable or inverted | Task 13 + Task 14 |
| 13 | gnome_bug_fixes.rs:259 | `src.contains("quality_bucket >= 2")` | enrollment.rs has the quality threshold | Same flaw as #12 | Task 13 + Task 14 |
| 14 | gnome_bug_fixes.rs:263 | `src.contains("Enrollment complete")` | enrollment.rs has completion text | String literal can be unused | Task 13 + Task 14 |
| 15 | gnome_bug_fixes.rs:278 | `src.contains("RenameCallback")` | profile_list.rs defines `RenameCallback` | Type can exist without a dialog | Task 9 + Task 14 |
| 16 | gnome_bug_fixes.rs:282 | `src.contains("rename_btn")` | profile_list.rs has a rename button binding | Binding can be unused | Task 9 + Task 14 |
| 17 | gnome_bug_fixes.rs:286 | `src.contains("document-edit-symbolic")` | rename button uses edit icon | Icon name can be in a comment | Task 9 + Task 14 |
| 18 | gnome_bug_fixes.rs:290 | `src.contains("Rename profile")` | rename button has accessible label | String literal can be unused | Task 9 + Task 14 |
| 19 | gnome_bug_fixes.rs:304 | `src.contains("enum PolkitAuthError")` | polkit_auth.rs defines the enum | Enum can exist without correct variant mapping | Task 14 |
| 20 | gnome_bug_fixes.rs:307 | `src.contains("Bus")` | polkit_auth.rs has Bus variant | Bare `Bus` matches any context | Task 14 |
| 21 | gnome_bug_fixes.rs:308 | `src.contains("Check")` | polkit_auth.rs has Check variant | Bare `Check` matches any context | Task 14 |
| 22 | gnome_bug_fixes.rs:309 | `src.contains("Denied")` | polkit_auth.rs has Denied variant | Bare `Denied` matches any context | Task 14 |
| 23 | gnome_bug_fixes.rs:311 | `src.contains("Cancelled")` | polkit_auth.rs has Cancelled variant | Bare `Cancelled` matches any context | Task 14 |
| 24 | gnome_bug_fixes.rs:375 | `!src.contains(".label(tr(&hint.title))")` | recovery.rs does not use old double-title pattern | Absence of a string does not prove the new code renders correctly | Task 14 |
| 25 | gnome_bug_fixes.rs:380 | `src.contains("set_title")` | recovery.rs uses `set_title` | Token can appear in unrelated code | Task 14 |
| 26 | gnome_bug_fixes.rs:391 | `src.contains("Recovery Steps:")` | recovery.rs has steps label | String literal can be unused | Task 14 |
| 27 | gnome_bug_fixes.rs:395 | `src.contains("steps_row") \|\| src.contains("hint.steps")` | recovery.rs renders steps as a row | Either token alone satisfies the OR | Task 14 |
| 28 | gnome_bug_fixes.rs:410 | `src.contains("USA-16")` | pam_services.rs mentions USA-16 | Comment token only | Task 10 + Task 14 |
| 29 | gnome_bug_fixes.rs:414 | `src.contains("dispatch_async")` | pam_services.rs mentions dispatch_async | Same flaw as #2 | Task 10 + Task 14 |
| 30 | gnome_bug_fixes.rs:424 | `src.contains("fn dispatch_async")` | presenter.rs declares `dispatch_async` | Method can exist without being called by mutations | Task 10 + Task 14 |

---

## B. `crates/namtor-gnome/tests/shell_navigation.rs` (34 matches)

All entries use `include_str!` into a `src` binding and assert `src.contains`.

| # | File:line | Assertion | What it checks | Why insufficient | Replaced by |
|---|---|---|---|---|---|
| 31 | shell_navigation.rs:210 | `!src.contains("set_direction(gtk4::TextDirection::Ltr)")` | shell.rs/nav.rs do not hardcode LTR | Absence of string does not prove RTL renders correctly | Task 14 |
| 32 | shell_navigation.rs:214 | `!src.contains("TextDirection::Ltr")` | no LTR direction override | Same negative-scan flaw | Task 14 |
| 33 | shell_navigation.rs:248 | `!src.contains("font_size") && !src.contains("font-size")` | no fixed font sizes | Absence does not prove text scales at 200% | Task 14 |
| 34 | shell_navigation.rs:381 | `!src.contains("set_focus_chain")` | no explicit focus chain | Absence does not prove Tab traversal is logical | Task 14 |
| 35 | shell_navigation.rs:384 | `!src.contains("grab_focus_loop")` | no focus grab loops | Same negative-scan flaw | Task 14 |
| 36 | shell_navigation.rs:395 | `src.contains(&needle)` | mnemonic `_Start` etc. present | Token presence does not prove mnemonic activates | Task 14 |
| 37 | shell_navigation.rs:400 | `src.contains(".use_underline(true)")` | use_underline enabled | Token can be on an unrelated widget | Task 14 |
| 38 | shell_navigation.rs:447 | `src.contains("adw::PreferencesDialog")` | shell uses PreferencesDialog | Token presence does not prove dialog is constructed | Task 14 |
| 39 | shell_navigation.rs:451 | `!src.contains("adw::ViewStack")` | old ViewStack gone | Absence does not prove new structure works | Task 14 |
| 40 | shell_navigation.rs:455 | `!src.contains("adw::ViewSwitcherBar")` | old ViewSwitcherBar gone | Same negative-scan flaw | Task 14 |
| 41 | shell_navigation.rs:459 | `!src.contains("adw::ViewSwitcher")` | old ViewSwitcher gone | Same negative-scan flaw | Task 14 |
| 42 | shell_navigation.rs:492 | `src.contains("push_subpage")` | shell exposes push_subpage | Token can be in a comment | Task 14 |
| 43 | shell_navigation.rs:496 | `src.contains("pop_subpage")` | shell exposes pop_subpage | Token can be in a comment | Task 14 |
| 44 | shell_navigation.rs:506 | `src.contains("adw::ApplicationWindow")` | shell uses ApplicationWindow | Token presence does not prove window is built | Task 14 |
| 45 | shell_navigation.rs:517 | `!src.contains("ScrolledWindow")` | no ScrolledWindow wrapping | Absence does not prove PreferencesDialog scrolls | Task 14 |
| 46 | shell_navigation.rs:528 | `!src.contains("Breakpoint")` | no Breakpoint adaptive switching | Absence does not prove adaptation works | Task 14 |
| 47 | shell_navigation.rs:539 | `src.contains("adw::PreferencesPage")` | enrollment builds PreferencesPage | Token presence only | Task 14 |
| 48 | shell_navigation.rs:543 | `src.contains("navigation_page")` | enrollment exposes navigation_page | Token can be a comment | Task 14 |
| 49 | shell_navigation.rs:547 | `src.contains("adw::NavigationPage")` | enrollment wraps in NavigationPage | Token presence only | Task 14 |
| 50 | shell_navigation.rs:558 | `src.contains("adw::PreferencesPage")` | review builds PreferencesPage | Token presence only | Task 14 |
| 51 | shell_navigation.rs:562 | `src.contains("navigation_page")` | review exposes navigation_page | Token can be a comment | Task 14 |
| 52 | shell_navigation.rs:566 | `src.contains("adw::NavigationPage")` | review wraps in NavigationPage | Token presence only | Task 14 |
| 53 | shell_navigation.rs:577 | `src.contains("adw::PreferencesGroup")` | profile list uses PreferencesGroup | Token presence only | Task 14 |
| 54 | shell_navigation.rs:581 | `src.contains("adw::ActionRow")` | profile list uses ActionRow | Token presence only | Task 14 |
| 55 | shell_navigation.rs:585 | `src.contains("adw::SwitchRow")` | profile list uses SwitchRow | Token presence only | Task 14 |
| 56 | shell_navigation.rs:589 | `!src.contains("CheckButton")` | profile list does not use CheckButton | Absence does not prove SwitchRow is wired | Task 14 |
| 57 | shell_navigation.rs:600 | `src.contains("adw::PreferencesGroup")` | pam_services uses PreferencesGroup | Token presence only | Task 14 |
| 58 | shell_navigation.rs:604 | `src.contains("adw::ActionRow")` | pam_services uses ActionRow | Token presence only | Task 14 |
| 59 | shell_navigation.rs:608 | `src.contains("adw::SwitchRow")` | pam_services uses SwitchRow | Token presence only | Task 14 |
| 60 | shell_navigation.rs:619 | `src.contains("adw::PreferencesGroup")` | system_status uses PreferencesGroup | Token presence only | Task 14 |
| 61 | shell_navigation.rs:623 | `src.contains("adw::ActionRow")` | system_status uses ActionRow | Token presence only | Task 14 |
| 62 | shell_navigation.rs:634 | `src.contains("set_profiles_content") \|\| src.contains("add_profiles_group")` | app drives new Shell API | Either token alone satisfies the OR | Task 14 |
| 63 | shell_navigation.rs:638 | `src.contains("push_subpage")` | app uses push_subpage | Token can be in a comment | Task 14 |
| 64 | shell_navigation.rs:642 | `src.contains("pop_subpage")` | app uses pop_subpage | Token can be in a comment | Task 14 |

---

## C. `crates/namtor-gnome/tests/presenter_flow.rs` (1 source-scan test)

This test uses `source` (not `src`) as the variable name and places
`include_str!` on a separate line, so it does not match the narrow grep pattern.
It is still a source-string-scan test.

| File:line | Assertion | What it checks | Why insufficient | Replaced by |
|---|---|---|---|---|
| presenter_flow.rs:391-398 | `source.contains("Connection::system()")` and `!source.contains("Connection::session()")` | dbus_client.rs uses the system bus | Token presence/absence does not prove the runtime connection is system bus. Task 7 deletes dbus_client.rs entirely (rewire onto namtor-client). | Task 7 + Task 14 |

---

## D. `crates/namtor-gnome/tests/wizard_integration.rs` (14 source-scan tests)

These tests use varied variable names (`wizard`, `app`, `enrollment`, `review`,
`profile_list`) and place `include_str!` on a separate line, so they do not
match the narrow grep pattern. They are all source-string-scan tests that guard
against a regression to the placeholder-only wizard.

| File:line | Assertion | What it checks | Why insufficient | Replaced by |
|---|---|---|---|---|
| wizard_integration.rs:19-23 | `!wizard.contains("will appear here")` | wizard.rs has no placeholder text | Absence of a string does not prove real UI is wired | Task 14 |
| wizard_integration.rs:30-36 | `wizard.contains("app::") \|\| app.contains("DbusManagerClient")` | wizard delegates to app controller | Either token alone satisfies the OR | Task 14 |
| wizard_integration.rs:43-47 | `app.contains("ProfilePresenter")` | app instantiates ProfilePresenter | Token presence does not prove instantiation | Task 14 |
| wizard_integration.rs:54-58 | `app.contains("ProfileListView")` | app renders ProfileListView | Token presence only | Task 14 |
| wizard_integration.rs:65-69 | `app.contains("render_pam_services")` | app calls render_pam_services | Token presence only | Task 14 |
| wizard_integration.rs:76-80 | `app.contains("render_system_status")` | app calls render_system_status | Token presence only | Task 14 |
| wizard_integration.rs:88-96 | `app.contains("EnrollmentViewComponent")` and `app.contains("ReviewViewComponent")` | app uses enrollment and review components | Token presence only | Task 14 |
| wizard_integration.rs:103-116 | `app.contains(method)` for 6 v3 lifecycle methods | app calls all v3 lifecycle methods | Token presence does not prove callbacks are wired | Task 14 |
| wizard_integration.rs:124-135 | `!enrollment.contains("_on_start")` and `enrollment.contains("connect_clicked")` | enrollment wires callbacks | Absence of underscore prefix does not prove connection | Task 14 |
| wizard_integration.rs:143-151 | `!review.contains("_on_save")` and `review.contains("connect_clicked")` | review wires callbacks | Same flaw | Task 14 |
| wizard_integration.rs:161-165 | `profile_list.contains("connect_active_notify")` | profile list wires toggle | Token presence only | Task 14 |
| wizard_integration.rs:174-188 | `!stdout.contains("will appear here")` (binary strings scan) | built binary has no placeholder | Grep over `strings` output; ignored in CI | Task 14 |

---

## E. `crates/namtor-kde/tests/abi_smoke.rs` (audited, no findings)

`abi_smoke.rs` was audited per the plan reference. It is **not** a source-string-
scan test. It calls the exported C ABI functions
(`namtor_kde_abi_version`, `namtor_kde_manager_bus_name`,
`namtor_kde_manager_object_path`) via FFI and asserts the return values equal
the canonical constants. The companion `src/tests.rs::abi_strings_match_bridge_constants`
is also behavioural (asserts FFI return values equal bridge constants). No
replacement needed. KDE has zero source-string-scan tests in its `tests/`
directory.

---

## F. `packaging/tests/test_doc_audit.sh` (grep-based doc enforcement)

These are shell `grep -qF` / `grep -qiF` checks over documentation files
(README.md, packaging/README.md, COPYING.md, status-audit.md). They assert that
required warning phrases, package names, and distro commands are present in
documentation, and that stale strings are absent.

| File:line | Grep assertion | What it checks | Replacement |
|---|---|---|---|
| test_doc_audit.sh:86 | `grep -qF "$pattern" "$f"` (stale strings) | No stale strings in audited docs | Not replaced |
| test_doc_audit.sh:111 | `grep -qiF "$phrase" "$f"` (experimental preview) | README has experimental-preview warning | Not replaced |
| test_doc_audit.sh:136 | `grep -qF "$pkg" "$f"` (package names) | README documents all three package names | Not replaced |
| test_doc_audit.sh:142 | `grep -qF "namtor" "$f"` | Core package name present | Not replaced |
| test_doc_audit.sh:154 | `grep -qF "$cmd" README.md` | Distro install commands documented | Not replaced |
| test_doc_audit.sh:186 | `grep -oP '\[[^]]*\]\(\K[^)]+'` | Internal markdown links resolve | Not replaced |

**Note:** These are documentation-content assertions, not code-behaviour tests.
They enforce that human-readable docs contain required safety warnings and
install instructions. A grep over a README is the correct tool for this job:
there is no runtime behaviour to exercise. They are listed here for completeness
because the plan referenced them, but Task 14 does not touch them and no
behavioural replacement is planned.

---

## G. `packaging/tests/test_release_ledger.sh` (grep-based release enforcement)

Shell `grep -qi` checks over README.md and status-audit.md that enforce public
claims (experimental-preview label, WITHHELD gates table, settings-only setup).

| File:line | Grep assertion | What it checks | Replacement |
|---|---|---|---|
| test_release_ledger.sh:227 | `grep -qi "experimental preview" "$README"` | README states experimental preview | Not replaced |
| test_release_ledger.sh:236 | `grep -qi "WITHHELD" "$README"` | README has WITHHELD gates table | Not replaced |
| test_release_ledger.sh:245 | `grep -qiE "Settings-only\|Settings app" "$README"` | README setup is Settings-only | Not replaced |

**Note:** Same rationale as section F. These enforce documentation content, not
code behaviour. No behavioural replacement is planned.

---

## Verification

The grep-verified count of `src.contains` / `include_str.*contains` matches
across the two GUI test directories is 64 (30 in gnome_bug_fixes.rs, 34 in
shell_navigation.rs). This document lists all 64 plus the additional
source-scan tests in presenter_flow.rs and wizard_integration.rs that use
different variable names, the audited KDE abi_smoke.rs (no findings), and the
packaging grep-based doc/release enforcement tests.
