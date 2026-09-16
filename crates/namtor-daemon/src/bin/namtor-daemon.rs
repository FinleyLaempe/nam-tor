// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-daemon` binary entry point.
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

use namtor_daemon::acl::resolve_namtor_auth_gid;
use namtor_daemon::backoff::{BackoffConfig, BackoffStore};
use namtor_daemon::daemon::Daemon;
use namtor_daemon::dbus::{ManagerService, SystemPolkit};
use namtor_daemon::dispatch::DaemonDispatcher;
use namtor_daemon::lifecycle::SystemClock;
use namtor_daemon::manager::bus::ManagerBusService;
use namtor_daemon::manager::dbus::{Manager2Service, Manager3EmbedFactory, Manager3Service};
use namtor_daemon::manager::embed::PipelineEmbedSource;
use namtor_daemon::manager::store::{ProfileStore, TemplateProfileStore};
use namtor_daemon::models::{
    hex, resolve_required_specs, ManifestGate, ModelFetcher, ModelManifest, NoFetch,
};
use namtor_daemon::startup;
use namtor_daemon::supervisor::ChildSupervisor;
use namtor_daemon::verify::VerifyEngine;
use namtor_layout::{InstallRoot, LayoutPaths};
use namtor_store::TemplateStore;
use namtor_worker::learned_pad::LearnedPadBackend;
use tracing_subscriber::EnvFilter;

#[path = "namtor-daemon/live_capture.rs"]
mod live_capture;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

const USAGE: &str = "\
Usage: namtor-daemon

Nam-tor authentication daemon. Started by systemd with no arguments.

Options:
  -h, --help       Print this help and exit
  -V, --version    Print the version and exit";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = namtor_layout::cli_preamble::handle(
        "namtor-daemon",
        env!("CARGO_PKG_VERSION"),
        USAGE,
        &args,
    ) {
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

fn run() -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "namtor-daemon starting"
    );

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
    let worker_bin = std::env::var("NAMTOR_WORKER_BIN")
        .map(PathBuf::from)
        .unwrap_or(layout.worker_bin);
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

        let required_ids: [&str; 3] = ["yunet_2023mar", "sface_2021dec", "minifasnet_pad"];
        let specs = resolve_required_specs(&manifest, &required_ids)?;

        let fetcher = ModelFetcher::new(&models_dir, Box::new(NoFetch));
        let accepted_by = std::env::var("USER").unwrap_or_else(|_| "namtor".into());
        let gate = ManifestGate {
            manifest: &manifest,
            accepted_by: &accepted_by,
            allow_restricted: false,
        };

        let mut validated_paths: Vec<PathBuf> = Vec::new();
        for spec in &specs {
            let path = fetcher.validate_against_manifest(&gate, spec)?;
            tracing::info!(
                model_id = %spec.id,
                sha256 = %hex(&spec.sha256),
                path = %path.display(),
                "model validated"
            );
            validated_paths.push(path);
        }
        tracing::info!(count = validated_paths.len(), "all models validated");

        yunet_path = validated_paths[0].clone();
        sface_path = validated_paths[1].clone();
        pad_path = validated_paths[2].clone();
        specs
    };

    let mock_infer = std::env::var("NAMTOR_MOCK_INFER").is_ok();
    tracing::info!(worker = %worker_bin.display(), mock_infer, "spawning worker");
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
                as Box<dyn namtor_daemon::manager::session::EmbedSource>)
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

    let started = if std::env::var("LISTEN_FDS").is_ok() {
        tracing::info!(fd = 3, "adopting socket-activation listener");
        startup::from_fd(3, socket_path, auth_gid, dispatcher)?
    } else {
        startup::bind(&socket_path, auth_gid, dispatcher)?
    };

    let listener = started.listener;

    install_signal_handlers();

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

    if let Some((bus, _svc, _svc3)) = bus_service {
        bus.shutdown();
    }

    Ok(())
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
    enroll_sources: Option<(namtor_daemon::daemon::CaptureFactory, Manager3EmbedFactory)>,
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
