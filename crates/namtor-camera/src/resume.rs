// SPDX-License-Identifier: GPL-3.0-only
//! Resume/suspend state machine and logind `PrepareForSleep` subscription.
//!
//! Spec §4.10.3: on resume the daemon must re-detect the device, reopen it,
//! and re-apply the emitter mode before servicing auth requests. The state
//! machine is testable without D-Bus by injecting [`SleepEvent`]s directly;
//! the live [`ResumeMonitor`] wires it to logind's `PrepareForSleep` signal.

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
}

/// Owns the resume state machine plus a queue of auth requests. On resume,
/// re-runs the reconcile callback (re-detect device, re-apply emitter), then
/// drains queued requests once `mark_ready` is called.
pub struct Reconciler<F: FnMut()> {
    machine: ResumeMachine,
    pending: Vec<Request>,
    on_reconcile: F,
}

impl<F: FnMut()> Reconciler<F> {
    pub fn new(on_reconcile: F) -> Self {
        Self {
            machine: ResumeMachine::new(),
            pending: Vec::new(),
            on_reconcile,
        }
    }

    pub fn state(&self) -> ResumeState {
        self.machine.state()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Feeds a sleep event. On `Resume`, transitions to `Reconciling` and
    /// fires the reconcile callback directly (re-detect + re-apply emitter).
    pub fn handle(&mut self, event: SleepEvent) {
        self.machine.handle(event, &NoopSignal);
        if event == SleepEvent::Resume && self.machine.state() == ResumeState::Reconciling {
            (self.on_reconcile)();
        }
    }

    /// Queues a request. Returns `Serviced` if the machine is `Ready`, or
    /// `Reconciling` if the request is held until reconciliation completes.
    pub fn queue_request(&mut self, req: Request) -> PendingState {
        match self.machine.state() {
            ResumeState::Ready => PendingState::Serviced,
            ResumeState::Reconciling => {
                self.pending.push(req);
                PendingState::Reconciling
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
}
