// SPDX-License-Identifier: GPL-3.0-only
//! `namtor daemon` subcommand: the daemon entry point.
//!
//! Wires the full authentication pipeline: spawns the sandboxed worker,
//! opens the template store + recognition models, binds the PAM socket,
//! and dispatches auth requests through the verify engine.
//!
//! Supports systemd socket activation: if `LISTEN_FDS=1` is set, adopts the
//! pre-bound listener fd from systemd (`namtor-daemon.socket`). Otherwise
//! binds `/run/namtor/auth.sock` directly (dev/test path).
//!
//! D-Bus manager service (`org.namtor.Manager` on the system bus) is started
//! in parallel with the PAM listener. If the bus is unavailable or the name
//! is already taken, the daemon logs a warning and continues serving PAM —
//! PAM is the critical path, D-Bus is management convenience.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::acl::resolve_namtor_auth_gid;
use crate::backoff::{BackoffConfig, BackoffStore};
use crate::daemon::Daemon;
use crate::dbus::{ManagerService, SystemPolkit};
use crate::dispatch::{DaemonDispatcher, VerifyRunner};
use crate::lifecycle::{Clock, SystemClock};
use crate::listener::Listener;
use crate::live_capture;
use crate::manager::bus::ManagerBusService;
use crate::manager::dbus::{Manager2Service, Manager3EmbedFactory, Manager3Service};
use crate::manager::embed::PipelineEmbedSource;
use crate::manager::store::{ProfileStore, TemplateProfileStore};
use crate::models::{
    hex, resolve_required_specs, ManifestGate, ModelFetcher, ModelManifest, NoFetch,
};
use crate::startup;
use crate::supervisor::ChildSupervisor;
use crate::verify::VerifyEngine;
use crate::DaemonError;
use namtor_layout::{InstallRoot, LayoutPaths};
use namtor_store::TemplateStore;
use namtor_worker::learned_pad::LearnedPadBackend;
use tracing_subscriber::EnvFilter;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

pub const USAGE: &str = "\
Usage: namtor daemon

Nam-tor authentication daemon. Started by systemd with no arguments.

Options:
  -h, --help       Print this help and exit
  -V, --version    Print the version and exit";

/// Entry point for `namtor daemon`. `args` are the arguments *after* the
/// subcommand name.
///
/// `cli_preamble::handle` runs before any tracing, model loading or socket
/// binding: `namtor daemon --version` must exit 0 without starting anything
/// (the release smoke test calls it, and a daemon boot there hung the
/// pipeline — runs 34802156299, 34804866795, 34807137454).
pub fn main(args: &[String]) -> ExitCode {
    if let Some(code) =
        namtor_layout::cli_preamble::handle("namtor daemon", env!("CARGO_PKG_VERSION"), USAGE, args)
    {
        return ExitCode::from(code);
    }
    init_tracing();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "daemon startup failed");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,namtor_daemon=info,namtor_pam=warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}

/// Install SIGTERM/SIGINT handlers that set [`SHUTDOWN`]. `Listener::accept_one`
/// polls with a short timeout and returns `Ok(None)` when it expires or a
/// signal interrupts it, so the accept loop regains control and observes the
/// flag. (`accept()` on a blocking listener retries `EINTR` internally and
/// would never hand control back.)
fn install_signal_handlers() {
    use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

    let action = SigAction::new(
        SigHandler::Handler(handle_signal),
        SaFlags::empty(),
        SigSet::empty(),
    );
    for sig in [Signal::SIGTERM, Signal::SIGINT] {
        let _ = unsafe { sigaction(sig, &action) };
    }
}

extern "C" fn handle_signal(_sig: nix::libc::c_int) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

/// The three models the daemon requires, named once so the list it asks the
/// manifest for and the roles it binds them to cannot drift apart.
pub const MODEL_DETECTOR: &str = "yunet_2023mar";
pub const MODEL_EMBEDDER: &str = "sface_2021dec";
pub const MODEL_PAD: &str = "minifasnet_pad";

