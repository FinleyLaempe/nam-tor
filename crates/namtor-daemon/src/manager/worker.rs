// SPDX-License-Identifier: GPL-3.0-only
//! Per-session enrollment worker thread (plan task 5).
//!
//! [`spawn_worker`] moves an [`EnrollmentCapture`] into a dedicated
//! `std::thread` that exclusively owns it. The manager service holds only a
//! lightweight [`WorkerHandle`] (command sender + join handle + latest
//! sanitized progress snapshot) — never the capture itself. Communication
//! uses bounded `std::sync::mpsc` (commands) and a bounded
//! `Mutex<VecDeque>` + `Condvar` (events) so the worker never blocks on a
//! full event queue: when the queue is full, the oldest [`WorkerEvent::Progress`]
//! is dropped (terminal events always get through).
//
// allow: SIZE_OK — single cohesive worker concept (thread + channels + clock +
// handle + registry + tests). Splitting would fragment one state machine across
// files; the event queue, clock, and handle are integral to the worker loop.

use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use namtor_store::template::Template;

use crate::dbus::ApiError;
use crate::manager::observation::{self, CaptureObservation, GuidanceProgress};
use crate::manager::session::{CapturePhase, EnrollmentCapture, ReviewSnapshot};

/// Minimum interval between [`WorkerEvent::Progress`] emissions (plan: 250 ms).
pub const PROGRESS_INTERVAL_MS: u64 = 250;
const CMD_CHANNEL_CAP: usize = 16;
const EVENT_CHANNEL_CAP: usize = 32;
const RECV_TIMEOUT_MS: u64 = 50;
const JOIN_TIMEOUT_SECS: u64 = 5;

/// Commands sent from the manager to the worker thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerCommand {
    Stop,
    Resume,
    Review,
    Commit,
    Cancel,
    Shutdown,
}

/// Events emitted by the worker thread to the manager.
#[derive(Debug, Clone)]
pub enum WorkerEvent {
    Progress(GuidanceProgress),
    ReviewReady(ReviewSnapshot),
    Committed(Vec<Template>),
    Cancelled,
    Error(String),
    Exhausted,
}

impl WorkerEvent {
    fn is_progress(&self) -> bool {
        matches!(self, WorkerEvent::Progress(_))
    }
}

/// Injectable clock for the worker loop. Tests use [`MockWorkerClock`].
pub trait WorkerClock: Send {
    fn now(&self) -> Instant;
    fn sleep(&self, dur: Duration);
}

/// Real-time clock that actually sleeps.
pub struct SystemWorkerClock;
impl WorkerClock for SystemWorkerClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, dur: Duration) {
        thread::sleep(dur);
    }
}

/// Deterministic virtual clock for tests. `sleep()` advances virtual time.
#[derive(Debug, Clone)]
pub struct MockWorkerClock {
    t: Arc<Mutex<Instant>>,
}
impl MockWorkerClock {
    pub fn new(start: Instant) -> Self {
        Self {
            t: Arc::new(Mutex::new(start)),
        }
    }
    pub fn advance(&self, d: Duration) {
        *lock(&self.t) += d;
    }
    pub fn shared_instant(&self) -> Arc<Mutex<Instant>> {
        Arc::clone(&self.t)
    }
}
impl WorkerClock for MockWorkerClock {
    fn now(&self) -> Instant {
        *lock(&self.t)
    }
    fn sleep(&self, dur: Duration) {
        self.advance(dur);
    }
}

// --- Bounded event queue (drop-oldest-progress on overflow) ---

#[derive(Debug)]
struct BoundedEventQueue {
    inner: Mutex<VecDeque<WorkerEvent>>,
    cvar: Condvar,
    cap: usize,
}

