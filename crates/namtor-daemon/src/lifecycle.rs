// SPDX-License-Identifier: GPL-3.0-only
//! Warm-models / cold-camera lifecycle (spec §5.5).
//!
//! The daemon starts at boot, pre-loads models into a warm worker, and opens
//! NO camera. `PrepareForAuth` opens the stream and applies the emitter mode;
//! `VerifyStart` scores arriving frames; `Release` closes the stream and
//! turns the emitter off. A warm stream left without `VerifyStart` for
//! `warm_timeout` (default 30 s) is auto-released.

use std::sync::Arc;
use std::time::{Duration, Instant};

use namtor_proto::pam::{Requester, StatusReply, StreamState};

use crate::DaemonError;

/// Lifecycle phase, mirroring [`namtor_proto::pam::AuthPhase`] but owned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LcPhase {
    Cold,
    Warming,
    Verifying,
}

/// The lifecycle state machine. `C` is the clock (real or mock).
pub struct Lifecycle<C: Clock> {
    phase: LcPhase,
    since: Instant,
    requester: Option<Requester>,
    warm_timeout: Duration,
    clock: C,
}

/// Clock seam for the lifecycle (mirrors `namtor_camera::watchdog::Clock` but
/// kept independent so the daemon doesn't depend on `namtor-camera` for this
/// trait — it depends on it for the emitter/capture types).
pub trait Clock {
    fn now(&self) -> Instant;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Debug, Clone)]
pub struct MockClock {
    t: Arc<std::sync::Mutex<Instant>>,
}

impl MockClock {
    pub fn new(start: Instant) -> Self {
        Self {
            t: Arc::new(std::sync::Mutex::new(start)),
        }
    }
    pub fn advance(&self, d: Duration) {
        let mut t = self.t.lock().expect("clock");
        *t += d;
    }
}

impl Clock for MockClock {
    fn now(&self) -> Instant {
        *self.t.lock().expect("clock")
    }
}

impl<C: Clock> Lifecycle<C> {
    pub fn new(clock: C, warm_timeout: Duration) -> Self {
        Self {
            phase: LcPhase::Cold,
            since: clock.now(),
            requester: None,
            warm_timeout,
            clock,
        }
    }

    pub fn phase(&self) -> LcPhase {
        self.phase
    }

    /// Open the stream, apply emitter mode, let AE settle (spec §5.5.2).
    pub fn prepare_for_auth(&mut self, req: Requester) -> Result<(), DaemonError> {
        match self.phase {
            LcPhase::Cold => {
                self.phase = LcPhase::Warming;
                self.since = self.clock.now();
                self.requester = Some(req);
                Ok(())
            }
            LcPhase::Warming => {
                self.requester = Some(req);
                Ok(())
            }
            LcPhase::Verifying => Err(DaemonError::State("cannot prepare while verifying".into())),
        }
    }

    /// Start scoring arriving frames (spec §5.5.2).
    pub fn verify_start(&mut self, req: Requester) -> Result<(), DaemonError> {
        match self.phase {
            LcPhase::Warming => {
                self.phase = LcPhase::Verifying;
                self.since = self.clock.now();
                self.requester = Some(req);
                Ok(())
            }
            LcPhase::Verifying => Ok(()),
            LcPhase::Cold => Err(DaemonError::State("cannot verify without prepare".into())),
        }
    }

    /// Close the stream, emitter off (spec §5.5.2).
    pub fn release(&mut self) -> Result<(), DaemonError> {
        self.phase = LcPhase::Cold;
        self.since = self.clock.now();
        self.requester = None;
        Ok(())
    }

    /// Tick: auto-release a warm stream past `warm_timeout` (spec §5.5.2).
    pub fn tick(&mut self) {
        if self.phase == LcPhase::Warming {
            let elapsed = self.clock.now().duration_since(self.since);
            if elapsed >= self.warm_timeout {
                self.phase = LcPhase::Cold;
                self.requester = None;
                self.since = self.clock.now();
            }
        }
    }

    /// Remaining warm time in milliseconds, if warming.
    pub fn warm_remaining_ms(&self) -> Option<u64> {
        if self.phase == LcPhase::Warming {
            let elapsed = self.clock.now().duration_since(self.since);
            let remaining = self.warm_timeout.saturating_sub(elapsed);
            Some(remaining.as_millis() as u64)
        } else {
            None
        }
    }

