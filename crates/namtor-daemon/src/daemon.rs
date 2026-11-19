// SPDX-License-Identifier: GPL-3.0-only
//! The top-level daemon: owns the worker supervisor, the lifecycle, and a
//! capture/emitter factory that is invoked lazily on first `prepare_for_auth`.
//!
//! At boot, models are loaded into the warm worker (`load_all`), the lifecycle
//! is `Cold`, and NO camera or emitter controller is constructed. The factory
//! runs only when an auth request actually needs the camera (spec §5.5).

use std::time::Duration;

use namtor_proto::pam::Requester;

use crate::lifecycle::{Clock, LcPhase, Lifecycle};
use crate::supervisor::WorkerSupervisor;
use crate::DaemonError;

pub type CaptureFactory =
    Box<dyn FnMut() -> Result<Box<dyn namtor_capture::source::CaptureSource>, DaemonError> + Send>;

pub struct Daemon<S: WorkerSupervisor, C: Clock> {
    supervisor: S,
    lifecycle: Lifecycle<C>,
    capture_factory: Option<CaptureFactory>,
    capture: Option<Box<dyn namtor_capture::source::CaptureSource>>,
    loaded: Vec<namtor_proto::worker::Loaded>,
}

impl<S: WorkerSupervisor, C: Clock> Daemon<S, C> {
    pub fn new(
        supervisor: S,
        models: &[namtor_proto::worker::ModelSpec],
        warm_timeout: Duration,
        clock: C,
        capture_factory: Option<CaptureFactory>,
    ) -> Result<Self, DaemonError> {
        let loaded = supervisor.load_all(models)?;
        let lifecycle = Lifecycle::new(clock, warm_timeout);
        Ok(Self {
            supervisor,
            lifecycle,
            capture_factory,
            capture: None,
            loaded,
        })
    }

    pub fn phase(&self) -> LcPhase {
        self.lifecycle.phase()
    }

    pub fn loaded_models(&self) -> &[namtor_proto::worker::Loaded] {
        &self.loaded
    }

    pub fn capture_open(&self) -> bool {
        self.capture.is_some()
    }

    pub fn prepare_for_auth(&mut self, req: Requester) -> Result<(), DaemonError> {
        if self.capture.is_none() {
            let factory = self
                .capture_factory
                .as_mut()
                .ok_or_else(|| DaemonError::State("no capture factory".into()))?;
            let src = factory()?;
            self.capture = Some(src);
        }
        self.lifecycle.prepare_for_auth(req)
    }

    pub fn verify_start(&mut self, req: Requester) -> Result<(), DaemonError> {
        self.lifecycle.verify_start(req)?;
        // Issue #30, fix 3: entering Verifying must not land in a duty-cycle
        // off-window. The capture turns the emitter on if it is dark; if it
        // is already on, the live on-budget is retained (never reset while
        // illuminated). A source without emitter control is a no-op.
        if let Some(src) = self.capture.as_mut() {
            src.ensure_illuminated();
        }
        Ok(())
    }

    pub fn release(&mut self) -> Result<(), DaemonError> {
        self.lifecycle.release()?;
        if let Some(mut src) = self.capture.take() {
            src.stop();
        }
        Ok(())
    }

    pub fn tick(&mut self) {
        self.lifecycle.tick();
        if self.lifecycle.phase() == LcPhase::Cold && self.capture.is_some() {
            if let Some(mut src) = self.capture.take() {
                src.stop();
            }
        }
    }

    pub fn snapshot(&self) -> namtor_proto::pam::StatusReply {
        self.lifecycle.snapshot()
    }

    pub fn supervisor_mut(&mut self) -> &mut S {
        &mut self.supervisor
    }

    pub fn capture_mut(&mut self) -> Option<&mut Box<dyn namtor_capture::source::CaptureSource>> {
        self.capture.as_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::MockClock;
    use crate::supervisor::MockSupervisor;
    use namtor_capture::frame::{Frame, PixFmt};
    use namtor_capture::source::{CaptureError, CaptureSource, SourceCaps, Spectrum};
    use namtor_proto::worker::{ModelSpec, Quant};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

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

    fn models() -> Vec<ModelSpec> {
        vec![ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        }]
    }

    fn requester() -> Requester {
        Requester {
            uid: 1000,
            label: "gdm".into(),
        }
    }

    #[test]
    fn boot_loads_models_but_never_opens_camera() {
        let sup = MockSupervisor::new();
        let daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            MockClock::new(Instant::now()),
            Some(Box::new(|| {
                panic!("capture factory must NOT be invoked at boot")
            })),
        )
        .expect("boot");

