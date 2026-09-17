// SPDX-License-Identifier: GPL-3.0-only
//! Daemon dispatcher: bridges `LifecycleDispatch` to the `Daemon` + `VerifyEngine`.
//!
//! Wraps both in a single `Mutex` so the dispatcher satisfies `Send + Sync`
//! (the listener holds it behind `Arc`). On `Verify`, captures frame pairs
//! from the daemon's capture source, runs the verify engine, maps the outcome
//! to an `AuthReply`.
//!
//! Per-uid backoff (spec §8.4): before running the verify engine the
//! dispatcher consults [`BackoffStore::is_locked`]. A locked uid is refused
//! with [`Verdict::RateLimited`] without consuming the camera or worker. A
//! `NoMatch` advances the per-uid schedule and persists it; a `Match` resets
//! it. `Unavailable`/`Refused` are infrastructure outcomes and do not move
//! the schedule (a glitching camera must not lock a user out).

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use namtor_capture::source::CaptureSource;
use namtor_proto::pam::{AuthBody, AuthPhase, AuthReply, Requester, StatusReply, Verdict};

use crate::backoff::BackoffStore;
use crate::daemon::Daemon;
use crate::lifecycle::Clock;
use crate::listener::LifecycleDispatch;
use crate::supervisor::WorkerSupervisor;
use crate::verify::{external_reason, VerifyEngine, VerifyOutcome};
use crate::DaemonError;

const VERIFY_TIMEOUT: Duration = Duration::from_secs(5);

/// Seam over [`VerifyEngine::verify`] so the dispatcher's backoff wiring can
/// be tested with a canned outcome instead of a real ONNX pipeline. The real
/// implementation is `impl VerifyRunner for VerifyEngine` below.
pub trait VerifyRunner: Send {
    fn run_verify(
        &mut self,
        capture: &mut dyn CaptureSource,
        target_uid: u32,
        deadline: Instant,
    ) -> VerifyOutcome;
}

impl VerifyRunner for VerifyEngine {
    fn run_verify(
        &mut self,
        capture: &mut dyn CaptureSource,
        target_uid: u32,
        deadline: Instant,
    ) -> VerifyOutcome {
        self.verify(capture, target_uid, deadline)
    }
}

struct DispatchState<S: WorkerSupervisor, C: Clock, V: VerifyRunner> {
    daemon: Daemon<S, C>,
    verify: V,
    backoff: BackoffStore,
    /// When `Some`, the store is persisted here after every mutation so a
    /// daemon restart does not forgive an in-progress bruteforce. `None` is
    /// in-memory only (unit tests that exercise the wiring without disk).
    backoff_path: Option<PathBuf>,
    clock: C,
}

pub struct DaemonDispatcher<S: WorkerSupervisor, C: Clock, V: VerifyRunner> {
    state: Mutex<DispatchState<S, C, V>>,
}

impl<S: WorkerSupervisor + Send, C: Clock + Send, V: VerifyRunner> DaemonDispatcher<S, C, V> {
    /// Close the auth capture and return the lifecycle to Cold, because the
    /// machine is about to suspend.
    ///
    /// The auth capture is opened by `prepare_for_auth` and stays open through
    /// the whole warm window — 30 s — until `release` or `tick` cools it. The
    /// daemon never listened for logind's `PrepareForSleep`, so closing a
    /// laptop lid right after an unlock suspended the machine with the IR
    /// emitter still armed. (The `ReconcileGuard` written to handle this was
    /// never constructed outside its own tests.) `Lifecycle::release` is
    /// infallible and forces Cold from any phase, so this is safe to call
    /// whatever the daemon was doing.
    pub fn release_for_suspend(&self) {
        let mut st = match self.state.lock() {
            Ok(g) => g,
            // A poisoned lock still guards a capture that must be closed.
            Err(p) => p.into_inner(),
        };
        let was_open = st.daemon.capture_open();
        if let Err(e) = st.daemon.release() {
            tracing::warn!(error = %e, "release before suspend failed");
        }
        if was_open {
            tracing::info!("auth capture closed for suspend; emitter off");
        }
    }

