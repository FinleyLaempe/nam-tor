// SPDX-License-Identifier: GPL-3.0-only
//! Resume/suspend state machine and logind `PrepareForSleep` subscription.
//!
//! Spec §4.10.3: on resume the daemon must re-detect the device, reopen it,
//! and re-apply the emitter mode before servicing auth requests. The state
//! machine is testable without D-Bus by injecting [`SleepEvent`]s directly;
//! the live [`ResumeMonitor`] wires it to logind's `PrepareForSleep` signal.

use std::path::PathBuf;

/// Suspend/resume events the state machine consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepEvent {
    Suspend,
    Resume,
}

/// Resume state. `Reconciling` queues auth requests; `Ready` services them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeState {
    Ready,
    Reconciling,
}

/// Callback invoked when the machine transitions into `Reconciling` (on
/// resume) so the owner can re-detect the device and re-apply the emitter.
pub trait SleepSignal {
    fn on_resume(&self);
}

/// Pure state machine. No I/O; the caller feeds [`SleepEvent`]s and observes
/// the resulting [`ResumeState`].
pub struct ResumeMachine {
    state: ResumeState,
}

impl ResumeMachine {
    pub fn new() -> Self {
        Self {
            state: ResumeState::Ready,
        }
    }

    pub fn state(&self) -> ResumeState {
        self.state
    }

    /// Feeds one event. On `Resume`, transitions to `Reconciling` and fires
    /// `on_resume` on the signal. The owner then calls `mark_ready` once
    /// re-detection and emitter re-apply complete.
    pub fn handle(&mut self, event: SleepEvent, signal: &dyn SleepSignal) {
        match event {
            SleepEvent::Suspend => {
                self.state = ResumeState::Reconciling;
            }
            SleepEvent::Resume => {
                self.state = ResumeState::Reconciling;
                signal.on_resume();
            }
        }
    }

    /// Marks reconciliation complete: the device is back and the emitter is
    /// re-applied. Queued requests can now be drained.
    pub fn mark_ready(&mut self) {
        self.state = ResumeState::Ready;
    }
}

impl Default for ResumeMachine {
    fn default() -> Self {
        Self::new()
    }
}

/// An auth request that may need to wait for reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub user: String,
}

/// Outcome of queueing a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingState {
    /// Request was serviced immediately (state was `Ready`).
    Serviced,
    /// Request was held; will be drained once reconciliation completes.
    Reconciling,
    /// Queue is full; the request was dropped (caller must fail closed).
    Dropped,
}

/// Result of a post-resume re-discovery, recorded by the owner and exposed
/// via [`Reconciler::last_outcome`] so the daemon can decide whether to
/// re-open on the same path, re-select after a renumber, or fall back to
/// password (camera missing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// Camera re-discovered at `path` (may differ from the pre-suspend path).
    Rediscovered { path: PathBuf },
    /// Camera re-discovered but its node path changed (device renumbered).
    StaleNode { old: PathBuf, new: PathBuf },
    /// No camera found after resume — auth must fall back to password.
    MissingCamera,
}

/// Default bound on the pending-request queue. Spec §4.10.3: the queue is
/// bounded so a stuck reconciliation cannot accumulate unbounded work.
pub const DEFAULT_MAX_PENDING: usize = 16;

/// Owns the resume state machine plus a queue of auth requests. On resume,
/// re-runs the reconcile callback (re-detect device, re-apply emitter), then
/// drains queued requests once `mark_ready` is called.
pub struct Reconciler<F: FnMut()> {
    machine: ResumeMachine,
    pending: Vec<Request>,
    max_pending: usize,
    on_reconcile: F,
    last_outcome: Option<ReconcileOutcome>,
}

impl<F: FnMut()> Reconciler<F> {
    pub fn new(on_reconcile: F) -> Self {
        Self::new_with_capacity(on_reconcile, DEFAULT_MAX_PENDING)
    }

    pub fn new_with_capacity(on_reconcile: F, max_pending: usize) -> Self {
        Self {
            machine: ResumeMachine::new(),
            pending: Vec::new(),
            max_pending,
            on_reconcile,
            last_outcome: None,
        }
    }

