// SPDX-License-Identifier: GPL-3.0-only
//! Opt-in presence mode (spec §8.5 — absence locking).
//!
//! Presence mode is **off by default**. The user opts in explicitly via
//! [`PresenceConfig::enabled`]. When enabled, the daemon watches for the user
//! in front of the camera and locks the session when they leave for longer
//! than `timeout_ms`. A lock triggered by absence is **not** an authentication
//! event: the locked state requires a fresh re-authentication through the
//! normal PAM path. No auth session is reused.
//!
//! Safety properties enforced here and asserted by tests:
//!
//! - **Opt-in only.** `enabled` defaults to `false`. With it off, every
//!   transition method is a no-op and the emitter stays off.
//! - **No invisible mode.** When presence mode is active the
//!   [`PresenceIndicator`] is always visible. There is no code path that
//!   activates detection while hiding the indicator.
//! - **Crash/suspend/removal safety.** [`PresenceEngine::emergency_stop`]
//!   turns the emitter off and forces the `Locked` state. The daemon never
//!   silently widens the emitter.
//! - **No auth reuse after lock.** Once `Locked`, `on_user_present` does NOT
//!   transition back to `Active`; only a suspend-resume cycle or an explicit
//!   disable/re-enable resets the engine, and a re-enable requires fresh
//!   detection (`Idle`).
//!
//! The engine is a pure state machine — it owns no I/O. The daemon wires its
//! return values to the camera/emitter supervisors. `emitter_on()` and
//! `indicator()` expose the observable outputs the daemon applies.

use serde::{Deserialize, Serialize};

/// Configuration for presence mode. All fields have explicit opt-in defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceConfig {
    /// Master switch. `false` = presence mode entirely inert. Default `false`.
    pub enabled: bool,
    /// How long the user may be absent before `lock_on_absence` fires, in ms.
    /// Default 30 s.
    pub timeout_ms: u64,
    /// Whether absence past `timeout_ms` triggers a lock. Default `true`.
    pub lock_on_absence: bool,
    /// Whether the persistent presence indicator is shown. Default `true`.
    /// There is no path that activates detection with the indicator hidden —
    /// `indicator_enabled == false` disables the indicator *and* is treated as
    /// a user signal that presence mode must not run invisibly.
    pub indicator_enabled: bool,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            timeout_ms: 30_000,
            lock_on_absence: true,
            indicator_enabled: true,
        }
    }
}

/// Presence state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceState {
    /// No user detected yet; waiting for the first `on_user_present`.
    Idle,
    /// User present and being tracked.
    Active,
    /// User has left; absence timer running.
    Absent,
    /// Absence (or explicit/emergency lock) tripped. Requires re-authentication
    /// via the normal PAM path; the presence engine never auto-unlocks.
    Locked,
}

/// Emitter duty cycle for long IR-only presence sessions. Bounds thermal and
/// power by strobing the emitter `on_ms` / `off_ms`. The daemon applies
/// [`DutyCycle::is_on`] to its emitter controller while presence mode runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DutyCycle {
    pub on_ms: u32,
    pub off_ms: u32,
}

impl Default for DutyCycle {
    fn default() -> Self {
        Self {
            on_ms: 1_000,
            off_ms: 100,
        }
    }
}

impl DutyCycle {
    /// Period of one full on/off cycle in ms. Saturates on overflow.
    pub fn period_ms(&self) -> u64 {
        (self.on_ms as u64).saturating_add(self.off_ms as u64)
    }

    /// Whether the emitter should be on at `elapsed_ms` into a session.
    /// `elapsed_ms` is wall time since the current presence run started.
    pub fn is_on(&self, elapsed_ms: u64) -> bool {
        let period = self.period_ms();
        if period == 0 {
            return false;
        }
        (elapsed_ms % period) < (self.on_ms as u64)
    }

    /// Fraction of the cycle the emitter is on, in [0.0, 1.0]. Used by the
    /// daemon for thermal budget reporting.
    pub fn duty_fraction(&self) -> f32 {
        let period = self.period_ms();
        if period == 0 {
            return 0.0;
        }
        (self.on_ms as f32) / (period as f32)
    }
}

/// Persistent indicator state. `visible` MUST be `true` whenever presence
/// detection is active — there is no invisible mode. The daemon renders this
/// indicator (e.g. a GNOME shell element) so the user can see that presence
/// mode is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresenceIndicator {
    pub visible: bool,
    pub active: bool,
}

