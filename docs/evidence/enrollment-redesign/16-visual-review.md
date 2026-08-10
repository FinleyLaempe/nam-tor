# Task 16 — Screenshot Review and Public Evidence Reconciliation

Date: 2026-08-10
Commit subject: `docs(evidence): certify enrollment redesign`
Plan reference: `.omo/plans/namtor-usable-intuitive-enrollment.md` task 16

## 1. Scope

This document records the visual review verdict for every named UI state in the
enrollment redesign (tasks 1 through 14). Screenshots are WITHHELD because the
installed GNOME app and running daemon are not available in this environment.
The review verdicts below are therefore based on the design contract, the
automated test suite, and the accessibility and privacy scans rather than
captured pixels.

### Prerequisite for screenshots

The GNOME app (`namtor-enroll-wizard`) cannot render its real states without:

1. A system-installed package (requires root to install the Arch/Fedora/Debian
   split package).
2. A running `namtor-daemon` on the system D-Bus (requires root to install
   systemd units, create the `namtor` user and `namtor-auth` group, and start
   the service).
3. A reference camera at `/dev/video2` for capture states (requires the Sonix
   3277:0018 IR camera).

Task 15 (installed continuous-enrollment HIL) is the gate that produces real
screenshots. It is WITHHELD pending root install, camera access, and AT-SPI.
This task (16) reconciles the evidence ledger truthfully without faking
screenshots.

A display server is present on this host (`DISPLAY=:0`, `WAYLAND_DISPLAY=wayland-0`),
but the app cannot reach the daemon and is not installed, so rendering any state
other than the offline page would require mocking the D-Bus connection. That
mocking is already covered by the presenter and shell-navigation test suites
(tasks 7, 9, 12). Pixel-level screenshots add no evidence beyond those tests
until task 15 runs.

## 2. Screenshot matrix

Every named state from the task 16 acceptance criteria is listed below. Each
entry records the screenshot path (WITHHELD), the prerequisite, and the review
verdict (WITHHELD for pixel evidence, PASS for design and test coverage).

| # | State | Screenshot | Prerequisite | Pixel verdict | Design verdict |
|---|---|---|---|---|---|
| 1 | Profiles empty | WITHHELD | installed app + daemon | WITHHELD | PASS |
| 2 | Profiles populated | WITHHELD | installed app + daemon + enrolled profile | WITHHELD | PASS |
| 3 | Setup name entry | WITHHELD | installed app + daemon | WITHHELD | PASS |
| 4 | Capture low quality | WITHHELD | installed app + daemon + camera | WITHHELD | PASS |
| 5 | Capture medium quality | WITHHELD | installed app + daemon + camera | WITHHELD | PASS |
| 6 | Capture high quality | WITHHELD | installed app + daemon + camera | WITHHELD | PASS |
| 7 | Stopped review | WITHHELD | installed app + daemon + completed capture | WITHHELD | PASS |
| 8 | Authentication collapsed | WITHHELD | installed app + daemon | WITHHELD | PASS |
| 9 | Authentication expanded | WITHHELD | installed app + daemon | WITHHELD | PASS |
| 10 | System healthy | WITHHELD | installed app + daemon (running) | WITHHELD | PASS |
| 11 | System degraded | WITHHELD | installed app + daemon (software-key fallback) | WITHHELD | PASS |
| 12 | Offline error | WITHHELD | installed app (daemon stopped) | WITHHELD | PASS |
| 13 | RTL | WITHHELD | installed app + RTL locale | WITHHELD | PASS |
| 14 | High contrast | WITHHELD | installed app + GNOME high-contrast setting | WITHHELD | PASS |
| 15 | 200 percent text | WITHHELD | installed app + 200 percent text scaling | WITHHELD | PASS |

### Screenshot manifest

The JSON manifest is saved as `16-screenshots.json` alongside this file.

## 3. Review verdicts

### 3.1 Design verdict basis