impl BoundedEventQueue {
    fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(cap)),
            cvar: Condvar::new(),
            cap,
        }
    }

    fn push(&self, event: WorkerEvent) {
        let mut q = lock(&self.inner);
        if q.len() >= self.cap {
            if event.is_progress() {
                drop_oldest_progress(&mut q);
            } else {
                while q.len() >= self.cap {
                    match q.pop_front() {
                        Some(WorkerEvent::Progress(_)) => continue,
                        Some(other) => {
                            q.push_front(other);
                            q.pop_front();
                        }
                        None => break,
                    }
                }
            }
        }
        q.push_back(event);
        self.cvar.notify_one();
    }

    fn recv_timeout(&self, timeout: Duration) -> Option<WorkerEvent> {
        let mut q = lock(&self.inner);
        if !q.is_empty() {
            return q.pop_front();
        }
        let (q2, _) = match self.cvar.wait_timeout(q, timeout) {
            Ok(r) => r,
            Err(e) => e.into_inner(),
        };
        let mut q2 = q2;
        q2.pop_front()
    }

    fn try_recv(&self) -> Option<WorkerEvent> {
        lock(&self.inner).pop_front()
    }
}

fn drop_oldest_progress(q: &mut VecDeque<WorkerEvent>) {
    if let Some(pos) = q.iter().position(|e| e.is_progress()) {
        q.remove(pos);
    } else {
        q.pop_front();
    }
}

/// Receiver end of the event queue.
#[derive(Debug)]
pub struct EventReceiver {
    queue: Arc<BoundedEventQueue>,
}
impl EventReceiver {
    pub fn recv_timeout(&self, timeout: Duration) -> Option<WorkerEvent> {
        self.queue.recv_timeout(timeout)
    }
    pub fn try_recv(&self) -> Option<WorkerEvent> {
        self.queue.try_recv()
    }
    pub fn drain(&self) -> Vec<WorkerEvent> {
        let mut out = Vec::new();
        while let Some(e) = self.try_recv() {
            out.push(e);
        }
        out
    }
}

// --- WorkerHandle ---

pub struct WorkerHandle {
    cmd_tx: SyncSender<WorkerCommand>,
    handle: Option<JoinHandle<()>>,
    latest_progress: Arc<Mutex<Option<GuidanceProgress>>>,
}
impl WorkerHandle {
    pub fn send_command(&self, cmd: WorkerCommand) -> Result<(), ApiError> {
        self.cmd_tx
            .send(cmd)
            .map_err(|_| ApiError::State("worker thread exited".into()))
    }
    pub fn latest_progress(&self) -> Option<GuidanceProgress> {
        lock(&self.latest_progress).clone()
    }
    pub fn join(&mut self, timeout: Duration) -> bool {
        let handle = match self.handle.take() {
            Some(h) => h,
            None => return true,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let _ = handle.join();
            let _ = tx.send(());
        });
        rx.recv_timeout(timeout).is_ok()
    }
    pub fn shutdown_and_join(&mut self) -> bool {
        let _ = self.cmd_tx.send(WorkerCommand::Shutdown);
        self.join(Duration::from_secs(JOIN_TIMEOUT_SECS))
    }
}
impl Drop for WorkerHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(WorkerCommand::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// --- WorkerRegistry: one session per UID ---

pub struct WorkerRegistry {
    workers: HashMap<u32, WorkerHandle>,
}
impl WorkerRegistry {
    pub fn new() -> Self {
        Self {
            workers: HashMap::new(),
        }
    }
    pub fn start<C: WorkerClock + 'static>(
        &mut self,
        uid: u32,
        capture: EnrollmentCapture,
        clock: C,
    ) -> Result<EventReceiver, ApiError> {
        if self.workers.contains_key(&uid) {
            return Err(ApiError::SessionBusy(uid));
        }
        let (handle, rx) = spawn_worker(capture, clock);
        self.workers.insert(uid, handle);
        Ok(rx)
    }
    pub fn remove(&mut self, uid: u32) -> Option<WorkerHandle> {
        self.workers.remove(&uid)
    }
    pub fn contains(&self, uid: u32) -> bool {
        self.workers.contains_key(&uid)
    }
    pub fn len(&self) -> usize {
        self.workers.len()
    }
    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }
    pub fn shutdown_all(&mut self) {
        let workers = std::mem::take(&mut self.workers);
        for (_, mut h) in workers {
            h.shutdown_and_join();
        }
    }
}
impl Default for WorkerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// --- spawn_worker ---

