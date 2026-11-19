// SPDX-License-Identifier: GPL-3.0-only
//! Emitter duty-cycle state machine (spec §4.10.2).
//!
//! The emitter's continuous-on budget (`max_on_ms`, default 2000) is a
//! safety property: 850 nm emission at laptop distance must be duty-cycled,
//! not continuous. Before this module the per-frame watchdog forced the
//! emitter off on expiry and nothing ever turned it back on — every capture
//! longer than `max_on_ms` went permanently dark (issue #30).
//!
//! [`DutyCycler`] tracks three phases and nothing else:
//!
//! - `On { since }` — illuminated; expires strictly after `max_on_ms`.
//! - `Cooldown { since }` — dark, waiting out the cooldown (default 300 ms)
//!   before the next on-window.
//! - `Off` — not cycling (never activated, deactivated by stop/drop, or a
//!   non-activatable device such as always-on hardware).
//!
//! All methods take `now: Instant` from the caller, so the machine is fully
//! deterministic under a mock clock and holds no clock of its own.
//!
//! Two invariants carry the safety and the fix:
//!
//! 1. **The on-budget never resets while already on.** Re-arming or a
//!    verify-readiness request during an on-window is a no-op; a fresh
//!    budget starts only after a force-off plus a cooldown (or an explicit
//!    verification window entry, which is a deliberate, user-initiated
//!    transition).
//! 2. **Expiry is re-evaluated every tick.** A force-off write that fails
//!    leaves the phase `On`, so the next tick returns [`CycleAction::Expire`]
//!    again and the caller retries — the watchdog never switches itself off.

use std::time::{Duration, Instant};

/// Default off-window between on-windows, in milliseconds.
pub const DEFAULT_COOLDOWN_MS: u32 = 300;

/// Default continuous-on budget, in milliseconds (spec §4.10.2; quirks may
/// override via `[emitter.lifecycle] max_on_ms`).
pub const DEFAULT_MAX_ON_MS: u32 = 2000;

/// Which phase of the duty cycle the emitter is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CyclePhase {
    /// Not cycling: never activated, or deactivated by stop/drop.
    Off,
    /// Illuminated; the continuous-on budget is running.
    On,
    /// Dark; waiting out the cooldown before the next on-window.
    Cooldown,
}

/// What [`DutyCycler::tick`] asks the caller to do. The caller performs the
/// device write and reports the outcome back through [`DutyCycler::activate`]
/// or [`DutyCycler::force_off`]; the machine never touches the device itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleAction {
    /// Nothing to do.
    None,
    /// The continuous-on budget has expired. The caller must force the
    /// emitter off; on success it records the transition with
    /// [`DutyCycler::force_off`]. On failure the phase stays `On`, so the
    /// next tick returns `Expire` again and the write is retried.
    Expire,
    /// The cooldown has elapsed. The caller may turn the emitter on; on
    /// success it records the transition with [`DutyCycler::activate`]. On
    /// failure the phase stays `Cooldown`, so the next tick returns `Rearm`
    /// again and the write is retried.
    Rearm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CycleState {
    Off,
    On { since: Instant },
    Cooldown { since: Instant },
}

/// The duty-cycle state machine. Pure: all time comes in as parameters.
#[derive(Debug, Clone, Copy)]
pub struct DutyCycler {
    max_on_ms: u32,
    cooldown_ms: u32,
    state: CycleState,
}

impl DutyCycler {
    pub fn new(max_on_ms: u32, cooldown_ms: u32) -> Self {
        Self {
            max_on_ms,
            cooldown_ms,
            state: CycleState::Off,
        }
    }

    /// Configured continuous-on budget, for transition diagnostics.
    pub fn max_on_ms(&self) -> u32 {
        self.max_on_ms
    }

    /// Configured cooldown, for transition diagnostics.
    pub fn cooldown_ms(&self) -> u32 {
        self.cooldown_ms
    }