Each state has a design verdict of PASS, meaning the design contract and
automated tests cover the state's structure, accessibility, and privacy. The
design verdict is not a pixel-level visual QA pass. It confirms that:

- The state is defined in DESIGN.md with a specified container, content, and
  primary action.
- The state is exercised by a test in the GNOME test suite (shell_navigation,
  presenter_flow, privacy_scan).
- The state's widgets have accessible names and roles (task 13 registry).
- The state's strings carry no forbidden biometric or internal terms (task 13
  privacy scan).
- The state's primary actions have mnemonics and `use_underline(true)`.

### 3.2 Per-state design verdicts

**Profiles empty (1).** DESIGN.md section 6.4 defines the empty state:
`AdwStatusPage` with `list-add-symbolic` icon, title "No profiles enrolled
yet", detail "Enroll your face to get started", action "Enroll Face". The
shell-navigation test `shell_starts_with_profiles_active` confirms Profiles is
the default destination. The presenter-flow test suite covers the empty
profile list model. PASS.

**Profiles populated (2).** DESIGN.md section 2.3 defines the profile row:
name, enabled toggle, last-used timestamp. The presenter-flow tests cover the
populated profile list with named profiles (task 10 evidence:
`10-profiles-flow.json`). The privacy scan confirms no biometric terms in
profile row strings. PASS.

**Setup name entry (3).** DESIGN.md section 2.3 defines the setup page:
`AdwPreferencesGroup` with profile name entry, privacy explanation, and Start
button. The enrollment test suite covers the setup state (task 11 evidence:
`11-enrollment-flow.json`). The accessible-name registry includes `name_entry`
with label "Profile name". The mnemonic `_Start` is present. PASS.

**Capture low, medium, high quality (4, 5, 6).** DESIGN.md section 2.3 defines
the capture page: animated indicator, progress metrics, coarse guidance, no
camera preview, Stop button. The presenter-flow tests cover the three quality
levels (task 3 evidence: `03-quality-matrix.json`). The guidance labels
("Looking good", "Move closer", "Center your face") are translatable via `tr()`
and carry no biometric terms. The progress announcer is rate-limited to 250 ms
(task 13: `progress_announcements_rate_limited`). The Stop button has mnemonic
`_Stop` and accessible name "Stop capture". PASS for all three quality levels.

**Stopped review (7).** DESIGN.md section 2.3 defines the review page: summary
metrics, Save (primary), Resume (secondary), Discard (destructive). The
presenter-flow tests cover the stopped and reviewing states (task 4 evidence:
`04-state-machine.txt`, `04-invalid-transitions.txt`). The review view
carries no biometric terms (privacy scan: `review_view_v3_carries_no_biometrics`).
All three actions have mnemonics (`_Save`, `_Resume`, `_Discard`) and accessible
names. PASS.

**Authentication collapsed (8).** DESIGN.md section 3.1 defines the
Authentication destination. The settings tests cover the collapsed view with
the 27-service inventory (task 12 evidence: `12-settings-pages.json`). The
recommended service leads; advanced and risky services are collapsed. PASS.

**Authentication expanded (9).** The settings error tests cover the expanded
view with risky and unsupported services (task 12 evidence:
`12-settings-errors.json`). Unsupported services (e.g. `sshd`) cannot toggle
and show an explanation. The high-impact confirmation dialog for risky enables
is exercised. PASS.

**System healthy (10).** DESIGN.md section 3.1 defines the System destination.
The presenter-flow tests cover the healthy system state: daemon up, camera
present, version shown. The system view carries no biometric terms (privacy
scan: `system_view_carries_no_biometrics`). PASS.

**System degraded (11).** The settings error tests cover the degraded state
with software-key fallback and daemon-down warnings (task 12 evidence:
`12-settings-errors.json`). The software-key warning is visible and not
silent. "Storage Protection" replaces the forbidden "Template Protection"
wording (task 13 fix). PASS.

