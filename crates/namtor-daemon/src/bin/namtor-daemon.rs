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

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use namtor_daemon::acl::resolve_namtor_auth_gid;
use namtor_daemon::daemon::Daemon;
use namtor_daemon::dispatch::DaemonDispatcher;
use namtor_daemon::lifecycle::SystemClock;
use namtor_daemon::listener::Listener;
use namtor_daemon::supervisor::ChildSupervisor;
use namtor_daemon::verify::VerifyEngine;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("namtor-daemon: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
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
    let supervisor = ChildSupervisor::spawn(&worker_bin, &model_specs, mock_infer, &models_dir)?;

    let verify = VerifyEngine::new(&yunet_path, &sface_path, &store_dir, &key_path)?;

    let capture_factory: Option<namtor_daemon::daemon::CaptureFactory> =
        if std::env::var("NAMTOR_NO_CAMERA").is_ok() {
            None
        } else {
            Some(Box::new(|| {
                use namtor_capture::frame::PixFmt;
                use namtor_capture::ir_uvc::{IrUvc, V4l2IrStream};
                use namtor_capture::source::{CaptureSource, SourceCaps, Spectrum};

                let streamer = V4l2IrStream::open(
                    std::path::Path::new("/dev/video2"),
                    Some(std::path::Path::new("/dev/video3")),
                    640,
                    360,
                )
                .map_err(|e| namtor_daemon::DaemonError::Capture(e.to_string()))?;
                let caps = SourceCaps {
                    spectrum: Spectrum::Ir,
                    format: PixFmt::Grey8,
                    width: 640,
                    height: 360,
                    fps: 30,
                    has_metadata: true,
                };
                struct UnknownPolicy;
                impl namtor_capture::ir_uvc::EmitterPolicy for UnknownPolicy {
                    fn tag(&self, _frame_index: u64) -> namtor_capture::frame::EmitterState {
                        namtor_capture::frame::EmitterState::Unknown
                    }
                }
                let mut source = IrUvc::new(streamer, UnknownPolicy, caps);
                source
                    .start()
                    .map_err(|e| namtor_daemon::DaemonError::Capture(e.to_string()))?;
                Ok(Box::new(source) as Box<dyn CaptureSource>)
            }))
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
        eprintln!("namtor-daemon: warning — namtor-auth group not found, allowing all");
        0
    });

    eprintln!("namtor-daemon: listening on {}", socket_path.display());

    let listener = if std::env::var("LISTEN_FDS").is_ok() {
        Listener::from_fd(3, socket_path, auth_gid, dispatcher)?
    } else {
        Listener::bind(&socket_path, auth_gid, dispatcher)?
    };

    loop {
        if let Err(e) = listener.accept_one() {
            eprintln!("namtor-daemon: accept error: {e}");
        }
    }
}
