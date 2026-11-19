// SPDX-License-Identifier: GPL-3.0-only
//! The daemon's worker supervisor drives a *separate process* that happens to
//! be the same file: `ChildSupervisor` re-execs the running executable as
//! `namtor worker`. This suite exercises that against the built `namtor`
//! binary (a test harness's own `/proc/self/exe` is the harness, so
//! `spawn_with_exe` is used to point at the real executable).

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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

// ---------------------------------------------------------------------------
// Worker-lifetime process regressions (issue #31).
//
// The daemon and the worker are separate processes sharing one executable.
// Two properties must hold regardless of how the daemon's life ends:
//
// 1. *Descriptor retention* — the worker must hold exactly its three IPC
//    descriptors (`--read-fd`, `--write-fd`, `--frame-memfd`). If it also
//    inherits the daemon-side endpoints (`req_w`, `rsp_r`), its own copy of
//    `req_w` keeps the request pipe open and it can never observe EOF: the
//    worker then outlives the daemon indefinitely.
// 2. *Parent death* — a SIGKILLed daemon must leave no orphaned worker, even
//    though SIGKILL runs no destructors and closes nothing gracefully.
//
// The tests re-exec this harness as the worker's *parent* ("stage"). The
// stage spawns the worker through the real `ChildSupervisor`, publishes the
// worker's pid and its own socket endpoints, then `exec`s `/bin/sleep`:
// exec keeps the stage process alive (so `PR_SET_PDEATHSIG` cannot fire) but
// — with the fix — closes the stage's CLOEXEC-marked descriptors, which is
// exactly what isolates the EOF mechanism from the parent-death mechanism.
// ---------------------------------------------------------------------------

/// Set on the re-exec'd stage; unset in the orchestrating test.
const STAGE_ENV: &str = "NAMTOR_TEST_WORKER_PARENT";

fn stage_requested() -> bool {
    std::env::var(STAGE_ENV).as_deref() == Ok("1")
}

/// Socket endpoint inodes of `pid`'s open descriptors.
fn socket_inodes(pid: u32) -> Vec<u64> {
    let mut inodes = Vec::new();
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return inodes;
    };
    for entry in entries.flatten() {
        let Ok(link) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let link = link.to_string_lossy().into_owned();
        let Some(rest) = link.strip_prefix("socket:[") else {
            continue;
        };
        let Some(num) = rest.strip_suffix(']') else {
            continue;
        };
        if let Ok(inode) = num.parse() {
            inodes.push(inode);
        }
    }
    inodes
}

/// Whether `pid` still holds a descriptor for the shared frame memfd.
fn holds_frame_memfd(pid: u32) -> bool {
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return false;
    };
    entries.flatten().any(|e| {
        std::fs::read_link(e.path())
            .map(|l| l.to_string_lossy().contains("memfd:namtor-frames"))
            .unwrap_or(false)
    })
}

fn pid_alive(pid: u32) -> bool {
    // A zombie has already exited — descriptors closed, nothing will ever
    // serve again — it is merely unreaped because the stage (a sleeping
    // `sleep`) never calls wait(). For the lifetime assertions a zombie is as
    // good as gone; only a live process (state != Z) counts.
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => {
            let after_comm = stat.rsplit_once(')').map(|(_, rest)| rest).unwrap_or("");
            after_comm.split_whitespace().next() != Some("Z")
        }
        Err(_) => false,
    }
}

/// Poll until `pid` is gone or the deadline passes. Returns the final state.
fn wait_pid_gone(pid: u32, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !pid_alive(pid)
}