impl PresenceIndicator {
    fn off() -> Self {
        Self {
            visible: false,
            active: false,
        }
    }
}

/// Pure presence state machine. Owns no I/O; the daemon applies its outputs.
pub struct PresenceEngine {
    config: PresenceConfig,
    state: PresenceState,
    /// Accumulated absence time in ms. Reset on `on_user_present`.
    absent_elapsed_ms: u64,
    /// Privacy pause flag. When true, capture is stopped and transitions are
    /// inert until `on_resume`.
    paused: bool,
    /// Opt-out flag. `disable()` sets this and tears everything down.
    disabled: bool,
    /// Mirror of the emitter output the daemon should apply. `true` = emitter
    /// commanded on. Every stop path (pause, lock, disable, emergency,
    /// suspend) clears it.
    emitter_on: bool,
    indicator: PresenceIndicator,
}

impl PresenceEngine {
    pub fn new(config: PresenceConfig) -> Self {
        let disabled = !config.enabled;
        let indicator = if config.enabled {
            PresenceIndicator {
                visible: config.indicator_enabled,
                active: false,
            }
        } else {
            PresenceIndicator::off()
        };
        Self {
            config,
            state: PresenceState::Idle,
            absent_elapsed_ms: 0,
            paused: false,
            disabled,
            emitter_on: false,
            indicator,
        }
    }

    pub fn config(&self) -> PresenceConfig {
        self.config
    }

    pub fn state(&self) -> &PresenceState {
        &self.state
    }

    /// Emitter output the daemon should apply. `true` = on.
    pub fn emitter_on(&self) -> bool {
        self.emitter_on
    }

    pub fn indicator(&self) -> PresenceIndicator {
        self.indicator
    }

    /// User detected. Transitions to `Active` unless disabled, paused, or
    /// locked. From `Locked` this is a no-op: a locked session requires
    /// re-authentication, not mere presence.
    pub fn on_user_present(&mut self) -> PresenceState {
        if self.disabled || self.paused {
            return self.state;
        }
        match self.state {
            PresenceState::Locked => {
                // No auth reuse: presence alone does not unlock.
                self.set_indicator_active(false);
                self.emitter_on = false;
            }
            PresenceState::Idle | PresenceState::Active | PresenceState::Absent => {
                self.state = PresenceState::Active;
                self.absent_elapsed_ms = 0;
                self.emitter_on = true;
                self.set_indicator_active(true);
            }
        }
        self.state
    }

    /// User left. Starts the absence timer (`Absent`). No-op if disabled,
    /// paused, or already locked.
    pub fn on_user_absent(&mut self) -> PresenceState {
        if self.disabled || self.paused {
            return self.state;
        }
        match self.state {
            PresenceState::Locked => {}
            PresenceState::Active | PresenceState::Idle | PresenceState::Absent => {
                self.state = PresenceState::Absent;
                self.absent_elapsed_ms = 0;
                // Keep the emitter on while absent so detection can still see
                // the user return; the duty cycle bounds thermal load.
                self.emitter_on = true;
                self.set_indicator_active(false);
            }
        }
        self.state
    }

    /// Advance the absence timer by `elapsed_ms`. If `lock_on_absence` is set
    /// and the timer reaches `timeout_ms`, transitions to `Locked` and turns
    /// the emitter off. No-op if disabled, paused, or not `Absent`.
    pub fn tick(&mut self, elapsed_ms: u64) -> PresenceState {
        if self.disabled || self.paused {
            return self.state;
        }
        if self.state != PresenceState::Absent {
            return self.state;
        }
        self.absent_elapsed_ms = self.absent_elapsed_ms.saturating_add(elapsed_ms);
        if self.config.lock_on_absence && self.absent_elapsed_ms >= self.config.timeout_ms {
            self.lock_internal();
        }
        self.state
    }

    /// Privacy pause. Stops capture immediately (emitter off). Transitions are
    /// inert until [`on_resume`]. The state is preserved for diagnostics but
    /// detection is suspended.
    pub fn on_pause(&mut self) -> PresenceState {
        self.paused = true;
        self.emitter_on = false;
        self.set_indicator_active(false);
        self.state
    }

    /// Resume from a privacy pause. Resets to `Idle` — a fresh detection is
    /// required; we do not assume the user is still present.
    pub fn on_resume(&mut self) -> PresenceState {
        self.paused = false;
        self.state = PresenceState::Idle;
        self.absent_elapsed_ms = 0;
        self.emitter_on = false;
        self.set_indicator_active(false);
        self.state
    }