/// Take one validated model out of the set by id.
///
/// `resolve_required_specs` already fails when a required id is absent, so
/// this is unreachable in practice — which is exactly why it must not be an
/// index into a vector. An error here names the model; a wrong index would
/// silently hand the detector the PAD weights.
pub fn take_model(
    validated: &mut std::collections::HashMap<String, PathBuf>,
    id: &str,
) -> Result<PathBuf, String> {
    validated
        .remove(id)
        .ok_or_else(|| format!("model '{id}' was required but not validated"))
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "namtor daemon starting"
    );

    // Before any child is spawned: systemd hands activation listeners over
    // with CLOEXEC *cleared* (sd_listen_fds semantics), and the worker is
    // spawned before the listener is adopted. An inherited activation socket
    // would pin the bind path: a restarted daemon could not re-bind while the
    // stale worker still held the socket. Every descriptor the supervisor
    // creates is close-on-exec already, so marking the activation range here
    // is sufficient — no generic descriptor sweep.
    if let Some(count) = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|c| c.parse::<u32>().ok())
    {
        let marked = mark_listen_fds_cloexec(count, 3);
        tracing::debug!(count, marked, "activation listeners marked close-on-exec");
    }

    let layout = LayoutPaths::resolve(
        std::path::Path::new("/"),
        std::path::Path::new("/usr/share/namtor/install-manifest.toml"),
        InstallRoot::Installed,
    );

    let socket_path = std::env::var("NAMTOR_SOCKET")
        .map(PathBuf::from)
        .unwrap_or(layout.daemon_socket);
    let models_dir = std::env::var("NAMTOR_MODELS_DIR")
        .map(PathBuf::from)
        .unwrap_or(layout.models_dir);
    let store_dir = std::env::var("NAMTOR_STORE_DIR")
        .map(PathBuf::from)
        .unwrap_or(layout.templates_dir);
    let key_path = std::env::var("NAMTOR_KEY_PATH")
        .map(PathBuf::from)
        .unwrap_or(layout.key_path);
    // Fallback only: `ChildSupervisor` re-execs `/proc/self/exe` so the
    // worker is this same executable. This path is used when /proc is not
    // mounted.
    let worker_bin = std::env::var("NAMTOR_WORKER_BIN")
        .map(PathBuf::from)
        .unwrap_or(layout.namtor_bin);
    let backoff_path = std::env::var("NAMTOR_BACKOFF_PATH")
        .map(PathBuf::from)
        .unwrap_or(layout.backoff_path);

    let yunet_path: PathBuf;
    let sface_path: PathBuf;
    let pad_path: PathBuf;
    let model_specs = {
        let model_manifest_path = std::env::var("NAMTOR_MODEL_MANIFEST")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                layout
                    .manifest_path
                    .parent()
                    .map(|p| p.join("model-manifest.toml"))
                    .unwrap_or_else(|| PathBuf::from("/usr/share/namtor/model-manifest.toml"))
            });

        let manifest = ModelManifest::parse(&model_manifest_path)?;
        tracing::info!(path = %model_manifest_path.display(), "model manifest parsed");

        let required_ids: [&str; 3] = [MODEL_DETECTOR, MODEL_EMBEDDER, MODEL_PAD];
        let specs = resolve_required_specs(&manifest, &required_ids)?;

        let fetcher = ModelFetcher::new(&models_dir, Box::new(NoFetch));
        let accepted_by = std::env::var("USER").unwrap_or_else(|_| "namtor".into());
        let gate = ManifestGate {
            manifest: &manifest,
            accepted_by: &accepted_by,
            allow_restricted: false,
        };

        // Keyed by model id, not by position. Binding by index meant the
        // order of `required_ids` and the order of these three assignments
        // had to agree, with nothing to enforce it: reorder one and the
        // daemon loads the PAD network as the face detector and boots
        // "successfully".
        let mut validated: std::collections::HashMap<String, PathBuf> =
            std::collections::HashMap::new();
        for spec in &specs {
            let path = fetcher.validate_against_manifest(&gate, spec)?;
            tracing::info!(
                model_id = %spec.id,
                sha256 = %hex(&spec.sha256),
                path = %path.display(),
                "model validated"
            );
            validated.insert(spec.id.clone(), path);
        }
        tracing::info!(count = validated.len(), "all models validated");

        yunet_path = take_model(&mut validated, MODEL_DETECTOR)?;
        sface_path = take_model(&mut validated, MODEL_EMBEDDER)?;
        pad_path = take_model(&mut validated, MODEL_PAD)?;
        specs
    };

    let mock_infer = std::env::var("NAMTOR_MOCK_INFER").is_ok();
    // Report the executable that will ACTUALLY run. `ChildSupervisor` re-execs
    // `/proc/self/exe`, so logging `worker_bin` announced a path that is
    // normally not used — on a dev run it named /usr/bin/namtor while the
    // worker was the binary in target/release, which sends anyone diagnosing a
    // worker problem to the wrong file.
    let resolved_worker =
        std::fs::read_link("/proc/self/exe").unwrap_or_else(|_| worker_bin.clone());
    tracing::info!(
        worker = %resolved_worker.display(),
        fallback = %worker_bin.display(),
        mock_infer,
        "spawning worker"
    );
    let supervisor = ChildSupervisor::spawn(&worker_bin, &model_specs, mock_infer, &models_dir)?;

    tracing::info!(store = %store_dir.display(), "opening template store");
    let learned_pad = namtor_worker::learned_pad::OnnxCpuLearnedPad::load(&pad_path)
        .map_err(|e| {
            tracing::error!(error = %e, path = %pad_path.display(), "learned PAD model load failed");
            e
        })?;
    tracing::info!(path = %pad_path.display(), "learned PAD model loaded");
    let verify = VerifyEngine::new(&yunet_path, &sface_path, &store_dir, &key_path)?
        .with_learned_pad(Box::new(learned_pad));

    let capture_factory = if std::env::var("NAMTOR_NO_CAMERA").is_ok() {
        tracing::info!("camera disabled (NAMTOR_NO_CAMERA)");
        None
    } else {
        let configured = std::env::var("NAMTOR_CAMERA_DEV").ok();
        tracing::info!(
            camera = configured.as_deref().unwrap_or("auto"),
            "camera enabled"
        );
        Some(live_capture::factory_with_configured(configured))
    };

    let daemon = Daemon::new(
        supervisor,
        &model_specs,
        Duration::from_secs(30),
        SystemClock,
        capture_factory,
    )?;

    // Load persistent per-uid backoff state (spec §8.4). A missing file is the
    // normal first-boot state and yields an empty store; a corrupt file errors
    // out so an operator inspects it rather than silently resetting an
    // in-progress bruteforce defence.
    let backoff = BackoffStore::load(&backoff_path, BackoffConfig::default())?;
    tracing::info!(
        path = %backoff_path.display(),
        uids = backoff.uid_count(),
        "backoff store loaded"
    );

    let dispatcher = Arc::new(DaemonDispatcher::new(
        daemon,
        verify,
        backoff,
        Some(backoff_path),
        SystemClock,
    ));

    let auth_gid = resolve_namtor_auth_gid().unwrap_or_else(|_| {
        tracing::warn!("namtor-auth group not found — allowing all connections");
        0
    });

    // Enrollment gets its own camera factory: the auth path's factory was
    // moved into `Daemon` above, and holding one source open across both
    // would keep the emitter armed between enrollments.
    let enroll_sources = if std::env::var("NAMTOR_NO_CAMERA").is_ok() {
        None
    } else {
        let configured = std::env::var("NAMTOR_CAMERA_DEV").ok();
        let yunet = yunet_path.clone();
        let sface = sface_path.clone();
        let embed_factory: Manager3EmbedFactory = Box::new(move || {
            Ok(Box::new(PipelineEmbedSource::load(&yunet, &sface)?)
                as Box<dyn crate::manager::session::EmbedSource>)
        });
        Some((
            live_capture::factory_with_configured(configured),
            embed_factory,
        ))
    };

    let bus_service = start_manager_bus(&store_dir, &key_path, enroll_sources);

    let _disconnect_monitor = bus_service.as_ref().and_then(|(bus, svc, svc3)| {
        match bus.monitor_disconnects(svc.clone(), svc3.clone()) {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::warn!(error = %e, "disconnect monitor not started");
                None
            }
        }
    });

    // Close every open capture when the machine is about to suspend, so the
    // IR emitter is dark before sleep.
    //
    // Nothing did this. Both the auth capture (open through the 30 s warm
    // window after a PAM prepare) and any enrollment session could be armed
    // when the lid closed. `ReconcileGuard::handle_sleep_event` was written
    // for this and never constructed outside its own tests, and
    // `ResumeMonitor` was never started. The subscription holds a logind
    // delay inhibitor, without which logind does not wait for the handler.
    //
    // Resume needs no work here: both capture factories re-enumerate sysfs
    // every time they open, so a camera renumbered by the resume is found
    // fresh by the next auth or enrollment.
    {
        // The monitor holds only a `Weak`: it must not pin the dispatcher.
        // Teardown is explicit (`serve_until_shutdown` / the bind-error path
        // below) and must not wait for this thread to release an Arc — a
        // strong Arc here used to keep the worker alive through startup
        // errors (issue #31).
        let dispatcher = Arc::downgrade(&dispatcher);
        let svc3 = bus_service.as_ref().map(|(_, _, svc3)| Arc::clone(svc3));
        let spawned = std::thread::Builder::new()
            .name("namtor-sleep".into())
            .spawn(move || {
                let result =
                    namtor_camera::resume::ResumeMonitor::run_with_delay_inhibitor(|event| {
                        if event == namtor_camera::resume::SleepEvent::Suspend {
                            let Some(dispatcher) = dispatcher.upgrade() else {
                                // The dispatcher is already torn down: the
                                // daemon is exiting and there is no capture
                                // left to darken.
                                return;
                            };
                            dispatcher.release_for_suspend();
                            if let Some(svc3) = &svc3 {
                                let (cancelled, stuck) = svc3.cancel_all_sessions();
                                if stuck > 0 {
                                    // Say so loudly. A worker that would not
                                    // stop still owns its capture, so the IR
                                    // emitter it armed has nothing left to
                                    // darken it before the machine sleeps.
                                    tracing::error!(
                                        sessions = cancelled,
                                        stuck,
                                        "enrollment workers did not stop before suspend; \
                                         the IR emitter may remain armed"
                                    );
                                } else if cancelled > 0 {
                                    tracing::info!(
                                        sessions = cancelled,
                                        "enrollment sessions cancelled for suspend"
                                    );
                                }
                            }
                        } else {
                            tracing::info!("resumed; captures reopen on demand");
                        }
                    });
                // Log either way. A clean return means the signal stream
                // ended — the bus connection dropped — which silently disarms
                // suspend handling for the life of the process, and only the
                // `Err` arm used to say anything at all.
                match result {
                    Ok(()) => tracing::warn!(
                        "sleep monitor stopped: the system bus connection ended. \
                         Suspend will no longer darken the IR emitter until the \
                         daemon restarts."
                    ),
                    Err(e) => tracing::warn!(error = %e, "sleep monitor stopped"),
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "sleep monitor not started");
        }
    }

    let started = if std::env::var("LISTEN_FDS").is_ok() {
        tracing::info!(fd = 3, "adopting socket-activation listener");
        adopt_listener(3, socket_path, auth_gid, &dispatcher)?
    } else {
        bind_listener(&socket_path, auth_gid, &dispatcher)?
    };

    let listener = started.listener;

    install_signal_handlers();

    serve_until_shutdown(listener, &dispatcher);

    if let Some((bus, _svc, _svc3)) = bus_service {
        bus.shutdown();
    }

    Ok(())
}

