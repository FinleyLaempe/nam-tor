// SPDX-License-Identifier: GPL-3.0-only
//! The daemon's worker supervisor drives a *separate process* that happens to
//! be the same file: `ChildSupervisor` re-execs the running executable as
//! `namtor worker`. This suite exercises that against the built `namtor`
//! binary (a test harness's own `/proc/self/exe` is the harness, so
//! `spawn_with_exe` is used to point at the real executable).

use std::path::Path;

use namtor_daemon::supervisor::{ChildSupervisor, WorkerSupervisor};
use namtor_proto::worker::{FrameRef, ModelSpec, Quant};

fn namtor_bin() -> &'static str {
    env!("CARGO_BIN_EXE_namtor")
}

/// End-to-end over the re-exec: spawn the child, load a real ONNX model,
/// run one inference, shut down. Needs the bundled weights under
/// `var/lib/namtor/models/` (gitignored), so it is `#[ignore]`d; CI covers
/// the boundary structurally through `worker_subcommand_is_reachable_*` and
/// the three sandbox escape suites.
#[test]
#[ignore = "requires the bundled ONNX weights under var/lib/namtor/models; run with NAMTOR_E2E_TEST=1"]
fn spawn_load_infer_shutdown_over_the_reexec() {
    let models_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../var/lib/namtor/models");
    let models = vec![ModelSpec {
        id: "yunet_2023mar".into(),
        quant: Quant::Fp32,
        sha256: [0; 32],
    }];

    let mut sup =
        ChildSupervisor::spawn_with_exe(Path::new(namtor_bin()), &models, false, &models_dir)
            .expect("spawn");

    let loaded = sup.load_all(&models).expect("load_all");
    assert_eq!(loaded.len(), 1);

    // YuNet takes 1x3xHxW f32; size the frame from what the worker reported.
    let n: usize = loaded[0]
        .input_shape
        .iter()
        .map(|d| (*d as usize).max(1))
        .product();
    let frame = vec![0u8; n * 4];
    let frame_ref = FrameRef {
        offset: 0,
        len: (n * 4) as u32,
        emitter: namtor_capture::frame::EmitterState::On,
        spectrum: namtor_capture::source::Spectrum::Ir,
        pix_fmt: namtor_capture::frame::PixFmt::Grey8,
        w: 320,
        h: 320,
        ts_us: 0,
    };
    let vec = sup.infer(0, &frame_ref, &frame).expect("infer");
    assert!(!vec.is_empty(), "inference should return a vector");

    sup.shutdown().expect("shutdown");
}

/// The worker subcommand must exist in the *same* executable the daemon
/// re-execs, and must refuse to serve without its fd pair. This is the cheap
/// always-on proof that `namtor worker` is reachable as a separate process.
#[test]
fn worker_subcommand_is_reachable_and_requires_fds() {
    let out = std::process::Command::new(namtor_bin())
        .arg("worker")
        .output()
        .expect("spawn namtor worker");
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--read-fd"), "stderr: {err}");
}