/// The stage body. Runs inside the re-exec'd harness: spawns the worker via
/// the real supervisor, prints `WORKER_PID`, prints the stage's own socket
/// endpoints as `PARENT_SOCKETS`, then execs `/bin/sleep 60`. Never returns.
fn run_worker_parent_stage() -> ! {
    let dir = tempfile::tempdir().expect("stage tempdir");
    // No models: the worker preloads nothing, applies its sandbox, and blocks
    // on its request pipe — the idle lifetime under test. (The shipped binary
    // uses the ONNX backend; a dummy model spec would fail to load and the
    // supervisor would refuse to start.)
    // Kept alive to the exec point: dropping the supervisor would shut the
    // worker down through the very channel under test.
    let _sup = ChildSupervisor::spawn_with_exe(Path::new(namtor_bin()), &[], false, dir.path())
        .expect("stage spawns worker via ChildSupervisor");

    // Find the worker child through /proc rather than a supervisor accessor,
    // so the regression stays independent of supervisor internals. Children
    // are listed per *thread* (`/proc/<tgid>/task/<tid>/children`), and the
    // harness runs tests on worker threads — aggregate every thread's list.
    let mut children = String::new();
    for tid in std::fs::read_dir("/proc/self/task").expect("own task dir") {
        let tid = tid.expect("task entry").file_name();
        if let Ok(list) = std::fs::read_to_string(format!(
            "/proc/self/task/{}/children",
            tid.to_string_lossy()
        )) {
            children.push_str(&list);
        }
    }
    let worker_pid: u32 = children
        .split_whitespace()
        .next()
        .expect("worker is a live child of the stage")
        .parse()
        .expect("child pid parses");

    println!("WORKER_PID {worker_pid}");
    let sockets = socket_inodes(std::process::id())
        .into_iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    println!("PARENT_SOCKETS {sockets}");
    // The pipe buffers block-wise; exec loses everything still buffered.
    let _ = std::io::stdout().flush();

    // Stay alive without holding any Nam-tor descriptor past exec. Try the
    // two spellings sleep(1) has across merged-/usr and non-merged systems.
    let path = std::ffi::CString::new("/bin/sleep").expect("NUL-free");
    let alt = std::ffi::CString::new("/usr/bin/sleep").expect("NUL-free");
    let arg0 = std::ffi::CString::new("sleep").expect("NUL-free");
    let arg1 = std::ffi::CString::new("60").expect("NUL-free");
    let args = [arg0.clone(), arg1];
    // execv only returns on failure; on success the process image is replaced
    // and nothing after this line executes.
    if nix::unistd::execv(&path, &args).is_err() {
        let _ = nix::unistd::execv(&alt, &args);
    }
    eprintln!("stage: execv of sleep failed");
    std::process::exit(99);
}

/// Spawn the stage for `test_name` and collect its markers. The stage is the
/// same test binary re-run for a single `--exact` test with `STAGE_ENV` set.
fn spawn_stage(test_name: &str) -> (Child, u32, Vec<u64>) {
    let exe = std::env::current_exe().expect("current_exe");
    let mut stage = Command::new(exe)
        .arg(test_name)
        .arg("--exact")
        // The harness captures test stdout by default; the markers must go to
        // the real pipe or the orchestrator would read nothing.
        .arg("--nocapture")
        .env(STAGE_ENV, "1")
        // Socket-activation state, as systemd would set it for the daemon.
        // Every worker under test inherits it; only the activation test
        // asserts on it.
        .env("LISTEN_FDS", "1")
        .env("LISTEN_PID", std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn worker-parent stage");

    use std::io::BufRead;
    let stdout = stage.stdout.take().expect("stage stdout piped");
    let mut worker_pid = None;
    let mut parent_sockets = None;
    for line in std::io::BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if let Some(p) = line.strip_prefix("WORKER_PID ") {
            worker_pid = Some(p.parse().expect("worker pid parses"));
        }
        if let Some(s) = line.strip_prefix("PARENT_SOCKETS ") {
            parent_sockets = Some(
                s.split_whitespace()
                    .map(|t| t.parse().expect("socket inode parses"))
                    .collect(),
            );
        }
        if worker_pid.is_some() && parent_sockets.is_some() {
            break;
        }
    }
    let (Some(worker_pid), Some(parent_sockets)) = (worker_pid, parent_sockets) else {
        // The stage died before publishing: surface its exit status so the
        // failure names the stage, not the assertion.
        let status = stage.wait().expect("wait stage");
        panic!("stage did not publish markers; exited with {status}");
    };
    (stage, worker_pid, parent_sockets)
}

/// The stage (and its sleeper image) must not outlive the test.
fn kill_stage(stage: &mut Child) {
    let _ = stage.kill();
    let _ = stage.wait();
}