/// Mark `count` socket-activation descriptors, starting at `first`,
/// close-on-exec. Missing descriptors are skipped: a partially consumed
/// activation set must not abort startup. Returns how many were marked.
fn mark_listen_fds_cloexec(count: u32, first: std::os::fd::RawFd) -> usize {
    let mut marked = 0;
    for offset in 0..count {
        let raw = first + offset as i32;
        // SAFETY: fcntl flag inspection on the descriptors systemd handed us
        // (sd_listen_fds semantics: `first..first+count` are valid open
        // listening sockets); both calls are async-signal-safe.
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
        if flags < 0 {
            continue;
        }
        // SAFETY: as above.
        if unsafe { libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) } >= 0 {
            marked += 1;
        }
    }
    marked
}

/// Bind the listener; on failure, tear the dispatcher down before returning.
///
/// A startup error after the worker was spawned used to leave the worker
/// running: the sleep monitor held a strong `Arc` to the dispatcher, so no
/// `Drop` ran before the process exited and destructors were skipped. The
/// teardown here is explicit and runs on the error path, whatever the
/// monitor thread is doing (issue #31).
fn bind_listener<S, C, V>(
    socket_path: &std::path::Path,
    auth_gid: u32,
    dispatcher: &Arc<DaemonDispatcher<S, C, V>>,
) -> Result<startup::Started, DaemonError>
where
    S: crate::supervisor::WorkerSupervisor + Send + 'static,
    C: Clock + Send + 'static,
    V: VerifyRunner + 'static,
{
    let dispatcher_for_listener: Arc<DaemonDispatcher<S, C, V>> = Arc::clone(dispatcher);
    let result = startup::bind(socket_path, auth_gid, dispatcher_for_listener);
    match result {
        Ok(started) => Ok(started),
        Err(e) => {
            tracing::warn!(error = %e, "listener startup failed; shutting the worker down");
            dispatcher.shutdown();
            Err(e)
        }
    }
}