    fn max_on(&self) -> Duration {
        Duration::from_millis(u64::from(self.max_on_ms))
    }

    fn cooldown(&self) -> Duration {
        Duration::from_millis(u64::from(self.cooldown_ms))
    }

    pub fn phase(&self) -> CyclePhase {
        match self.state {
            CycleState::Off => CyclePhase::Off,
            CycleState::On { .. } => CyclePhase::On,
            CycleState::Cooldown { .. } => CyclePhase::Cooldown,
        }
    }

    /// Time the emitter has been on in the current window, if it is on.
    pub fn on_elapsed(&self, now: Instant) -> Option<Duration> {
        match self.state {
            CycleState::On { since } => Some(now.saturating_duration_since(since)),
            _ => None,
        }
    }

    /// Record a successful turn-on with a fresh budget.
    ///
    /// A no-op while already `On`: the live budget is retained and never
    /// resets while the emitter is illuminated.
    pub fn activate(&mut self, now: Instant) {
        if matches!(self.state, CycleState::On { .. }) {
            return;
        }
        self.state = CycleState::On { since: now };
    }

    /// Record a successful force-off: `On` → `Cooldown`. A no-op when not
    /// `On` — repeated force-offs (e.g. retried writes) never extend or
    /// restart the cooldown.
    pub fn force_off(&mut self, now: Instant) {
        if matches!(self.state, CycleState::On { .. }) {
            self.state = CycleState::Cooldown { since: now };
        }
    }

    /// Record that the emitter is fully off and must stay off until an
    /// explicit re-arm: stop/drop. The next [`Self::activate`] starts a
    /// fresh budget with no cooldown wait.
    pub fn deactivate(&mut self) {
        self.state = CycleState::Off;
    }

    /// Enter the cooldown phase without a device write. Used when a turn-on
    /// attempt failed at a point where retries should pace themselves by the
    /// cooldown rather than hammering the device per frame.
    pub fn enter_cooldown(&mut self, now: Instant) {
        self.state = CycleState::Cooldown { since: now };
    }

    /// Evaluate the machine at `now`.
    pub fn tick(&mut self, now: Instant) -> CycleAction {
        match self.state {
            CycleState::Off => CycleAction::None,
            // Strictly after: a window of exactly max_on_ms is still legal.
            CycleState::On { since } => {
                if now.saturating_duration_since(since) > self.max_on() {
                    CycleAction::Expire
                } else {
                    CycleAction::None
                }
            }
            // Inclusive: a cooldown of exactly cooldown_ms has elapsed.
            CycleState::Cooldown { since } => {
                if now.saturating_duration_since(since) >= self.cooldown() {
                    CycleAction::Rearm
                } else {
                    CycleAction::None
                }
            }
        }
    }