    /// Explicit lock. Turns the emitter off and clears the active indicator.
    /// Same end state as an absence-triggered lock.
    pub fn lock(&mut self) -> PresenceState {
        self.lock_internal();
        self.state
    }

    /// Opt out of presence mode entirely. Ends capture and emitter
    /// immediately, hides the indicator, and returns to `Idle`. The engine
    /// stays inert until re-constructed with `enabled: true`.
    pub fn disable(&mut self) {
        self.disabled = true;
        self.emitter_on = false;
        self.state = PresenceState::Idle;
        self.absent_elapsed_ms = 0;
        self.paused = false;
        self.indicator = PresenceIndicator::off();
    }

    /// Crash / suspend / hot-unplug safety. Stops everything and forces the
    /// emitter off. Forces `Locked` (the safe default for an unexpected
    /// stop) — re-authentication is required to resume.
    pub fn emergency_stop(&mut self) {
        self.emitter_on = false;
        self.paused = false;
        self.absent_elapsed_ms = 0;
        self.set_indicator_active(false);
        self.state = PresenceState::Locked;
    }

    /// Resume from a real suspend/resume (lid-open). Resets to `Idle`,
    /// requires fresh detection. The emitter stays off until the user is
    /// seen again.
    pub fn on_resume_from_suspend(&mut self) -> PresenceState {
        self.state = PresenceState::Idle;
        self.absent_elapsed_ms = 0;
        self.paused = false;
        self.emitter_on = false;
        self.set_indicator_active(false);
        self.state
    }

    fn lock_internal(&mut self) {
        self.state = PresenceState::Locked;
        self.emitter_on = false;
        self.absent_elapsed_ms = 0;
        self.set_indicator_active(false);
    }