    pub fn state(&self) -> ResumeState {
        self.machine.state()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// The most recent re-discovery outcome, if [`handle`](Self::handle) has
    /// fired the reconcile callback at least once.
    pub fn last_outcome(&self) -> Option<&ReconcileOutcome> {
        self.last_outcome.as_ref()
    }

    /// Records the result of a post-resume re-discovery so the daemon can
    /// expose it via [`last_outcome`](Self::last_outcome).
    pub fn record_rediscovery(&mut self, outcome: ReconcileOutcome) {
        self.last_outcome = Some(outcome);
    }

    /// Feeds a sleep event. On `Resume`, transitions to `Reconciling` and
    /// fires the reconcile callback directly (re-detect + re-apply emitter).
    pub fn handle(&mut self, event: SleepEvent) {
        self.machine.handle(event, &NoopSignal);
        if event == SleepEvent::Resume && self.machine.state() == ResumeState::Reconciling {
            (self.on_reconcile)();
        }
    }

    /// Queues a request. Returns `Serviced` if the machine is `Ready`,
    /// `Reconciling` if the request is held, or `Dropped` if the bounded
    /// queue is full (caller must fail closed).
    pub fn queue_request(&mut self, req: Request) -> PendingState {
        match self.machine.state() {
            ResumeState::Ready => PendingState::Serviced,
            ResumeState::Reconciling => {
                if self.pending.len() >= self.max_pending {
                    PendingState::Dropped
                } else {
                    self.pending.push(req);
                    PendingState::Reconciling
                }
            }
        }
    }

    /// Marks reconciliation complete and returns the queued requests for the
    /// caller to service.
    pub fn drain_ready(&mut self) -> Vec<Request> {
        self.machine.mark_ready();
        std::mem::take(&mut self.pending)
    }
}

struct NoopSignal;
impl SleepSignal for NoopSignal {
    fn on_resume(&self) {}
}

/// Live logind `PrepareForSleep` subscription. The signal payload is a bool:
/// `true` = about to suspend, `false` = resumed.
pub struct ResumeMonitor;

impl ResumeMonitor {
    /// Subscribes to logind's `PrepareForSleep` and drives `machine` with the
    /// corresponding events. Blocks until the connection is lost. The
    /// `signal` is fired on each resume.
    pub fn run_loop(machine: &mut ResumeMachine, signal: &dyn SleepSignal) -> Result<(), String> {
        use zbus::blocking::Connection;

        let conn = Connection::system().map_err(|e| format!("connect to system bus: {e}"))?;

        let proxy = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )
        .map_err(|e| format!("create login1 proxy: {e}"))?;

        let signals = proxy
            .receive_signal("PrepareForSleep")
            .map_err(|e| format!("subscribe PrepareForSleep: {e}"))?;

        for msg in signals {
            let suspending: bool = msg
                .body()
                .deserialize::<bool>()
                .map_err(|e| format!("PrepareForSleep payload not bool: {e}"))?;
            let event = if suspending {
                SleepEvent::Suspend
            } else {
                SleepEvent::Resume
            };
            machine.handle(event, signal);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Recorder {
        calls: RefCell<u32>,
    }
    impl Recorder {
        fn new() -> Self {
            Self {
                calls: RefCell::new(0),
            }
        }
        fn calls(&self) -> u32 {
            *self.calls.borrow()
        }
    }
    impl SleepSignal for Recorder {
        fn on_resume(&self) {
            *self.calls.borrow_mut() += 1;
        }
    }

    #[test]
    fn initial_state_is_ready() {
        let m = ResumeMachine::new();
        assert_eq!(m.state(), ResumeState::Ready);
    }

    #[test]
    fn suspend_transitions_to_reconciling() {
        let mut m = ResumeMachine::new();
        let r = Recorder::new();
        m.handle(SleepEvent::Suspend, &r);
        assert_eq!(m.state(), ResumeState::Reconciling);
        assert_eq!(r.calls(), 0, "suspend must not fire on_resume");
    }

    #[test]
    fn resume_fires_on_resume_and_stays_reconciling() {
        let mut m = ResumeMachine::new();
        let r = Recorder::new();
        m.handle(SleepEvent::Resume, &r);
        assert_eq!(m.state(), ResumeState::Reconciling);
        assert_eq!(r.calls(), 1);
    }

    #[test]
    fn mark_ready_returns_to_ready() {
        let mut m = ResumeMachine::new();
        let r = Recorder::new();
        m.handle(SleepEvent::Suspend, &r);
        m.mark_ready();
        assert_eq!(m.state(), ResumeState::Ready);
    }

    #[test]
    fn full_suspend_resume_cycle() {
        let mut m = ResumeMachine::new();
        let r = Recorder::new();
        m.handle(SleepEvent::Suspend, &r);
        assert_eq!(m.state(), ResumeState::Reconciling);
        m.handle(SleepEvent::Resume, &r);
        assert_eq!(m.state(), ResumeState::Reconciling);
        assert_eq!(r.calls(), 1);
        m.mark_ready();
        assert_eq!(m.state(), ResumeState::Ready);
    }

    #[test]
    fn reconciler_holds_requests_during_reconciling() {
        let calls = std::rc::Rc::new(std::cell::RefCell::new(0u32));
        let calls_for_closure = std::rc::Rc::clone(&calls);
        let mut rec = Reconciler::new(move || {
            *calls_for_closure.borrow_mut() += 1;
        });

        rec.handle(SleepEvent::Suspend);
        assert_eq!(rec.state(), ResumeState::Reconciling);

        let st = rec.queue_request(Request {
            user: "alice".into(),
        });
        assert_eq!(st, PendingState::Reconciling);
        assert_eq!(rec.pending_count(), 1);
    }

    #[test]
    fn reconciler_drains_queued_requests_after_ready() {
        let calls = std::rc::Rc::new(std::cell::RefCell::new(0u32));
        let calls_for_closure = std::rc::Rc::clone(&calls);
        let mut rec = Reconciler::new(move || {
            *calls_for_closure.borrow_mut() += 1;
        });

        rec.handle(SleepEvent::Suspend);
        rec.queue_request(Request {
            user: "alice".into(),
        });
        rec.queue_request(Request { user: "bob".into() });
        assert_eq!(rec.pending_count(), 2);

        rec.handle(SleepEvent::Resume);
        assert_eq!(*calls.borrow(), 1, "on_reconcile fired once on resume");
        assert_eq!(rec.state(), ResumeState::Reconciling);

        let drained = rec.drain_ready();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].user, "alice");
        assert_eq!(drained[1].user, "bob");
        assert_eq!(rec.pending_count(), 0);
        assert_eq!(rec.state(), ResumeState::Ready);
    }