/// The stage's worker must be up and serving before the assertions run. By
/// the time `spawn_with_exe` returns the child has exec'd, so its descriptor
/// table is final; the wait only guards against /proc visibility lag.
fn wait_worker_descriptors(worker_pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && socket_inodes(worker_pid).len() < 2 {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The worker holds only its three IPC descriptors. A fourth socket endpoint
/// (a daemon-side `req_w` or `rsp_r`) is the descriptor-retention bug: it
/// keeps the request pipe open from the worker's own side, so EOF can never
/// reach it and the worker outlives the daemon.
#[test]
fn worker_holds_only_the_three_ipc_descriptors() {
    if stage_requested() {
        run_worker_parent_stage();
    }
    let (mut stage, worker_pid, parent_sockets) =
        spawn_stage("worker_holds_only_the_three_ipc_descriptors");
    wait_worker_descriptors(worker_pid);

    assert!(pid_alive(worker_pid), "worker died before inspection");
    let worker_sockets = socket_inodes(worker_pid);
    assert_eq!(
        worker_sockets.len(),
        2,
        "worker must hold exactly the request-read and reply-write endpoints, \
         got {worker_sockets:?} (parent endpoints: {parent_sockets:?})"
    );
    let overlap: Vec<u64> = worker_sockets
        .iter()
        .filter(|s| parent_sockets.contains(s))
        .copied()
        .collect();
    assert!(
        overlap.is_empty(),
        "worker shares daemon-side endpoints {overlap:?} — its own copy of the \
         daemon's request-write fd keeps the pipe open and EOF can never arrive"
    );
    assert!(
        holds_frame_memfd(worker_pid),
        "worker lost the frame memfd descriptor"
    );

    kill_stage(&mut stage);
}

/// Closing the daemon side of the request pipe must end the worker, even
/// though no `Shutdown` request was ever sent and the worker's parent is
/// still alive (the stage exec'd into `sleep`, so `PR_SET_PDEATHSIG` cannot
/// be the mechanism here — only EOF on `--read-fd` is).
#[test]
fn worker_exits_on_daemon_write_close_even_without_shutdown() {
    if stage_requested() {
        run_worker_parent_stage();
    }
    let (mut stage, worker_pid, _) =
        spawn_stage("worker_exits_on_daemon_write_close_even_without_shutdown");
    wait_worker_descriptors(worker_pid);
    assert!(pid_alive(worker_pid), "worker died before the close");

    // The stage has exec'd sleep by now: the daemon-side write end is closed
    // (CLOEXEC at exec). The worker must observe EOF and exit on its own.
    let gone = wait_pid_gone(worker_pid, Instant::now() + Duration::from_secs(15));
    assert!(
        gone,
        "worker {worker_pid} survived EOF on its request pipe; it still holds \
         a copy of the daemon's write end (descriptor retention)"
    );

    kill_stage(&mut stage);
}

/// SIGKILL of the worker's parent must take the worker down with it. SIGKILL
/// runs no destructors, so neither a graceful `Shutdown` request nor an EOF
/// write can happen — the kernel must deliver the parent-death signal.
#[test]
fn worker_exits_after_parent_sigkill() {
    if stage_requested() {
        run_worker_parent_stage();
    }
    let (mut stage, worker_pid, _) = spawn_stage("worker_exits_after_parent_sigkill");
    wait_worker_descriptors(worker_pid);
    assert!(pid_alive(worker_pid), "worker died before the SIGKILL");

    // SIGKILL the worker's parent (the stage process).
    let _ = stage.kill();
    let _ = stage.wait();

    let gone = wait_pid_gone(worker_pid, Instant::now() + Duration::from_secs(15));
    assert!(
        gone,
        "worker {worker_pid} outlived its SIGKILLed parent — no parent-death \
         signal and no EOF (its own fd copies keep both pipes open)"
    );
}

/// The worker serves no sockets: it must not carry socket-activation state
/// into its environment. A `LISTEN_PID` naming the daemon could otherwise
/// make an `sd_listen_fds`-style consumer in the worker adopt the daemon's
/// listeners.
#[test]
fn worker_does_not_inherit_socket_activation_environment() {
    if stage_requested() {
        run_worker_parent_stage();
    }
    let (mut stage, worker_pid, _) =
        spawn_stage("worker_does_not_inherit_socket_activation_environment");
    wait_worker_descriptors(worker_pid);
    assert!(pid_alive(worker_pid), "worker died before inspection");

    let environ = std::fs::read_to_string(format!("/proc/{worker_pid}/environ"))
        .expect("worker environ readable");
    let vars: Vec<&str> = environ.split('\0').filter(|v| !v.is_empty()).collect();
    for banned in ["LISTEN_FDS", "LISTEN_PID", "LISTEN_FDNAMES"] {
        assert!(
            !vars.iter().any(|v| v.starts_with(&format!("{banned}="))),
            "worker environment carries {banned}: {vars:?}"
        );
    }

    kill_stage(&mut stage);
}