/// Adopt the socket-activation listener; on failure, tear the dispatcher
/// down before returning — same rationale as [`bind_listener`].
fn adopt_listener<S, C, V>(
    fd: std::os::fd::RawFd,
    socket_path: std::path::PathBuf,
    auth_gid: u32,
    dispatcher: &Arc<DaemonDispatcher<S, C, V>>,
) -> Result<startup::Started, DaemonError>
where
    S: crate::supervisor::WorkerSupervisor + Send + 'static,
    C: Clock + Send + 'static,
    V: VerifyRunner + 'static,
{
    let dispatcher_for_listener: Arc<DaemonDispatcher<S, C, V>> = Arc::clone(dispatcher);
    // `from_fd` is safe here because, with `LISTEN_FDS` set, systemd has
    // passed a bound AF_UNIX listener at `fd` (sd_listen_fds semantics).
    let result = startup::from_fd(fd, socket_path, auth_gid, dispatcher_for_listener);
    match result {
        Ok(started) => Ok(started),
        Err(e) => {
            tracing::warn!(error = %e, "listener adoption failed; shutting the worker down");
            dispatcher.shutdown();
            Err(e)
        }
    }
}

/// Run the accept loop until the shutdown flag is observed, then tear the
/// dispatcher down explicitly — not through whichever thread happens to drop
/// the last `Arc` (issue #31).
fn serve_until_shutdown<S, C, V>(listener: Listener, dispatcher: &Arc<DaemonDispatcher<S, C, V>>)
where
    S: crate::supervisor::WorkerSupervisor + Send + 'static,
    C: Clock + Send + 'static,
    V: VerifyRunner + 'static,
{
    loop {
        if SHUTDOWN.load(Ordering::Relaxed) {
            tracing::info!("shutdown signal received, stopping");
            break;
        }
        match listener.accept_one() {
            // Connection served.
            Ok(Some(())) => {}
            // Poll timed out or a signal interrupted it: re-check SHUTDOWN.
            Ok(None) => continue,
            Err(e) => {
                if SHUTDOWN.load(Ordering::Relaxed) {
                    tracing::info!("shutdown signal received during accept, stopping");
                    break;
                }
                tracing::warn!(error = %e, "accept error");
            }
        }
    }
    dispatcher.shutdown();
}