        assert_eq!(daemon.phase(), LcPhase::Cold);
        assert!(!daemon.capture_open());
        assert_eq!(daemon.loaded_models().len(), 1);
    }

    #[test]
    fn prepare_for_auth_invokes_factory_lazily() {
        let count = Arc::new(AtomicUsize::new(0));
        let factory_count = count.clone();
        let sup = MockSupervisor::new();
        let mut daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            MockClock::new(Instant::now()),
            Some(Box::new(move || {
                factory_count.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(StubSource) as Box<dyn CaptureSource>)
            })),
        )
        .expect("boot");

        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(!daemon.capture_open());

        daemon.prepare_for_auth(requester()).unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(daemon.capture_open());
        assert_eq!(daemon.phase(), LcPhase::Warming);
    }

    #[test]
    fn release_closes_capture() {
        let count = Arc::new(AtomicUsize::new(0));
        let factory_count = count.clone();
        let sup = MockSupervisor::new();
        let mut daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            MockClock::new(Instant::now()),
            Some(Box::new(move || {
                factory_count.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(StubSource) as Box<dyn CaptureSource>)
            })),
        )
        .expect("boot");

        daemon.prepare_for_auth(requester()).unwrap();
        assert!(daemon.capture_open());
        daemon.verify_start(requester()).unwrap();
        daemon.release().unwrap();
        assert!(!daemon.capture_open());
        assert_eq!(daemon.phase(), LcPhase::Cold);
    }

    #[test]
    fn tick_auto_releases_warm_timeout_and_closes_capture() {
        let clock = MockClock::new(Instant::now());
        let count = Arc::new(AtomicUsize::new(0));
        let factory_count = count.clone();
        let sup = MockSupervisor::new();
        let mut daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(5),
            clock.clone(),
            Some(Box::new(move || {
                factory_count.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(StubSource) as Box<dyn CaptureSource>)
            })),
        )
        .expect("boot");

        daemon.prepare_for_auth(requester()).unwrap();
        assert!(daemon.capture_open());

        clock.advance(Duration::from_secs(6));
        daemon.tick();
        assert_eq!(daemon.phase(), LcPhase::Cold);
        assert!(!daemon.capture_open());
    }

    /// A capture source that counts `ensure_illuminated` calls (issue #30).
    struct HookSource {
        calls: Arc<AtomicUsize>,
    }

    impl CaptureSource for HookSource {
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
        fn ensure_illuminated(&mut self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Issue #30, fix 3: entering Verifying must obtain an illuminated window
    /// deterministically — `verify_start` asks the capture, which turns the
    /// emitter on if it is dark and keeps the live budget if it is already on.
    #[test]
    fn verify_start_requests_an_illuminated_window() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hook_calls = calls.clone();
        let sup = MockSupervisor::new();
        let mut daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            MockClock::new(Instant::now()),
            Some(Box::new(move || {
                Ok(Box::new(HookSource {
                    calls: hook_calls.clone(),
                }) as Box<dyn CaptureSource>)
            })),
        )
        .expect("boot");

        daemon.prepare_for_auth(requester()).unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "warming must not force a window"
        );
        daemon.verify_start(requester()).unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "verify_start must ask the capture for an illuminated window"
        );
    }

    /// Review B1 (round 1): `prepare_for_auth` opens and arms the emitter,
    /// and nothing pulls a frame until `verify_start`. The 2 s continuous-on
    /// cap must be enforced with ZERO `next_frame` calls — the emitter goes
    /// dark while the daemon just sits in `Warming` (no `Daemon::tick`).
    #[test]
    fn warming_capture_is_capped_without_any_frame_pulls() {
        use crate::live_capture::emitter_test_support::{armed_test_source, FakeDev};
        use std::time::Duration;

        let dev = FakeDev::new();
        let dev_handle = dev.clone();
        let sup = MockSupervisor::new();
        let mut daemon = Daemon::new(
            sup,
            &models(),
            Duration::from_secs(30),
            MockClock::new(Instant::now()),
            Some(Box::new(move || {
                Ok(armed_test_source(
                    &dev_handle,
                    300,
                    300,
                    Box::new(crate::lifecycle::SystemClock),
                    Duration::from_millis(10),
                ) as Box<dyn CaptureSource>)
            })),
        )
        .expect("boot");

        daemon.prepare_for_auth(requester()).unwrap();
        assert_eq!(daemon.phase(), LcPhase::Warming);

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && dev.current() != armed_test_source_payload_off() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            daemon.phase(),
            LcPhase::Warming,
            "no tick was called: the cap held purely in the capture layer"
        );
        assert_eq!(
            dev.current(),
            armed_test_source_payload_off(),
            "the emitter must be forced off during warming with zero frame pulls (review B1)"
        );
    }

    fn armed_test_source_payload_off() -> Vec<u8> {
        crate::live_capture::emitter_test_support::PAYLOAD_OFF.to_vec()
    }
}