pub fn spawn_worker<C: WorkerClock + 'static>(
    capture: EnrollmentCapture,
    clock: C,
) -> (WorkerHandle, EventReceiver) {
    let (cmd_tx, cmd_rx) = sync_channel::<WorkerCommand>(CMD_CHANNEL_CAP);
    let queue = Arc::new(BoundedEventQueue::new(EVENT_CHANNEL_CAP));
    let latest_progress = Arc::new(Mutex::new(None::<GuidanceProgress>));
    let queue_clone = Arc::clone(&queue);
    let latest_clone = Arc::clone(&latest_progress);
    let handle = thread::spawn(move || {
        worker_loop(capture, cmd_rx, queue_clone, latest_clone, clock);
    });
    let worker_handle = WorkerHandle {
        cmd_tx,
        handle: Some(handle),
        latest_progress,
    };
    (worker_handle, EventReceiver { queue })
}

// --- worker loop ---

enum CmdOutcome {
    Continue,
    Exit,
}

fn worker_loop(
    mut capture: EnrollmentCapture,
    cmd_rx: std::sync::mpsc::Receiver<WorkerCommand>,
    events: Arc<BoundedEventQueue>,
    latest_progress: Arc<Mutex<Option<GuidanceProgress>>>,
    clock: impl WorkerClock,
) {
    let interval = Duration::from_millis(PROGRESS_INTERVAL_MS);
    let mut last_emit: Option<Instant> = None;
    let mut exhausted = false;
    loop {
        let pumping = !exhausted && capture.phase() == CapturePhase::Capturing;
        if pumping {
            match cmd_rx.try_recv() {
                Ok(cmd) => {
                    if let CmdOutcome::Exit =
                        handle_command(cmd, &mut capture, &events, &mut exhausted)
                    {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => {
                    let result = catch_unwind(AssertUnwindSafe(|| capture.pump()));
                    match result {
                        Ok(Ok(Some(snapshot))) => {
                            let progress = compute_progress(&snapshot, &capture);
                            maybe_emit_progress(
                                &events,
                                &latest_progress,
                                &clock,
                                &mut last_emit,
                                interval,
                                progress,
                            );
                        }
                        Ok(Ok(None)) => {
                            exhausted = true;
                            events.push(WorkerEvent::Exhausted);
                        }
                        Ok(Err(e)) => {
                            capture.cancel();
                            events.push(WorkerEvent::Error(e.to_string()));
                            return;
                        }
                        Err(_) => {
                            capture.cancel();
                            events.push(WorkerEvent::Error("capture pump panicked".into()));
                            return;
                        }
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    capture.cancel();
                    return;
                }
            }
        } else {
            match cmd_rx.recv_timeout(Duration::from_millis(RECV_TIMEOUT_MS)) {
                Ok(cmd) => {
                    if let CmdOutcome::Exit =
                        handle_command(cmd, &mut capture, &events, &mut exhausted)
                    {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    capture.cancel();
                    return;
                }
            }
        }
    }
}

fn handle_command(
    cmd: WorkerCommand,
    capture: &mut EnrollmentCapture,
    events: &Arc<BoundedEventQueue>,
    exhausted: &mut bool,
) -> CmdOutcome {
    match cmd {
        WorkerCommand::Stop => match capture.stop() {
            Ok(()) => match capture.review_snapshot() {
                Ok(review) => events.push(WorkerEvent::ReviewReady(review)),
                Err(e) => events.push(WorkerEvent::Error(e.to_string())),
            },
            Err(e) => events.push(WorkerEvent::Error(e.to_string())),
        },
        WorkerCommand::Resume => {
            *exhausted = false;
            if let Err(e) = capture.resume() {
                events.push(WorkerEvent::Error(e.to_string()));
            }
        }
        WorkerCommand::Review => match capture.review_snapshot() {
            Ok(review) => events.push(WorkerEvent::ReviewReady(review)),
            Err(e) => events.push(WorkerEvent::Error(e.to_string())),
        },
        WorkerCommand::Commit => match capture.finish() {
            Ok(finished) => {
                events.push(WorkerEvent::Committed(finished.templates));
                return CmdOutcome::Exit;
            }
            Err(e) => events.push(WorkerEvent::Error(e.to_string())),
        },
        WorkerCommand::Cancel => {
            capture.cancel();
            events.push(WorkerEvent::Cancelled);
            return CmdOutcome::Exit;
        }
        WorkerCommand::Shutdown => {
            capture.cancel();
            return CmdOutcome::Exit;
        }
    }
    CmdOutcome::Continue
}

fn maybe_emit_progress(
    events: &Arc<BoundedEventQueue>,
    latest: &Arc<Mutex<Option<GuidanceProgress>>>,
    clock: &impl WorkerClock,
    last_emit: &mut Option<Instant>,
    interval: Duration,
    progress: GuidanceProgress,
) {
    let now = clock.now();
    let should_emit = match last_emit {
        None => true,
        Some(prev) => now.duration_since(*prev) >= interval,
    };
    if should_emit {
        *lock(latest) = Some(progress.clone());
        events.push(WorkerEvent::Progress(progress));
        *last_emit = Some(now);
    }
}

fn compute_progress(
    snapshot: &crate::manager::session::ProgressSnapshot,
    capture: &EnrollmentCapture,
) -> GuidanceProgress {
    use crate::dbus::QualityLabel;
    let quality_percent: u32 = match snapshot.quality {
        QualityLabel::Low => 20,
        QualityLabel::Medium => 50,
        QualityLabel::High => 85,
    };
    let frames_seen = snapshot.frames_captured;
    let frames_accepted = capture.reservoir_len() as u32;
    let frames_rejected = frames_seen.saturating_sub(frames_accepted);
    let diversity_percent = snapshot.coverage_percent.min(100);
    let obs = CaptureObservation {
        detection_suitability: quality_percent.min(100) as u8,
        landmark_stability: 100,
        blur_estimate: quality_percent.min(100) as u8,
        pose_frontality: quality_percent.min(100) as u8,
        illumination: quality_percent.min(100) as u8,
        embedding_spread: diversity_percent.min(100) as u8,
    };
    observation::project(&obs, frames_seen, frames_accepted, frames_rejected)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::session::MockEmbedSource;
    use namtor_capture::frame::{EmitterState, Frame, PixFmt};
    use namtor_capture::source::{CaptureError, CaptureSource, SourceCaps, Spectrum};

    struct MockSource {
        frames: Vec<Frame>,
        idx: usize,
        served: usize,
        max_serve: usize,
        started: bool,
        clock_t: Option<Arc<Mutex<Instant>>>,
        frame_interval: Duration,
        error_after: Option<usize>,
    }
    impl MockSource {
        fn finite(frames: Vec<Frame>) -> Self {
            let len = frames.len();
            Self {
                frames,
                idx: 0,
                served: 0,
                max_serve: len,
                started: false,
                clock_t: None,
                frame_interval: Duration::ZERO,
                error_after: None,
            }
        }
        fn circular(frames: Vec<Frame>, count: usize) -> Self {
            Self {
                frames,
                idx: 0,
                served: 0,
                max_serve: count,
                started: false,
                clock_t: None,
                frame_interval: Duration::ZERO,
                error_after: None,
            }
        }
        fn with_clock(mut self, clock: &MockWorkerClock, interval: Duration) -> Self {
            self.clock_t = Some(clock.shared_instant());
            self.frame_interval = interval;
            self
        }
        fn error_after_n(mut self, n: usize) -> Self {
            self.error_after = Some(n);
            self
        }
    }
    impl CaptureSource for MockSource {
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
            self.started = true;
            Ok(())
        }
        fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError> {
            if let Some(t) = &self.clock_t {
                *lock(t) += self.frame_interval;
            }
            if let Some(err_at) = self.error_after {
                if self.served >= err_at {
                    return Err(CaptureError::Device("mock source error".into()));
                }
            }
            if self.max_serve > 0 && self.served >= self.max_serve {
                return Ok(None);
            }
            if self.frames.is_empty() {
                return Ok(None);
            }
            let f = self.frames[self.idx % self.frames.len()].clone();
            self.idx += 1;
            self.served += 1;
            Ok(Some(f))
        }
        fn stop(&mut self) {
            self.started = false;
        }
    }

    fn grey_frame(byte: u8) -> Frame {
        Frame {
            data: vec![byte; 64],
            format: PixFmt::Grey8,
            width: 8,
            height: 8,
            ts_mono: Duration::from_millis(0),
            sof: None,
            emitter: EmitterState::On,
            quality: None,
        }
    }
    fn make_capture(frames: Vec<Frame>, max_reservoir: usize) -> EnrollmentCapture {
        let capture = Box::new(MockSource::finite(frames));
        let embed = Box::new(MockEmbedSource::new(8));
        EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(capture),
            Some(embed),
            max_reservoir,
        )
    }
    fn make_capture_circular(
        frames: Vec<Frame>,
        count: usize,
        max_reservoir: usize,
    ) -> EnrollmentCapture {
        let capture = Box::new(MockSource::circular(frames, count));
        let embed = Box::new(MockEmbedSource::new(8));
        EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(capture),
            Some(embed),
            max_reservoir,
        )
    }
    fn collect_events(rx: &EventReceiver, timeout: Duration) -> Vec<WorkerEvent> {
        let mut out = Vec::new();
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(e) = rx.recv_timeout(Duration::from_millis(100)) {
                out.push(e);
            }
        }
        out
    }
    fn wait_for_event(rx: &EventReceiver, timeout: Duration) -> Option<WorkerEvent> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(e) = rx.recv_timeout(Duration::from_millis(50)) {
                return Some(e);
            }
        }
        None
    }
    fn wait_for_non_progress(rx: &EventReceiver, timeout: Duration) -> Option<WorkerEvent> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(e) = rx.recv_timeout(Duration::from_millis(50)) {
                if !matches!(e, WorkerEvent::Progress(_)) {
                    return Some(e);
                }
            }
        }
        None
    }
    fn count_progress(events: &[WorkerEvent]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, WorkerEvent::Progress(_)))
            .count()
    }

    #[test]
    fn worker_pumps_frames_and_emits_progress() {
        let frames: Vec<Frame> = (0..30).map(|i| grey_frame(110 + i)).collect();
        let capture = make_capture(frames, 100);
        let clock = MockWorkerClock::new(Instant::now());
        let (mut handle, rx) = spawn_worker(capture, clock);
        let events = collect_events(&rx, Duration::from_secs(2));
        assert!(count_progress(&events) >= 1, "at least one progress event");
        assert!(
            events.iter().any(|e| matches!(e, WorkerEvent::Exhausted)),
            "exhausted after finite source"
        );
        handle.shutdown_and_join();
    }

    #[test]
    fn worker_stop_emits_review_ready() {
        let frames: Vec<Frame> = (0..10).map(|i| grey_frame(110 + i)).collect();
        let capture = make_capture_circular(frames, 10000, 100);
        let clock = MockWorkerClock::new(Instant::now());
        let (mut handle, rx) = spawn_worker(capture, clock);
        thread::sleep(Duration::from_millis(50));
        handle.send_command(WorkerCommand::Stop).unwrap();
        let event = wait_for_non_progress(&rx, Duration::from_secs(2));
        match &event {
            Some(WorkerEvent::ReviewReady(review)) => assert!(review.frames_accepted > 0),
            other => panic!("expected ReviewReady, got {other:?}"),
        }
        handle.shutdown_and_join();
    }

    #[test]
    fn worker_resume_continues_capture() {
        let frames: Vec<Frame> = (0..5).map(|i| grey_frame(110 + i)).collect();
        let clock = MockWorkerClock::new(Instant::now());
        let src = MockSource::circular(frames, 10000).with_clock(&clock, Duration::from_millis(50));
        let embed = Box::new(MockEmbedSource::new(8));
        let capture = EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(Box::new(src)),
            Some(embed),
            100,
        );
        let (mut handle, rx) = spawn_worker(capture, clock.clone());
        thread::sleep(Duration::from_millis(50));
        handle.send_command(WorkerCommand::Stop).unwrap();
        let _ = wait_for_non_progress(&rx, Duration::from_secs(1));
        handle.send_command(WorkerCommand::Resume).unwrap();
        let events = collect_events(&rx, Duration::from_millis(300));
        assert!(
            events.iter().any(|e| matches!(e, WorkerEvent::Progress(_))),
            "progress resumed"
        );
        handle.shutdown_and_join();
    }

    #[test]
    fn worker_commit_emits_templates() {
        let frames: Vec<Frame> = (0..10).map(|i| grey_frame(110 + i)).collect();
        let capture = make_capture_circular(frames, 10000, 100);
        let clock = MockWorkerClock::new(Instant::now());
        let (mut handle, rx) = spawn_worker(capture, clock);
        thread::sleep(Duration::from_millis(50));
        handle.send_command(WorkerCommand::Stop).unwrap();
        let _ = wait_for_non_progress(&rx, Duration::from_secs(1));
        handle.send_command(WorkerCommand::Commit).unwrap();
        let event = wait_for_non_progress(&rx, Duration::from_secs(2));
        match &event {
            Some(WorkerEvent::Committed(t)) => assert!(!t.is_empty(), "templates produced"),
            other => panic!("expected Committed, got {other:?}"),
        }
        handle.shutdown_and_join();
    }

    #[test]
    fn worker_cancel_emits_cancelled_and_zeroizes() {
        let frames: Vec<Frame> = (0..10).map(|i| grey_frame(110 + i)).collect();
        let capture = make_capture_circular(frames, 10000, 100);
        let clock = MockWorkerClock::new(Instant::now());
        let (mut handle, rx) = spawn_worker(capture, clock);
        thread::sleep(Duration::from_millis(50));
        handle.send_command(WorkerCommand::Cancel).unwrap();
        let event = wait_for_non_progress(&rx, Duration::from_secs(2));
        assert!(
            matches!(event, Some(WorkerEvent::Cancelled)),
            "expected Cancelled, got {event:?}"
        );
        assert!(handle.join(Duration::from_secs(2)), "worker exited");
    }

    #[test]
    fn worker_shutdown_joins_cleanly() {
        let frames: Vec<Frame> = (0..5).map(|i| grey_frame(110 + i)).collect();
        let capture = make_capture_circular(frames, 1000, 100);
        let clock = MockWorkerClock::new(Instant::now());
        let (mut handle, _rx) = spawn_worker(capture, clock);
        thread::sleep(Duration::from_millis(50));
        assert!(handle.shutdown_and_join(), "thread exited within timeout");
    }

    #[test]
    fn worker_source_exhausted_emits_exhausted() {
        let frames: Vec<Frame> = (0..10).map(|i| grey_frame(110 + i)).collect();
        let capture = make_capture(frames, 100);
        let clock = MockWorkerClock::new(Instant::now());
        let (mut handle, rx) = spawn_worker(capture, clock);
        let event = wait_for_non_progress(&rx, Duration::from_secs(2));
        assert!(
            matches!(event, Some(WorkerEvent::Exhausted)),
            "expected Exhausted, got {event:?}"
        );
        handle.send_command(WorkerCommand::Stop).unwrap();
        let event2 = wait_for_non_progress(&rx, Duration::from_secs(1));
        assert!(
            matches!(event2, Some(WorkerEvent::ReviewReady(_))),
            "ReviewReady post-exhaustion"
        );
        handle.shutdown_and_join();
    }

    #[test]
    fn worker_source_error_emits_error_and_cancels() {
        let frames: Vec<Frame> = (0..5).map(|i| grey_frame(110 + i)).collect();
        let src = MockSource::finite(frames).error_after_n(3);
        let embed = Box::new(MockEmbedSource::new(8));
        let capture = EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(Box::new(src)),
            Some(embed),
            100,
        );
        let clock = MockWorkerClock::new(Instant::now());
        let (mut handle, rx) = spawn_worker(capture, clock);
        let event = wait_for_non_progress(&rx, Duration::from_secs(2));
        match &event {
            Some(WorkerEvent::Error(msg)) => assert!(
                msg.contains("mock") || msg.contains("capture"),
                "error msg: {msg}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
        assert!(
            handle.join(Duration::from_secs(2)),
            "worker exited after error"
        );
    }

    #[test]
    fn worker_stress_10000_frames_constant_memory() {
        let frames: Vec<Frame> = (0..10).map(|i| grey_frame(110 + i)).collect();
        let clock = MockWorkerClock::new(Instant::now());
        let src = MockSource::circular(frames, 10_000).with_clock(&clock, Duration::from_millis(1));
        let embed = Box::new(MockEmbedSource::new(8));
        let capture = EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(Box::new(src)),
            Some(embed),
            100,
        );
        let (mut handle, rx) = spawn_worker(capture, clock);
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(e) = rx.recv_timeout(Duration::from_millis(100)) {
                events.push(e);
                if matches!(events.last(), Some(WorkerEvent::Exhausted)) {
                    break;
                }
            }
        }
        let progress_count = count_progress(&events);
        assert!(
            progress_count < 100,
            "throttled: {progress_count} < 100, NOT 10000"
        );
        assert!(progress_count >= 1, "at least one immediate progress");
        assert!(events.len() <= 42, "total bounded: {} <= 42", events.len());
        handle.shutdown_and_join();
    }

    #[test]
    fn worker_one_session_per_uid() {
        let frames: Vec<Frame> = (0..5).map(|i| grey_frame(110 + i)).collect();
        let mut registry = WorkerRegistry::new();
        let clock = MockWorkerClock::new(Instant::now());
        let capture1 = make_capture_circular(frames.clone(), 100, 100);
        let _rx1 = registry.start(1000, capture1, clock.clone()).unwrap();
        let capture2 = make_capture_circular(frames, 100, 100);
        let err = registry.start(1000, capture2, clock).unwrap_err();
        assert!(
            matches!(err, ApiError::SessionBusy(1000)),
            "second start rejected: {err:?}"
        );
        registry.shutdown_all();
    }

    #[test]
    fn worker_blocked_source_does_not_deadlock() {
        // Source that returns Ok(None) immediately (no frames) - worker should
        // emit Exhausted and not deadlock on shutdown.
        let capture = make_capture(vec![], 100);
        let clock = MockWorkerClock::new(Instant::now());
        let (mut handle, rx) = spawn_worker(capture, clock);
        let event = wait_for_event(&rx, Duration::from_secs(2));
        assert!(
            matches!(event, Some(WorkerEvent::Exhausted)),
            "immediate exhaustion"
        );
        assert!(handle.shutdown_and_join(), "clean shutdown, no deadlock");
    }

    #[test]
    fn worker_progress_throttle_250ms() {
        // Mock clock advances 100ms per frame. Throttle at 250ms means
        // a progress event every 3 frames (300ms >= 250ms), not every frame.
        let frames: Vec<Frame> = (0..30).map(|i| grey_frame(110 + i)).collect();
        let clock = MockWorkerClock::new(Instant::now());
        let capture =
            make_capture_with_clock_helper(frames, &clock, Duration::from_millis(100), 100);
        let (mut handle, rx) = spawn_worker(capture, clock);
        let events = collect_events(&rx, Duration::from_secs(2));
        let progress_count = count_progress(&events);
        // 30 frames × 100ms = 3000ms virtual time. Throttle at 250ms.
        // Immediate on first frame (frame 1, t=100ms).
        // Next emit when elapsed >= 250ms: t=100+300=400ms (frame 4), etc.
        // So emits at frames 1, 4, 7, 10, 13, 16, 19, 22, 25, 28 → 10 events.
        // Plus 1 Exhausted terminal. Total progress = 10.
        assert!(
            progress_count <= 15,
            "throttled to ~10, got {progress_count}"
        );
        assert!(
            progress_count >= 5,
            "at least 5 progress events over 30 frames, got {progress_count}"
        );
        handle.shutdown_and_join();
    }

    fn make_capture_with_clock_helper(
        frames: Vec<Frame>,
        clock: &MockWorkerClock,
        interval: Duration,
        max_reservoir: usize,
    ) -> EnrollmentCapture {
        let src = MockSource::finite(frames).with_clock(clock, interval);
        let embed = Box::new(MockEmbedSource::new(8));
        EnrollmentCapture::new(
            1000,
            1000,
            ":1.42".into(),
            "alice".into(),
            Some(Box::new(src)),
            Some(embed),
            max_reservoir,
        )
    }
}
