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
        self.lifecycle.verify_start(req)
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
}
