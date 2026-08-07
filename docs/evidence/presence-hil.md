# Presence Mode — Hardware-in-the-Loop (HIL) Evidence

**Status: WITHHELD** — no HIL measurement yet. The presence state machine is
unit-tested with mocks; no real camera/emitter/suspend cycle has exercised it.

## Scope

Task 29 implements opt-in presence mode (`crates/namtor-daemon/src/presence.rs`):
absence locking, duty-cycled emitter, privacy pause, emergency stop, suspend
resume, and a no-invisible-mode indicator. The engine is a pure state machine
owning no I/O; the daemon wires its outputs (`emitter_on()`, `indicator()`,
`state()`) to the camera/emitter supervisors.

## Unit evidence (CI, no hardware)

`cargo test -p namtor-daemon --lib` — 12 presence tests green:

- `opt_in_disabled_by_default`
- `user_present_transitions_to_active`
- `user_absent_starts_timeout`
- `timeout_exceeded_triggers_lock`
- `pause_stops_capture`
- `resume_resumes_detection`
- `disable_ends_capture_immediately`
- `emergency_stop_turns_emitter_off`
- `indicator_visible_when_active`
- `no_auth_reuse_after_lock`
- `resume_from_suspend_resets_to_idle`
- `lock_on_absence_disabled_does_not_lock`
- `duty_cycle_phase`

clippy `-D warnings` clean; `cargo fmt --check` clean; SPDX header present;
no `unwrap`/`expect` in non-test code.

## HIL harness ready (not run)

The harness for a real HIL run consists of:

1. **Opt-in enable** — `PresenceConfig { enabled: true, .. }` wired into the
   daemon config, persisted via the existing TOML loader.
2. **Presence detector** — a face-presence signal fed into
   `PresenceEngine::on_user_present` / `on_user_absent`. On the reference
   camera this reuses `namtor-recog`'s YuNet detector on the IR stream.
3. **Emitter supervisor** — daemon applies `engine.emitter_on()` modulated by
   `DutyCycle::is_on` to the MSXU emitter controller + watchdog.
4. **Indicator surface** — `engine.indicator()` rendered by the GNOME shell
   integration (M7) so the no-invisible-mode invariant is user-visible.
5. **Suspend/resume** — `engine.on_resume_from_suspend()` hooked to the
   systemd `PrepareForSleep` / lid-open signal (R10 path).
6. **Lock action** — `PresenceState::Locked` triggers the screen lock via the
   KDE/GNOME session D-Bus interface and requires a fresh PAM re-auth.

## Blocked-on (owner-gated inputs)

- **R10** (emitter survives suspend) is untested on real hardware; the
  suspend-resume reset path is unit-tested only.
- **Presence detector accuracy** (true absence vs. transient occlusion) needs
  a live session; no dataset is licensed for absence-detection tuning.
- **Indicator rendering** depends on the GNOME/KDE shell integration (M7)
  which has no HIL either.

## Gates affected

- G3 (liveness/PAD): no direct overlap; presence mode is a session-lock
  feature, not a PAD gate.
- G5 (security invariants): the no-auth-reuse and no-invisible-mode
  properties are asserted by unit tests; a real lock-action HIL would add an
  end-to-end assertion that the screen actually locks.
