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
use namtor_daemon::daemon::Daemon;
use namtor_daemon::dbus::{ManagerService, SystemPolkit};
use namtor_daemon::dispatch::DaemonDispatcher;
use namtor_daemon::lifecycle::SystemClock;
use namtor_daemon::manager::bus::ManagerBusService;
use namtor_daemon::manager::store::{ProfileStore, TemplateProfileStore};
use namtor_daemon::startup;
use namtor_daemon::supervisor::ChildSupervisor;
use namtor_daemon::verify::VerifyEngine;
use namtor_store::TemplateStore;
use tracing_subscriber::EnvFilter;

#[path = "namtor-daemon/live_capture.rs"]
mod live_capture;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

fn main() -> ExitCode {
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

/// Install SIGTERM/SIGINT handlers that set [`SHUTDOWN`]. The blocking
/// `accept()` in the listener returns `EINTR` when a signal arrives, so
/// the accept loop wakes and checks the flag.
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

    let socket_path = PathBuf::from(
        std::env::var("NAMTOR_SOCKET").unwrap_or_else(|_| "/run/namtor/auth.sock".into()),
    );
    let models_dir = PathBuf::from(
        std::env::var("NAMTOR_MODELS_DIR").unwrap_or_else(|_| "/var/lib/namtor/models".into()),
    );
    let store_dir = PathBuf::from(
        std::env::var("NAMTOR_STORE_DIR").unwrap_or_else(|_| "/var/lib/namtor/templates".into()),
    );
    let key_path = PathBuf::from(
        std::env::var("NAMTOR_KEY_PATH").unwrap_or_else(|_| "/var/lib/namtor/templates.key".into()),
    );
    let worker_bin = PathBuf::from(
        std::env::var("NAMTOR_WORKER_BIN").unwrap_or_else(|_| "namtor-worker".into()),
    );

    let yunet_path = models_dir.join("yunet_2023mar.onnx");
    let sface_path = models_dir.join("sface_2021dec.onnx");

    if !yunet_path.exists() {
        return Err(format!("YuNet model not found: {}", yunet_path.display()).into());
    }
    if !sface_path.exists() {
        return Err(format!("SFace model not found: {}", sface_path.display()).into());
    }

    let model_specs = vec![
        namtor_proto::worker::ModelSpec {
            id: "yunet_2023mar".into(),
            quant: namtor_proto::worker::Quant::Fp32,
            sha256: [0; 32],
        },
        namtor_proto::worker::ModelSpec {
            id: "sface_2021dec".into(),
            quant: namtor_proto::worker::Quant::Fp32,
            sha256: [0; 32],
        },
    ];
    let mock_infer = std::env::var("NAMTOR_MOCK_INFER").is_ok();
    tracing::info!(worker = %worker_bin.display(), mock_infer, "spawning worker");
    let supervisor = ChildSupervisor::spawn(&worker_bin, &model_specs, mock_infer, &models_dir)?;

    tracing::info!(store = %store_dir.display(), "opening template store");
    let verify = VerifyEngine::new(&yunet_path, &sface_path, &store_dir, &key_path)?;

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

    let dispatcher = Arc::new(DaemonDispatcher::new(daemon, verify));

    let auth_gid = resolve_namtor_auth_gid().unwrap_or_else(|_| {
        tracing::warn!("namtor-auth group not found — allowing all connections");
        0
    });

    let bus_service = start_manager_bus(&store_dir, &key_path);

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
        if let Err(e) = listener.accept_one() {
            if SHUTDOWN.load(Ordering::Relaxed) {
                tracing::info!("shutdown signal received during accept, stopping");
                break;
            }
            tracing::warn!(error = %e, "accept error");
        }
    }

    if let Some(bus) = bus_service {
        bus.shutdown();
    }

    Ok(())
}

/// Start the `org.namtor.Manager` service on the system bus. Returns `None`
/// and logs a warning if the bus is unavailable or the name is already taken.
fn start_manager_bus(
    store_dir: &std::path::Path,
    key_path: &std::path::Path,
) -> Option<ManagerBusService> {
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
        profile_store,
    ));
    match ManagerBusService::start_system(svc) {
        Ok(bus) => Some(bus),
        Err(e) => {
            tracing::warn!(error = %e, "manager d-bus service not started (pam path continues)");
            None
        }
    }
}