type ManagerBusHandle = (
    ManagerBusService,
    Arc<ManagerService<SystemPolkit>>,
    Arc<Manager3Service<SystemPolkit>>,
);

/// Start the `org.namtor.Manager` service on the system bus. Returns `None`
/// and logs a warning if the bus is unavailable or the name is already taken.
fn start_manager_bus(
    store_dir: &std::path::Path,
    key_path: &std::path::Path,
    enroll_sources: Option<(crate::daemon::CaptureFactory, Manager3EmbedFactory)>,
) -> Option<ManagerBusHandle> {
    let store = match TemplateStore::open(store_dir, key_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::warn!(error = %e, "template store open failed (manager bus not started)");
            return None;
        }
    };
    let profile_store: Arc<dyn ProfileStore> = Arc::new(TemplateProfileStore::new(store));
    let svc = Arc::new(ManagerService::new_with_store(
        SystemPolkit::default(),
        profile_store.clone(),
    ));
    let pamd_root = std::path::PathBuf::from("/etc/pam.d");
    let registry_root = std::path::PathBuf::from("/var/lib/namtor");
    let svc2 = Arc::new(Manager2Service::new(
        SystemPolkit::default(),
        profile_store.clone(),
        pamd_root,
        registry_root,
    ));
    // Without these factories `StartEnrollment` creates a session whose
    // first pump fails with "no capture source for session" — which is what
    // the GUI enrollment path did before task 3.6.
    let svc3 = Arc::new(match enroll_sources {
        Some((capture_factory, embed_factory)) => Manager3Service::new_with_capture(
            SystemPolkit::default(),
            profile_store.clone(),
            capture_factory,
            embed_factory,
        ),
        None => {
            tracing::warn!("no camera configured; GUI enrollment will capture no frames");
            Manager3Service::new(SystemPolkit::default(), profile_store.clone())
        }
    });
    match ManagerBusService::start_system(svc.clone(), svc2, svc3.clone()) {
        Ok(bus) => Some((bus, svc, svc3)),
        Err(e) => {
            tracing::warn!(error = %e, "manager d-bus service not started (pam path continues)");
            None
        }
    }
}