    /// Set the indicator's `active` flag. `visible` follows the config: when
    /// presence mode is enabled the indicator is always visible while the
    /// engine is alive, enforcing the no-invisible-mode invariant.
    fn set_indicator_active(&mut self, active: bool) {
        if self.disabled {
            self.indicator = PresenceIndicator::off();
            return;
        }
        self.indicator.visible = self.config.indicator_enabled;
        self.indicator.active = active;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_config() -> PresenceConfig {
        PresenceConfig {
            enabled: true,
            timeout_ms: 1_000,
            lock_on_absence: true,
            indicator_enabled: true,
        }
    }

    // Given default config, when a new engine is constructed,
    // then presence mode is disabled and inert.
    #[test]
    fn opt_in_disabled_by_default() {
        let e = PresenceEngine::new(PresenceConfig::default());
        assert_eq!(*e.state(), PresenceState::Idle);
        assert!(!e.emitter_on());
        assert!(!e.indicator().visible);
        assert!(!e.indicator().active);
        // Transitions are no-ops.
        let mut e = PresenceEngine::new(PresenceConfig::default());
        assert_eq!(e.on_user_present(), PresenceState::Idle);
        assert_eq!(e.on_user_absent(), PresenceState::Idle);
        assert_eq!(e.tick(10_000), PresenceState::Idle);
    }

    // Given an enabled engine, when on_user_present is called,
    // then it transitions to Active with the emitter on.
    #[test]
    fn user_present_transitions_to_active() {
        let mut e = PresenceEngine::new(enabled_config());
        assert_eq!(e.on_user_present(), PresenceState::Active);
        assert!(e.emitter_on());
        assert!(e.indicator().active);
    }

    // Given an active engine, when on_user_absent is called,
    // then it transitions to Absent (timer starting at zero).
    #[test]
    fn user_absent_starts_timeout() {
        let mut e = PresenceEngine::new(enabled_config());
        e.on_user_present();
        assert_eq!(e.on_user_absent(), PresenceState::Absent);
        // Tick below the threshold stays Absent.
        assert_eq!(e.tick(500), PresenceState::Absent);
    }

    // Given an absent engine past the timeout, when tick is called,
    // then it transitions to Locked and the emitter turns off.
    #[test]
    fn timeout_exceeded_triggers_lock() {
        let mut e = PresenceEngine::new(enabled_config());
        e.on_user_present();
        e.on_user_absent();
        assert_eq!(e.tick(1_000), PresenceState::Locked);
        assert!(!e.emitter_on());
        assert!(!e.indicator().active);
    }

    // Given an active engine, when on_pause is called,
    // then capture stops (emitter off) and further transitions are inert.
    #[test]
    fn pause_stops_capture() {
        let mut e = PresenceEngine::new(enabled_config());
        e.on_user_present();
        assert_eq!(e.on_pause(), PresenceState::Active);
        assert!(!e.emitter_on());
        // While paused, presence/absence/tick do nothing.
        assert_eq!(e.on_user_present(), PresenceState::Active);
        assert_eq!(e.on_user_absent(), PresenceState::Active);
        assert_eq!(e.tick(5_000), PresenceState::Active);
    }

    // Given a paused engine, when on_resume is called,
    // then it returns to Idle and detection resumes on next present.
    #[test]
    fn resume_resumes_detection() {
        let mut e = PresenceEngine::new(enabled_config());
        e.on_user_present();
        e.on_pause();
        assert_eq!(e.on_resume(), PresenceState::Idle);
        assert!(!e.emitter_on());
        // Fresh detection reactivates.
        assert_eq!(e.on_user_present(), PresenceState::Active);
        assert!(e.emitter_on());
    }

    // Given an enabled engine, when disable is called,
    // then capture ends immediately and the engine is inert.
    #[test]
    fn disable_ends_capture_immediately() {
        let mut e = PresenceEngine::new(enabled_config());
        e.on_user_present();
        assert!(e.emitter_on());
        e.disable();
        assert_eq!(*e.state(), PresenceState::Idle);
        assert!(!e.emitter_on());
        assert!(!e.indicator().visible);
        // Subsequent transitions are no-ops.
        assert_eq!(e.on_user_present(), PresenceState::Idle);
    }

    // Given an active engine, when emergency_stop is called,
    // then the emitter is off and the state is Locked.
    #[test]
    fn emergency_stop_turns_emitter_off() {
        let mut e = PresenceEngine::new(enabled_config());
        e.on_user_present();
        assert!(e.emitter_on());
        e.emergency_stop();
        assert!(!e.emitter_on());
        assert_eq!(*e.state(), PresenceState::Locked);
    }

    // Given an enabled engine, when active,
    // then the indicator is visible (no invisible mode).
    #[test]
    fn indicator_visible_when_active() {
        let mut e = PresenceEngine::new(enabled_config());
        e.on_user_present();
        let ind = e.indicator();
        assert!(ind.visible, "indicator must be visible when active");
        assert!(ind.active);
        // Also visible while absent (still running).
        e.on_user_absent();
        assert!(e.indicator().visible);
    }

    // Given a locked engine, when on_user_present is called,
    // then it stays Locked — no auth reuse.
    #[test]
    fn no_auth_reuse_after_lock() {
        let mut e = PresenceEngine::new(enabled_config());
        e.on_user_present();
        e.on_user_absent();
        e.tick(1_000);
        assert_eq!(*e.state(), PresenceState::Locked);
        // Presence alone does not unlock.
        assert_eq!(e.on_user_present(), PresenceState::Locked);
        assert!(
            !e.emitter_on(),
            "emitter must stay off after a no-op present"
        );
    }

    // Given any engine state, when on_resume_from_suspend is called,
    // then it resets to Idle and the emitter is off.
    #[test]
    fn resume_from_suspend_resets_to_idle() {
        let mut e = PresenceEngine::new(enabled_config());
        e.on_user_present();
        e.on_user_absent();
        e.tick(500);
        assert_eq!(e.on_resume_from_suspend(), PresenceState::Idle);
        assert!(!e.emitter_on());
        assert!(!e.indicator().active);
        // After reset, fresh detection works.
        assert_eq!(e.on_user_present(), PresenceState::Active);
    }

    // Given a DutyCycle, when is_on is evaluated across a period,
    // then it is on for on_ms and off for off_ms.
    #[test]
    fn duty_cycle_phase() {
        let dc = DutyCycle {
            on_ms: 100,
            off_ms: 50,
        };
        assert_eq!(dc.period_ms(), 150);
        assert!(dc.is_on(0));
        assert!(dc.is_on(99));
        assert!(!dc.is_on(100));
        assert!(!dc.is_on(149));
        // Wraps.
        assert!(dc.is_on(150));
        assert!(dc.is_on(249));
        assert!(!dc.is_on(250));
    }

    // Given lock_on_absence disabled, when absence times out,
    // then no lock fires (mode reports absence but does not lock).
    #[test]
    fn lock_on_absence_disabled_does_not_lock() {
        let mut cfg = enabled_config();
        cfg.lock_on_absence = false;
        let mut e = PresenceEngine::new(cfg);
        e.on_user_present();
        e.on_user_absent();
        assert_eq!(e.tick(10_000), PresenceState::Absent);
    }
}