    #[test]
    fn reconciler_services_immediately_when_ready() {
        let mut rec = Reconciler::new(|| {});
        assert_eq!(rec.state(), ResumeState::Ready);
        let st = rec.queue_request(Request {
            user: "carol".into(),
        });
        assert_eq!(st, PendingState::Serviced);
        assert_eq!(rec.pending_count(), 0);
    }

    #[test]
    fn reconciler_fires_on_reconcile_once_per_resume() {
        let calls = std::rc::Rc::new(std::cell::RefCell::new(0u32));
        let calls_for_closure = std::rc::Rc::clone(&calls);
        let mut rec = Reconciler::new(move || {
            *calls_for_closure.borrow_mut() += 1;
        });

        rec.handle(SleepEvent::Suspend);
        rec.handle(SleepEvent::Resume);
        assert_eq!(*calls.borrow(), 1);

        rec.drain_ready();
        rec.handle(SleepEvent::Suspend);
        rec.handle(SleepEvent::Resume);
        assert_eq!(*calls.borrow(), 2);
    }

    #[test]
    fn reconciler_drops_when_queue_full() {
        let mut rec = Reconciler::new_with_capacity(|| {}, 2);
        rec.handle(SleepEvent::Suspend);
        assert_eq!(
            rec.queue_request(Request { user: "a".into() }),
            PendingState::Reconciling
        );
        assert_eq!(
            rec.queue_request(Request { user: "b".into() }),
            PendingState::Reconciling
        );
        assert_eq!(
            rec.queue_request(Request { user: "c".into() }),
            PendingState::Dropped
        );
        assert_eq!(rec.pending_count(), 2, "dropped request not enqueued");
    }

    #[test]
    fn reconciler_records_and_exposes_rediscovery_outcome() {
        let mut rec = Reconciler::new(|| {});
        assert!(rec.last_outcome().is_none());
        rec.record_rediscovery(ReconcileOutcome::Rediscovered {
            path: PathBuf::from("/dev/video2"),
        });
        assert_eq!(
            rec.last_outcome(),
            Some(&ReconcileOutcome::Rediscovered {
                path: PathBuf::from("/dev/video2")
            })
        );
    }

    #[test]
    fn reconciler_stale_node_outcome_after_renumber() {
        let mut rec = Reconciler::new(|| {});
        rec.record_rediscovery(ReconcileOutcome::StaleNode {
            old: PathBuf::from("/dev/video2"),
            new: PathBuf::from("/dev/video4"),
        });
        assert_eq!(
            rec.last_outcome(),
            Some(&ReconcileOutcome::StaleNode {
                old: PathBuf::from("/dev/video2"),
                new: PathBuf::from("/dev/video4")
            })
        );
    }

    #[test]
    fn reconciler_missing_camera_outcome_signals_password_fallback() {
        let mut rec = Reconciler::new(|| {});
        rec.record_rediscovery(ReconcileOutcome::MissingCamera);
        assert_eq!(rec.last_outcome(), Some(&ReconcileOutcome::MissingCamera));
        let drained = rec.drain_ready();
        assert!(drained.is_empty());
        assert_eq!(rec.state(), ResumeState::Ready);
    }
}