#[cfg(test)]
mod model_binding_tests {
    use super::{take_model, MODEL_DETECTOR, MODEL_EMBEDDER, MODEL_PAD};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn validated() -> HashMap<String, PathBuf> {
        [
            (MODEL_PAD, "/models/minifasnet_pad.onnx"),
            (MODEL_DETECTOR, "/models/yunet_2023mar.onnx"),
            (MODEL_EMBEDDER, "/models/sface_2021dec.onnx"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), PathBuf::from(v)))
        .collect()
    }

    /// The point of the change: the order the models were validated in no
    /// longer decides which role each one gets. The map above is built in
    /// PAD-first order on purpose.
    #[test]
    fn each_role_gets_its_own_model_whatever_the_order() {
        let mut v = validated();
        assert_eq!(
            take_model(&mut v, MODEL_DETECTOR).unwrap(),
            PathBuf::from("/models/yunet_2023mar.onnx")
        );
        assert_eq!(
            take_model(&mut v, MODEL_EMBEDDER).unwrap(),
            PathBuf::from("/models/sface_2021dec.onnx")
        );
        assert_eq!(
            take_model(&mut v, MODEL_PAD).unwrap(),
            PathBuf::from("/models/minifasnet_pad.onnx")
        );
        assert!(v.is_empty(), "left over: {v:?}");
    }

    /// A missing model names itself rather than panicking on an index.
    #[test]
    fn a_missing_model_is_named() {
        let mut v = validated();
        v.remove(MODEL_PAD);
        let err = take_model(&mut v, MODEL_PAD).unwrap_err();
        assert!(err.contains(MODEL_PAD), "{err}");
    }

    /// The three ids are distinct; two roles sharing one id would load the
    /// same weights twice and look like it worked.
    #[test]
    fn the_three_model_ids_are_distinct() {
        let ids = [MODEL_DETECTOR, MODEL_EMBEDDER, MODEL_PAD];
        let unique: std::collections::BTreeSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "{ids:?}");
    }
}