    /// Verification hook: guarantee an illuminated window.
    ///
    /// - `On` → no-op, returns `false`. **The live on-budget is retained** —
    ///   a verify landing mid-window must not restart the 2 s cap.
    /// - `Cooldown` or `Off` → turn on immediately with a fresh budget,
    ///   returns `true`. The remaining cooldown is skipped on purpose: this
    ///   is a user-initiated authentication, and determinism (verify frames
    ///   are never captured in a dark window) outranks the short off-window.
    ///
    /// Returns whether a transition to `On` was made.
    pub fn ensure_on(&mut self, now: Instant) -> bool {
        if matches!(self.state, CycleState::On { .. }) {
            return false;
        }
        self.activate(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed epoch so every Instant in this module is deterministic.
    fn epoch() -> Instant {
        // A real Instant (monotonic base) is required; relative arithmetic
        // below keeps everything deterministic regardless of the base.
        Instant::now()
    }

    fn ms(m: u64) -> Duration {
        Duration::from_millis(m)
    }

    fn cycler(max_on_ms: u64, cooldown_ms: u64) -> DutyCycler {
        DutyCycler::new(max_on_ms as u32, cooldown_ms as u32)
    }

    #[test]
    fn budget_expires_strictly_after_max_on() {
        let t0 = epoch();
        let mut c = cycler(2000, 300);
        c.activate(t0);
        assert_eq!(c.phase(), CyclePhase::On);

        assert_eq!(c.tick(t0 + ms(1999)), CycleAction::None, "1999 < 2000");
        assert_eq!(
            c.tick(t0 + ms(2000)),
            CycleAction::None,
            "expiry is strictly after max_on, not at it"
        );
        assert_eq!(c.tick(t0 + ms(2001)), CycleAction::Expire);
    }

    #[test]
    fn cooldown_elapses_before_rearm() {
        let t0 = epoch();
        let mut c = cycler(2000, 300);
        c.activate(t0);
        c.force_off(t0 + ms(2000));
        assert_eq!(c.phase(), CyclePhase::Cooldown);

        assert_eq!(c.tick(t0 + ms(2299)), CycleAction::None, "299 < 300");
        assert_eq!(c.tick(t0 + ms(2300)), CycleAction::Rearm, "300 >= 300");
    }

    /// The core safety invariant: re-arming (or any call that would turn the
    /// emitter on) while already on must NOT restart the continuous-on
    /// budget. Otherwise a chatty caller could keep the emitter lit forever.
    #[test]
    fn activate_while_on_does_not_reset_budget() {
        let t0 = epoch();
        let mut c = cycler(2000, 300);
        c.activate(t0);
        c.activate(t0 + ms(1500));
        c.activate(t0 + ms(1990));

        assert_eq!(
            c.tick(t0 + ms(2001)),
            CycleAction::Expire,
            "budget still anchored at t0 despite re-activate calls"
        );
    }

    /// Regression for issue #30: a 10-second session must duty-cycle, not go
    /// permanently dark after the first 2 s. Simulated frame pulls every
    /// 33 ms (30 fps); on Expire the caller forces off, on Rearm it turns on.
    #[test]
    fn ten_second_session_duty_cycles_instead_of_going_dark() {
        let t0 = epoch();
        let mut c = cycler(2000, 300);
        c.activate(t0);

        let mut expires = 0u32;
        let mut rearms = 0u32;
        let mut last_rearm_ms = 0u64;
        let mut on_since_ms = 0u64;
        let mut longest_on_ms = 0u64;

        for step in 0..=303 {
            let t = t0 + ms(33 * step);
            match c.tick(t) {
                CycleAction::None => {}
                CycleAction::Expire => {
                    expires += 1;
                    longest_on_ms = longest_on_ms.max(33 * step - on_since_ms);
                    on_since_ms = 0;
                    c.force_off(t);
                }
                CycleAction::Rearm => {
                    rearms += 1;
                    last_rearm_ms = 33 * step;
                    on_since_ms = 33 * step;
                    c.activate(t);
                }
            }
        }

        assert!(
            expires >= 3,
            "10 s with a 2 s budget must expire at least 3 times, got {expires}"
        );
        assert!(
            rearms >= 3,
            "the emitter must come back after each expiry, got {rearms} re-arms"
        );
        assert!(
            last_rearm_ms >= 9000,
            "the emitter must be illuminated again inside the final second (issue #30: \
             it went dark forever after 2 s); last re-arm at {last_rearm_ms} ms"
        );
        assert!(
            longest_on_ms <= 2000 + 33,
            "every on-window respects max_on (+ one frame interval for detection): \
             {longest_on_ms} ms"
        );
    }

    #[test]
    fn on_elapsed_reports_live_on_time() {
        let t0 = epoch();
        let mut c = cycler(2000, 300);
        assert_eq!(c.on_elapsed(t0), None, "not on yet");
        c.activate(t0);
        assert_eq!(c.on_elapsed(t0 + ms(500)), Some(ms(500)));
        c.force_off(t0 + ms(600));
        assert_eq!(c.on_elapsed(t0 + ms(700)), None);
    }

    /// The verification hook must never reset a live budget.
    #[test]
    fn ensure_on_retains_live_budget() {
        let t0 = epoch();
        let mut c = cycler(2000, 300);
        c.activate(t0);

        assert!(
            !c.ensure_on(t0 + ms(1500)),
            "already on: no transition, budget retained"
        );
        assert_eq!(c.phase(), CyclePhase::On);
        assert_eq!(
            c.tick(t0 + ms(2001)),
            CycleAction::Expire,
            "budget anchored at the original activation, not the ensure_on"
        );
    }

    #[test]
    fn ensure_on_from_cooldown_or_off_turns_on_with_fresh_budget() {
        let t0 = epoch();
        let mut c = cycler(2000, 300);
        c.activate(t0);
        c.force_off(t0 + ms(2000));

        assert!(c.ensure_on(t0 + ms(2050)), "cooldown skipped deliberately");
        assert_eq!(c.phase(), CyclePhase::On);
        assert_eq!(c.tick(t0 + ms(2050) + ms(1999)), CycleAction::None);
        assert_eq!(
            c.tick(t0 + ms(2050) + ms(2001)),
            CycleAction::Expire,
            "fresh budget anchored at the ensure_on"
        );

        c.force_off(t0 + ms(2050) + ms(2001));
        c.deactivate();
        assert_eq!(c.phase(), CyclePhase::Off);
        assert!(c.ensure_on(t0 + ms(5000)));
        assert_eq!(c.phase(), CyclePhase::On);
    }

    #[test]
    fn deactivate_then_activate_starts_a_fresh_budget_without_cooldown() {
        let t0 = epoch();
        let mut c = cycler(2000, 300);
        c.activate(t0);
        c.force_off(t0 + ms(1000));
        c.deactivate();
        assert_eq!(c.phase(), CyclePhase::Off);

        let t2 = t0 + ms(4000);
        c.activate(t2);
        assert_eq!(c.phase(), CyclePhase::On);
        assert_eq!(c.tick(t2 + ms(1999)), CycleAction::None);
        assert_eq!(c.tick(t2 + ms(2001)), CycleAction::Expire);
    }

    /// Repeated force-offs must not extend or restart the cooldown.
    #[test]
    fn repeated_force_offs_do_not_restart_cooldown() {
        let t0 = epoch();
        let mut c = cycler(2000, 300);
        c.activate(t0);
        c.force_off(t0 + ms(2000));
        c.force_off(t0 + ms(2100));
        c.force_off(t0 + ms(2200));

        assert_eq!(
            c.tick(t0 + ms(2300)),
            CycleAction::Rearm,
            "cooldown anchored at the first force-off"
        );
    }

    #[test]
    fn force_off_and_deactivate_when_off_are_noops() {
        let t0 = epoch();
        let mut c = cycler(2000, 300);
        c.force_off(t0);
        c.deactivate();
        assert_eq!(c.phase(), CyclePhase::Off);
        assert_eq!(c.tick(t0 + ms(5000)), CycleAction::None);
    }

    /// A turn-on failure enters cooldown so retries pace themselves; the
    /// cooldown then elapses into a Rearm normally.
    #[test]
    fn enter_cooldown_paces_retry_until_rearm() {
        let t0 = epoch();
        let mut c = cycler(2000, 300);
        c.enter_cooldown(t0);
        assert_eq!(c.phase(), CyclePhase::Cooldown);
        assert_eq!(c.tick(t0 + ms(299)), CycleAction::None);
        assert_eq!(c.tick(t0 + ms(300)), CycleAction::Rearm);
    }

    /// Default constants match the plan: 300 ms cooldown, 2000 ms on-budget.
    #[test]
    fn defaults_match_the_plan() {
        assert_eq!(DEFAULT_COOLDOWN_MS, 300);
        assert_eq!(DEFAULT_MAX_ON_MS, 2000);
    }
}