    /// Status snapshot for `namtor-cli status` / `AuthRequest::Status`.
    pub fn snapshot(&self) -> StatusReply {
        let stream = match self.phase {
            LcPhase::Cold => StreamState::Closed,
            LcPhase::Warming => StreamState::Warming,
            LcPhase::Verifying => StreamState::Verifying,
        };
        StatusReply {
            stream,
            requester: self.requester.clone(),
            warm_remaining_ms: self.warm_remaining_ms(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requester() -> Requester {
        Requester {
            uid: 1000,
            label: "gdm".into(),
        }
    }

    #[test]
    fn cold_to_warming_on_prepare() {
        let clock = MockClock::new(Instant::now());
        let mut lc = Lifecycle::new(clock, Duration::from_secs(30));
        assert_eq!(lc.phase(), LcPhase::Cold);
        lc.prepare_for_auth(requester()).unwrap();
        assert_eq!(lc.phase(), LcPhase::Warming);
        assert!(lc.warm_remaining_ms().is_some());
    }

    #[test]
    fn warming_to_verifying() {
        let clock = MockClock::new(Instant::now());
        let mut lc = Lifecycle::new(clock, Duration::from_secs(30));
        lc.prepare_for_auth(requester()).unwrap();
        lc.verify_start(requester()).unwrap();
        assert_eq!(lc.phase(), LcPhase::Verifying);
    }

    #[test]
    fn verifying_to_cold_on_release() {
        let clock = MockClock::new(Instant::now());
        let mut lc = Lifecycle::new(clock, Duration::from_secs(30));
        lc.prepare_for_auth(requester()).unwrap();
        lc.verify_start(requester()).unwrap();
        lc.release().unwrap();
        assert_eq!(lc.phase(), LcPhase::Cold);
        assert!(lc.warm_remaining_ms().is_none());
    }

    #[test]
    fn verify_without_prepare_errors() {
        let clock = MockClock::new(Instant::now());
        let mut lc = Lifecycle::new(clock, Duration::from_secs(30));
        let err = lc.verify_start(requester()).unwrap_err();
        assert!(matches!(err, DaemonError::State(_)));
    }

    #[test]
    fn prepare_while_verifying_errors() {
        let clock = MockClock::new(Instant::now());
        let mut lc = Lifecycle::new(clock, Duration::from_secs(30));
        lc.prepare_for_auth(requester()).unwrap();
        lc.verify_start(requester()).unwrap();
        let err = lc.prepare_for_auth(requester()).unwrap_err();
        assert!(matches!(err, DaemonError::State(_)));
    }

    #[test]
    fn warm_timeout_auto_releases() {
        let clock = MockClock::new(Instant::now());
        let mut lc = Lifecycle::new(clock.clone(), Duration::from_secs(5));
        lc.prepare_for_auth(requester()).unwrap();
        assert_eq!(lc.phase(), LcPhase::Warming);

        clock.advance(Duration::from_secs(4));
        lc.tick();
        assert_eq!(lc.phase(), LcPhase::Warming, "4s < 5s timeout");

        clock.advance(Duration::from_secs(2));
        lc.tick();
        assert_eq!(lc.phase(), LcPhase::Cold, "6s > 5s timeout → auto release");
    }

    #[test]
    fn custom_warm_timeout_honoured() {
        let clock = MockClock::new(Instant::now());
        let mut lc = Lifecycle::new(clock.clone(), Duration::from_millis(500));
        lc.prepare_for_auth(requester()).unwrap();
        clock.advance(Duration::from_millis(600));
        lc.tick();
        assert_eq!(lc.phase(), LcPhase::Cold);
    }

    #[test]
    fn snapshot_reflects_state() {
        let clock = MockClock::new(Instant::now());
        let mut lc = Lifecycle::new(clock.clone(), Duration::from_secs(30));

        let s = lc.snapshot();
        assert_eq!(s.stream, StreamState::Closed);
        assert!(s.requester.is_none());

        lc.prepare_for_auth(requester()).unwrap();
        let s = lc.snapshot();
        assert_eq!(s.stream, StreamState::Warming);
        assert_eq!(s.requester.as_ref().unwrap().uid, 1000);
        assert!(s.warm_remaining_ms.is_some());

        lc.verify_start(requester()).unwrap();
        let s = lc.snapshot();
        assert_eq!(s.stream, StreamState::Verifying);

        lc.release().unwrap();
        let s = lc.snapshot();
        assert_eq!(s.stream, StreamState::Closed);
    }
}