    /// Build a dispatcher over the given daemon, verify runner, and backoff
    /// store. `backoff_path` enables atomic disk persistence after each
    /// mutation; pass `None` for an in-memory store.
    pub fn new(
        daemon: Daemon<S, C>,
        verify: V,
        backoff: BackoffStore,
        backoff_path: Option<PathBuf>,
        clock: C,
    ) -> Self {
        Self {
            state: Mutex::new(DispatchState {
                daemon,
                verify,
                backoff,
                backoff_path,
                clock,
            }),
        }
    }
}

impl<S: WorkerSupervisor + Send, C: Clock + Send, V: VerifyRunner> LifecycleDispatch
    for DaemonDispatcher<S, C, V>
{
    fn dispatch(&self, body: &AuthBody) -> Result<AuthReply, DaemonError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| DaemonError::State("dispatcher mutex poisoned".into()))?;
        let req = Requester {
            uid: body.target_uid,
            label: "pam".into(),
        };

        match body.phase {
            AuthPhase::Prepare => {
                state.daemon.prepare_for_auth(req)?;
                Ok(AuthReply {
                    nonce: body.nonce,
                    verdict: Verdict::Unavailable,
                    score: 0.0,
                    reason: Some("prepare ack".into()),
                })
            }
            AuthPhase::Verify => {
                let now = state.clock.now();

                // Backoff gate (spec §8.4): refuse a locked uid before
                // touching the camera or worker. A probe during lockout does
                // NOT extend the schedule — otherwise an attacker polling
                // while locked could keep widening the window forever.
                if state.backoff.is_locked(body.target_uid, now) {
                    tracing::debug!(
                        target: "namtor::journal",
                        target_uid = body.target_uid,
                        remaining_ms = state.backoff.remaining_ms(body.target_uid, now),
                        "backoff active"
                    );
                    return Ok(AuthReply {
                        nonce: body.nonce,
                        verdict: Verdict::RateLimited,
                        score: 0.0,
                        reason: Some("backoff active".into()),
                    });
                }

                state.daemon.verify_start(req)?;
                let deadline = Instant::now() + VERIFY_TIMEOUT;
                let DispatchState {
                    daemon,
                    verify,
                    backoff,
                    backoff_path,
                    ..
                } = &mut *state;
                let outcome = if let Some(capture) = daemon.capture_mut() {
                    verify.run_verify(capture.as_mut(), body.target_uid, deadline)
                } else {
                    VerifyOutcome::unavailable("no capture source")
                };

                // Backoff bookkeeping: only genuine auth outcomes move the
                // schedule. Match resets; NoMatch advances. Unavailable/
                // Refused are infrastructure issues and must not lock out a
                // user whose camera glitched or whose rhost was set.
                let mut dirty = false;
                match outcome.verdict {
                    Verdict::Match => {
                        backoff.record_success(body.target_uid);
                        dirty = true;
                    }
                    Verdict::NoMatch => {
                        backoff.record_failure(body.target_uid, now);
                        dirty = true;
                    }
                    Verdict::Unavailable | Verdict::RateLimited | Verdict::Refused => {}
                }
                if dirty {
                    if let Some(p) = backoff_path.as_ref() {
                        if let Err(e) = backoff.save(p) {
                            tracing::warn!(error = %e, "backoff persist failed");
                        }
                    }
                }

                // Spec §8.3: the journal records the true reason at debug
                // level (gate name + scalar — not biometric data); the
                // AuthReply sent to PAM carries only the generic external
                // reason so a PAD veto is indistinguishable from NoMatch.
                tracing::debug!(
                    target: "namtor::journal",
                    target_uid = body.target_uid,
                    verdict = ?outcome.verdict,
                    score = outcome.score,
                    internal_reason = outcome.reason.as_deref().unwrap_or(""),
                    "verify outcome"
                );
                Ok(AuthReply {
                    nonce: body.nonce,
                    verdict: outcome.verdict,
                    score: outcome.score,
                    reason: external_reason(&outcome),
                })
            }
            AuthPhase::Release => {
                state.daemon.release()?;
                Ok(AuthReply {
                    nonce: body.nonce,
                    verdict: Verdict::Unavailable,
                    score: 0.0,
                    reason: Some("released".into()),
                })
            }
        }
    }

    fn snapshot(&self) -> StatusReply {
        let state = self.state.lock().expect("dispatcher mutex");
        state.daemon.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backoff::BackoffConfig;
    use crate::daemon::CaptureFactory;
    use crate::lifecycle::{LcPhase, MockClock};
    use crate::supervisor::MockSupervisor;
    use namtor_capture::frame::{Frame, PixFmt};
    use namtor_capture::source::{CaptureError, CaptureSource, SourceCaps, Spectrum};
    use namtor_proto::pam::AuthPhase;
    use namtor_proto::worker::{ModelSpec, Quant};
    use std::time::Instant;

    /// Test-only peek at the dispatcher's in-memory backoff remaining time.
    /// The lock window (`backoff_until`) is not persisted, so this is the only
    /// way to assert on the active lock from outside the dispatcher.
    fn backoff_remaining(
        disp: &DaemonDispatcher<MockSupervisor, MockClock, MockRunner>,
        uid: u32,
    ) -> u64 {
        let state = disp.state.lock().expect("dispatcher mutex");
        state.backoff.remaining_ms(uid, state.clock.now())
    }

    struct StubSource;
    impl CaptureSource for StubSource {
        fn caps(&self) -> SourceCaps {
            SourceCaps {
                spectrum: Spectrum::Ir,
                format: PixFmt::Grey8,
                width: 8,
                height: 8,
                fps: 30,
                has_metadata: false,
            }
        }
        fn start(&mut self) -> Result<(), CaptureError> {
            Ok(())
        }
        fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError> {
            Ok(None)
        }
        fn stop(&mut self) {}
    }

    /// Returns a canned outcome on every call, so the dispatcher's backoff
    /// wiring can be exercised without a real ONNX pipeline.
    struct MockRunner {
        outcome: VerifyOutcome,
    }
    impl VerifyRunner for MockRunner {
        fn run_verify(
            &mut self,
            _capture: &mut dyn CaptureSource,
            _target_uid: u32,
            _deadline: Instant,
        ) -> VerifyOutcome {
            self.outcome.clone()
        }
    }

    fn models() -> Vec<ModelSpec> {
        vec![ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        }]
    }

    fn capture_factory() -> CaptureFactory {
        Box::new(|| Ok(Box::new(StubSource) as Box<dyn CaptureSource>))
    }

    fn verifier(outcome: VerifyOutcome) -> MockRunner {
        MockRunner { outcome }
    }

    fn body(uid: u32, phase: AuthPhase) -> AuthBody {
        AuthBody {
            nonce: [0; 16],
            target_uid: uid,
            phase,
            rhost: None,
            deadline_us: 5_000_000,
        }
    }

    fn cfg_small_max() -> BackoffConfig {
        BackoffConfig {
            base_ms: 1000,
            max_ms: 4_000,
            multiplier: 2.0,
        }
    }

    type Disp = DaemonDispatcher<MockSupervisor, MockClock, MockRunner>;

    /// Drive a full Prepare→Verify→Release cycle for `uid`. Advances the mock
    /// clock past any active lock window first, so repeated attempts in the
    /// same test each reach the verify engine (mirrors a real user retrying
    /// after the lockout expires). Returns the Verify-phase reply.
    fn attempt(disp: &Disp, clock: &MockClock, uid: u32) -> AuthReply {
        clock.advance(Duration::from_secs(3600));
        disp.dispatch(&body(uid, AuthPhase::Prepare)).unwrap();
        let reply = disp.dispatch(&body(uid, AuthPhase::Verify)).unwrap();
        disp.dispatch(&body(uid, AuthPhase::Release)).unwrap();
        reply
    }

    /// Prepare→Verify→Release with no clock advance. Use when the target uid
    /// has no active lock and the test asserts on instants relative to the
    /// unadvanced clock.
    fn attempt_fresh(disp: &Disp, uid: u32) -> AuthReply {
        disp.dispatch(&body(uid, AuthPhase::Prepare)).unwrap();
        let reply = disp.dispatch(&body(uid, AuthPhase::Verify)).unwrap();
        disp.dispatch(&body(uid, AuthPhase::Release)).unwrap();
        reply
    }

    fn build(outcome: VerifyOutcome) -> (Disp, MockClock) {
        let sup = MockSupervisor::new();
        let clock = MockClock::new(Instant::now());
        let daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            clock.clone(),
            Some(capture_factory()),
        )
        .unwrap();
        let disp = DaemonDispatcher::new(
            daemon,
            verifier(outcome),
            BackoffStore::new(BackoffConfig::default()),
            None,
            clock.clone(),
        );
        (disp, clock)
    }

    // Given a Prepare request, when dispatched, then the daemon acks with
    // Unavailable (backoff wiring does not interfere with the non-verify
    // phases).
    #[test]
    fn prepare_dispatches_to_daemon() {
        let (disp, clock) = build(VerifyOutcome::no_match("none"));
        let reply = disp.dispatch(&body(1000, AuthPhase::Prepare)).unwrap();
        assert_eq!(reply.verdict, Verdict::Unavailable);
        let _ = clock;
    }

    // Given a locked uid, when a Verify is dispatched, then the reply is
    // RateLimited and the verify runner is never invoked.
    #[test]
    fn verify_returns_rate_limited_when_locked() {
        let sup = MockSupervisor::new();
        let clock = MockClock::new(Instant::now());
        let now = clock.now();
        let mut backoff = BackoffStore::new(BackoffConfig::default());
        backoff.record_failure(1000, now);
        let daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            clock.clone(),
            Some(capture_factory()),
        )
        .unwrap();
        // A canned Match proves the runner was NOT called: the gate refuses
        // before reaching verify, so RateLimited is returned instead.
        let disp = DaemonDispatcher::new(
            daemon,
            verifier(VerifyOutcome::match_(0.9)),
            backoff,
            None,
            clock.clone(),
        );
        let reply = disp.dispatch(&body(1000, AuthPhase::Verify)).unwrap();
        assert_eq!(reply.verdict, Verdict::RateLimited);
        assert_eq!(reply.reason.as_deref(), Some("backoff active"));
    }

    // Given a Verify that returns NoMatch, when dispatched, then the per-uid
    // backoff is recorded AND persisted to disk so a daemon restart does not
    // forgive the attempt.
    #[test]
    fn no_match_records_failure_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backoff.json");
        let sup = MockSupervisor::new();
        let clock = MockClock::new(Instant::now());
        let daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            clock.clone(),
            Some(capture_factory()),
        )
        .unwrap();
        let disp = DaemonDispatcher::new(
            daemon,
            verifier(VerifyOutcome::no_match("no face")),
            BackoffStore::new(BackoffConfig::default()),
            Some(path.clone()),
            clock.clone(),
        );
        let reply = attempt_fresh(&disp, 1000);
        assert_eq!(reply.verdict, Verdict::NoMatch);
        // Persisted: attempts == 1 for uid 1000 (the durable signal; the lock
        // window itself is process-local and not persisted).
        let loaded = BackoffStore::load(&path, BackoffConfig::default()).unwrap();
        assert_eq!(loaded.attempts(1000), 1);
        assert!(path.exists(), "backoff file must be persisted");
    }

    // Given a persisted backoff from a prior daemon run, when a new dispatcher
    // loads it and a further NoMatch arrives, then the schedule continues from
    // the persisted attempt count (restart does not reset the bruteforce
    // defence).
    #[test]
    fn restart_continues_schedule() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backoff.json");
        let clock = MockClock::new(Instant::now());

        // First daemon run: two failures -> attempts == 2 -> 4000ms.
        let d1 = build_with_path(VerifyOutcome::no_match("no face"), &path, clock.clone());
        attempt(&d1, &clock, 1000);
        attempt(&d1, &clock, 1000);

        // Second daemon run: load persisted store, one more failure -> attempts == 3 -> 8000ms.
        let loaded = BackoffStore::load(&path, BackoffConfig::default()).unwrap();
        let d2 = build_with_loaded(
            VerifyOutcome::no_match("no face"),
            loaded,
            &path,
            clock.clone(),
        );
        attempt(&d2, &clock, 1000);

        // The 3rd failure (attempts == 3) produces an 8000ms in-memory lock,
        // proving the schedule continued from the persisted count rather than
        // restarting at 2000ms.
        assert_eq!(backoff_remaining(&d2, 1000), 8_000);
        let final_store = BackoffStore::load(&path, BackoffConfig::default()).unwrap();
        assert_eq!(final_store.attempts(1000), 3);
    }

    // Given a locked uid, when a Verify returns Match (after the lock
    // expires), then the per-uid backoff is reset and the persisted store
    // drops the uid.
    #[test]
    fn match_resets_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backoff.json");
        let clock = MockClock::new(Instant::now());
        let now = clock.now();

        let mut backoff = BackoffStore::new(BackoffConfig::default());
        backoff.record_failure(1000, now);
        backoff.save(&path).unwrap();

        let sup = MockSupervisor::new();
        let daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            clock.clone(),
            Some(capture_factory()),
        )
        .unwrap();
        let disp = DaemonDispatcher::new(
            daemon,
            verifier(VerifyOutcome::match_(0.9)),
            backoff,
            Some(path.clone()),
            clock.clone(),
        );
        // `attempt` advances past the lock window so the gate lets verify run.
        let reply = attempt(&disp, &clock, 1000);
        assert_eq!(reply.verdict, Verdict::Match);

        let loaded = BackoffStore::load(&path, BackoffConfig::default()).unwrap();
        assert_eq!(loaded.attempts(1000), 0);
        assert!(!loaded.is_locked(1000, clock.now()));
    }

    // Given uid 1000 is locked, when a Verify for uid 1001 returns NoMatch,
    // then 1001 accrues its own failure while 1000's schedule is untouched
    // (backoff is per-uid, not global).
    #[test]
    fn per_uid_isolation_in_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backoff.json");
        let clock = MockClock::new(Instant::now());
        let now = clock.now();

        let mut backoff = BackoffStore::new(BackoffConfig::default());
        backoff.record_failure(1000, now);
        backoff.record_failure(1000, now); // 1000: attempts == 2 -> 4000ms
        backoff.save(&path).unwrap();

        let sup = MockSupervisor::new();
        let daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            clock.clone(),
            Some(capture_factory()),
        )
        .unwrap();
        let disp = DaemonDispatcher::new(
            daemon,
            verifier(VerifyOutcome::no_match("no face")),
            backoff,
            Some(path.clone()),
            clock.clone(),
        );
        // 1001 fails — must NOT affect 1000. No clock advance: assert against
        // the original `now` so both uids' lock windows are comparable.
        let reply = attempt_fresh(&disp, 1001);
        assert_eq!(reply.verdict, Verdict::NoMatch);

        let loaded = BackoffStore::load(&path, BackoffConfig::default()).unwrap();
        assert_eq!(loaded.attempts(1000), 2);
        assert_eq!(loaded.attempts(1001), 1);
    }

    // Given many successive NoMatches, when the delay is observed, then it is
    // bounded by the configured max lockout.
    #[test]
    fn max_lockout_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backoff.json");
        let clock = MockClock::new(Instant::now());

        let sup = MockSupervisor::new();
        let daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            clock.clone(),
            Some(capture_factory()),
        )
        .unwrap();
        let disp = DaemonDispatcher::new(
            daemon,
            verifier(VerifyOutcome::no_match("no face")),
            BackoffStore::new(cfg_small_max()),
            Some(path.clone()),
            clock.clone(),
        );
        for _ in 0..8 {
            attempt(&disp, &clock, 1000);
        }
        let remaining = backoff_remaining(&disp, 1000);
        assert!(
            remaining <= 4_000,
            "lockout must be bounded by max_ms (got {remaining})"
        );
        assert!(remaining > 0, "a failed attempt must produce a lock");
    }

    // Given a Verify that returns Unavailable, when dispatched, then the
    // backoff schedule does NOT advance (infrastructure failure must not lock
    // out a user).
    #[test]
    fn unavailable_does_not_advance_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backoff.json");
        let clock = MockClock::new(Instant::now());

        let sup = MockSupervisor::new();
        let daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            clock.clone(),
            Some(capture_factory()),
        )
        .unwrap();
        let disp = DaemonDispatcher::new(
            daemon,
            verifier(VerifyOutcome::unavailable("store error")),
            BackoffStore::new(BackoffConfig::default()),
            Some(path.clone()),
            clock.clone(),
        );
        let reply = attempt_fresh(&disp, 1000);
        assert_eq!(reply.verdict, Verdict::Unavailable);
        let loaded = BackoffStore::load(&path, BackoffConfig::default()).unwrap();
        assert_eq!(loaded.remaining_ms(1000, clock.now()), 0);
        assert!(!loaded.is_locked(1000, clock.now()));
    }

    // Smoke: a Prepare drives the daemon to Warming (backoff wiring is
    // verify-only).
    #[test]
    fn prepare_advances_to_warming() {
        let sup = MockSupervisor::new();
        let clock = MockClock::new(Instant::now());
        let mut daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            clock,
            Some(capture_factory()),
        )
        .unwrap();
        let req = Requester {
            uid: 1000,
            label: "pam".into(),
        };
        daemon.prepare_for_auth(req).unwrap();
        assert_eq!(daemon.phase(), LcPhase::Warming);
    }

    /// Build a dispatcher backed by a fresh on-disk backoff store at `path`.
    fn build_with_path(outcome: VerifyOutcome, path: &std::path::Path, clock: MockClock) -> Disp {
        let sup = MockSupervisor::new();
        let daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            clock.clone(),
            Some(capture_factory()),
        )
        .unwrap();
        DaemonDispatcher::new(
            daemon,
            verifier(outcome),
            BackoffStore::new(BackoffConfig::default()),
            Some(path.to_path_buf()),
            clock,
        )
    }

    /// Build a dispatcher backed by a pre-loaded backoff store persisted at
    /// `path`.
    fn build_with_loaded(
        outcome: VerifyOutcome,
        loaded: BackoffStore,
        path: &std::path::Path,
        clock: MockClock,
    ) -> Disp {
        let sup = MockSupervisor::new();
        let daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            clock.clone(),
            Some(capture_factory()),
        )
        .unwrap();
        DaemonDispatcher::new(
            daemon,
            verifier(outcome),
            loaded,
            Some(path.to_path_buf()),
            clock,
        )
    }

    // --- suspend -------------------------------------------------------------

    /// A suspend during the warm window must close the auth capture and return
    /// the lifecycle to Cold.
    ///
    /// The capture stays open for up to 30 s after `prepare_for_auth`, and the
    /// daemon never listened for `PrepareForSleep`, so closing a lid right
    /// after an unlock suspended the machine with the IR emitter armed.
    #[test]
    fn release_for_suspend_closes_a_warm_capture() {
        let (disp, _clock) = build(VerifyOutcome::no_match("none"));
        {
            let mut st = disp.state.lock().unwrap();
            st.daemon
                .prepare_for_auth(Requester {
                    uid: 1000,
                    label: "pam".into(),
                })
                .unwrap();
            assert!(st.daemon.capture_open(), "prepare opens the capture");
            assert_eq!(st.daemon.phase(), LcPhase::Warming);
        }

        disp.release_for_suspend();

        let st = disp.state.lock().unwrap();
        assert!(!st.daemon.capture_open(), "suspend must close the capture");
        assert_eq!(st.daemon.phase(), LcPhase::Cold);
    }

    /// Mid-verification is the worst case, and must be handled the same way.
    #[test]
    fn release_for_suspend_closes_a_capture_mid_verification() {
        let (disp, _clock) = build(VerifyOutcome::no_match("none"));
        {
            let mut st = disp.state.lock().unwrap();
            let req = Requester {
                uid: 1000,
                label: "pam".into(),
            };
            st.daemon.prepare_for_auth(req.clone()).unwrap();
            st.daemon.verify_start(req).unwrap();
            assert_eq!(st.daemon.phase(), LcPhase::Verifying);
        }
        disp.release_for_suspend();
        let st = disp.state.lock().unwrap();
        assert!(!st.daemon.capture_open());
        assert_eq!(st.daemon.phase(), LcPhase::Cold);
    }

    /// With nothing open, a suspend is a harmless no-op.
    #[test]
    fn release_for_suspend_with_no_capture_is_a_noop() {
        let (disp, _clock) = build(VerifyOutcome::no_match("none"));
        disp.release_for_suspend();
        let st = disp.state.lock().unwrap();
        assert!(!st.daemon.capture_open());
        assert_eq!(st.daemon.phase(), LcPhase::Cold);
    }
}