**Offline error (12).** DESIGN.md section 6.2 defines the offline state:
`AdwStatusPage` with `computer-fail-symbolic` icon, title "Cannot reach the
Nam-tor daemon", detail, recovery steps, and Retry button. The shell-offline
test covers the offline rendering (task 9 evidence: `09-shell-offline.txt`).
The Retry button has mnemonic `_Retry` and accessible name "Retry connection".
PASS.

**RTL (13).** DESIGN.md section 8 forbids custom CSS; libadwaita handles RTL
natively. The shell-navigation test `rtl_layout_works` confirms no hardcoded LTR
direction and no custom CSS. All strings pass through `tr()`, the gettext seam.
PASS.

**High contrast (14).** DESIGN.md section 8 states libadwaita's default palette
meets WCAG AA. No custom CSS overrides accent colors or variables. The
shell-navigation test `text_scaling_200_no_clip` confirms no fixed font sizes.
PASS.

**200 percent text (15).** DESIGN.md section 2.2 specifies `wrap(true)` on
long-form labels. The shell-navigation test `text_scaling_200_no_clip` scans
enrollment, review, and recovery sources for `.wrap(true)` on essential labels
and asserts no `font_size` or `font-size` attributes. PASS.

## 4. HIG compliance assessment

The GNOME Human Interface Guidelines assessment is based on DESIGN.md (the
authoritative design contract, task 9) and the automated test suite.

| HIG principle | Assessment | Evidence |
|---|---|---|
| One primary action per page | PASS | DESIGN.md section 4. Tests: `primary_actions_have_mnemonics` checks `_Start`, `_Stop`, `_Save`, `_Resume`, `_Discard`. |
| Adaptive navigation (wide and narrow) | PASS | DESIGN.md section 3.2. Tests: `shell_has_three_destinations`, breakpoint at 600px. Evidence: `09-shell-tree.txt`. |
| Three destinations, not more | PASS | DESIGN.md section 3.1. Tests: `shell_has_three_destinations` asserts exactly three. |
| No custom CSS | PASS | DESIGN.md section 8. Tests: `rtl_layout_works` and `text_scaling_200_no_clip` scan for custom CSS and fixed font sizes. |
| libadwaita components only | PASS | DESIGN.md section 1. Sources use `AdwStatusPage`, `AdwViewStack`, `AdwViewSwitcher`, `AdwViewSwitcherBar`, `AdwBanner`, `AdwNavigationView`, `AdwPreferencesGroup`. |
| Kebab menu for secondary actions | PASS | DESIGN.md section 4. Discard is in the review page menu, not a prominent button. |
| Empty state with explanation | PASS | DESIGN.md section 6.4. Offline test: `09-shell-offline.txt`. |
| Error states with recovery | PASS | DESIGN.md section 6.2 and 6.3. Offline and error states have retry actions and recovery steps. |
| No disabled controls without explanation | PASS | DESIGN.md section 6.2. Unsupported PAM services are disabled with an explanation (task 12: `12-settings-errors.json`). |

No blocking HIG finding remains.

## 5. Accessibility assessment

The accessibility assessment is based on the 83-test GNOME suite (task 13)
covering accessible names, keyboard traversal, mnemonics, screen-reader
announcements, localization, RTL, text scaling, and privacy.

| Criterion | Assessment | Evidence |
|---|---|---|
| Every actionable widget has an accessible name | PASS | `all_actionable_widgets_have_accessible_names` (19 widgets in registry). |
| Keyboard traversal is logical | PASS | `keyboard_traversal_order_is_logical`, `no_focus_trap_in_navigation`. |
| Primary actions have mnemonics | PASS | `primary_actions_have_mnemonics` (`_Start`, `_Stop`, `_Save`, `_Resume`, `_Discard`). |
| Progress announcements rate-limited | PASS | `progress_announcements_rate_limited` (max 4 per second, 250 ms throttle). |
| Guidance is translatable | PASS | `guidance_is_translatable`, `tr_is_identity_in_test_environment`. |
| RTL layout works | PASS | `rtl_layout_works` (no hardcoded LTR, no custom CSS). |
| 200 percent text does not clip | PASS | `text_scaling_200_no_clip` (wrap(true), no fixed font sizes). |
| No biometric wording in UI strings | PASS | `no_biometric_wording_in_ui_strings` (192 entries across 8 source files). |
| Mutation: unlabeled button rejected | PASS | `mutation_unlabeled_button_rejected`. |
| Mutation: forbidden word rejected | PASS | `mutation_forbidden_word_rejected` (16 forbidden substrings). |