#[cfg(test)]
mod teardown_tests {
    use super::*;
    use crate::backoff::{BackoffConfig, BackoffStore};
    use crate::dispatch::VerifyRunner;
    use crate::lifecycle::MockClock;
    use crate::supervisor::ShutdownCountingSupervisor;
    use crate::verify::VerifyOutcome;
    use namtor_capture::source::CaptureSource;
    use namtor_proto::worker::{ModelSpec, Quant};
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// Runner that never runs — teardown tests never dispatch a verify.
    struct NoVerify;
    impl VerifyRunner for NoVerify {
        fn run_verify(
            &mut self,
            _capture: &mut dyn CaptureSource,
            _target_uid: u32,
            _deadline: Instant,
        ) -> VerifyOutcome {
            VerifyOutcome::unavailable("test")
        }
    }

    type TestDispatcher = DaemonDispatcher<ShutdownCountingSupervisor, MockClock, NoVerify>;

    fn models() -> Vec<ModelSpec> {
        vec![ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        }]
    }

    fn dispatcher() -> (Arc<TestDispatcher>, Arc<AtomicUsize>) {
        let sup = ShutdownCountingSupervisor::new();
        let counter = sup.shutdowns.clone();
        let clock = MockClock::new(Instant::now());
        let daemon =
            Daemon::new(sup, &models(), Duration::from_secs(30), clock.clone(), None).unwrap();
        let disp = Arc::new(DaemonDispatcher::new(
            daemon,
            NoVerify,
            BackoffStore::new(BackoffConfig::default()),
            None,
            clock,
        ));
        (disp, counter)
    }

    /// A bind failure after the worker was spawned must tear the worker down
    /// *on the error path* — not via a Drop racing the sleep monitor's Arc.
    #[test]
    fn bind_failure_tears_the_worker_supervisor_down() {
        let (disp, counter) = dispatcher();
        let result = bind_listener(
            std::path::Path::new("/proc/namtor-cannot-bind-here.sock"),
            1000,
            &disp,
        );
        assert!(result.is_err(), "bind in /proc must fail");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "startup failure must shut the worker supervisor down deterministically"
        );
    }

    /// Normal termination (shutdown flag observed by the accept loop) must
    /// shut the worker supervisor down explicitly, not through a last-Arc
    /// Drop.
    #[test]
    fn normal_termination_tears_the_worker_supervisor_down() {
        let (disp, counter) = dispatcher();
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("auth.sock");
        let started = bind_listener(&path, 1000, &disp).expect("bind");
        SHUTDOWN.store(true, Ordering::Relaxed);
        serve_until_shutdown(started.listener, &disp);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "normal termination must shut the worker supervisor down deterministically"
        );
    }

    /// systemd hands activation listeners over with CLOEXEC *cleared*; before
    /// any child is spawned the daemon must mark them close-on-exec, or the
    /// worker pins the activation socket across exec.
    #[test]
    fn activation_listener_is_marked_cloexec_before_any_spawn() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let sock = std::os::unix::net::UnixListener::bind(dir.path().join("s.sock"))
            .expect("bind test listener");
        let raw = sock.as_raw_fd();
        // Simulate systemd: activation fds arrive with the flag cleared.
        // SAFETY: fcntl flag manipulation on our own open descriptor.
        unsafe {
            let flags = libc::fcntl(raw, libc::F_GETFD);
            assert!(flags >= 0);
            assert_eq!(
                libc::fcntl(raw, libc::F_SETFD, flags & !libc::FD_CLOEXEC),
                0
            );
        }

        let marked = mark_listen_fds_cloexec(1, raw);
        assert_eq!(marked, 1, "the handed-over listener must be marked");
        // SAFETY: read-back of our own descriptor's flags.
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "activation listener must carry FD_CLOEXEC before the worker is spawned"
        );
    }

    /// Missing descriptors in the activation range are skipped, not fatal —
    /// a partially consumed set must not abort startup.
    #[test]
    fn marking_tolerates_missing_descriptors() {
        assert_eq!(mark_listen_fds_cloexec(1, -1), 0);
    }
}