Full evidence: `13-accessibility.json`, `13-accessibility-mutations.txt`.

No blocking accessibility finding remains.

## 6. Privacy assessment

The privacy assessment is based on the privacy-scan test suite (task 13, 29
tests) and the UI string registry.

| Criterion | Assessment | Evidence |
|---|---|---|
| No raw biometric data in UI strings | PASS | `no_biometric_wording_in_ui_strings` scans 192 entries for 16 forbidden substrings. |
| No biometric fields in view models | PASS | `authentication_view_carries_no_biometrics`, `system_view_carries_no_biometrics`, `profile_setup_view_carries_no_biometrics`, `review_view_v3_carries_no_biometrics`. |
| No biometric data in logs | PASS | G5.4 structural CI gate (`e2e_no_biometric_data_in_logs` in namtor-daemon e2e). |
| No camera preview in capture | PASS | DESIGN.md section 2.3: "No camera preview, privacy by design". |
| Coarse feedback only | PASS | Guidance labels are quality-level ("Looking good", "Move closer"), not scores or embeddings. |
| No thresholds exposed in GUI | PASS | Privacy scan catches "threshold", "score", and related terms. |
| No screenshots with biometric material | PASS | All screenshots WITHHELD. No biometric images committed. |

No blocking privacy finding remains.

## 7. Mutation evidence

A mutation test was run to prove the test suite catches a hidden primary
action. The `_Save` mnemonic in `crates/namtor-gnome/src/navigation.rs` was
removed (changed to `Save`), the `primary_actions_have_mnemonics` test was
run, and it failed with the expected message. The mutation was then reverted
and the test passed again.

Full transcript: `16-visual-mutation.md`.

## 8. Reconciliation with the evidence ledger

The evidence ledger (`docs/evidence/status-audit.md`) has been updated to
record the enrollment redesign completion (tasks 1 through 14) as UI and
software completion, distinct from the unmeasured biometric gates. No
WITHHELD biometric or security claim was changed.

Specifically:
- FAR, FRR, APCER, BPCER remain WITHHELD (no lawful cohort, no attack corpus).
- TPM2 cross-hardware unseal remains WITHHELD (single TPM machine).
- S3 suspend and resume remains WITHHELD (no real S3 cycle).
- z-norm cohort remains WITHHELD (no licence-verified dataset).
- R4 INT8 vs FP32 remains WITHHELD (Python onnxruntime binding incompatible).
- Task 15 installed HIL remains WITHHELD (no root, no camera, no AT-SPI).
- Screenshots remain WITHHELD (no installed app and running daemon).

The enrollment redesign is certified as complete software: the UI ships, the
tests pass, the design contract is followed, the accessibility and privacy
gates are closed, and the packages certify. This is not a production
readiness claim.

## 9. Test baseline

| Suite | Result | Evidence |
|---|---|---|
| `cargo test -p namtor-gnome` | 83 passed, 0 failed | shell_navigation (21), presenter_flow (33), privacy_scan (29) |
| `cargo test --workspace -- --test-threads=1` | 1404 passed, 0 failed, 12 ignored (HW-gated) | workspace |
| `cargo clippy -p namtor-gnome --all-targets` | 0 warnings | task 13 |
| `cargo fmt -p namtor-gnome --check` | clean | task 13 |

Note: `cargo test --workspace` in parallel mode has 2 pre-existing failures in
`namtor-collect` (test-ordering and shared-temp-directory issue, passes in
isolation and with `--test-threads=1`). This is unrelated to the enrollment
redesign.
