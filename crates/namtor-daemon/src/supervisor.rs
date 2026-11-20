// SPDX-License-Identifier: GPL-3.0-only
//! Worker supervisor: re-execs this executable as `namtor worker` in a child
//! process, establishes the pipe + memfd frame channel, and forwards
//! Load/Infer requests.
//!
//! The daemon (non-root, user `namtor`) spawns the worker directly as a
//! child process — the worker inherits the daemon's UID (`namtor`). The
//! worker self-applies seccomp + landlock; the sandbox is the privilege
//! boundary, not a UID drop.
//!
//! `--apply-sandbox` is passed *unconditionally* — there is no opt-out and no
//! test-only bypass. Because Landlock denies every filesystem path, the worker
//! cannot open a `.onnx` file once it has sealed itself, so the daemon also
//! names every model on the command line (`--model <id>:<quant>:<sha256>`);
//! the worker loads those weights first and only then applies the sandbox.
//! The `Load` requests below are answered from that preloaded set.
//!
//! Daemon and worker are the *same file* (one shipped `namtor` executable)
//! but remain *separate processes*: merging the worker into the daemon's
//! address space would delete the isolation that keeps an inference-side
//! compromise away from templates, keys and the PAM socket. The child is
//! started from `/proc/self/exe` so a renamed, moved or still-unlinked
//! executable re-execs itself correctly; `worker_bin` is the fallback when
//! `/proc` is not mounted (e.g. a minimal container).

use std::ffi::OsString;
use std::io::{Seek, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use namtor_proto::framing::{decode, encode};
use namtor_proto::worker::{FrameRef, Loaded, ModelSpec, WorkerReq, WorkerRsp};

use crate::DaemonError;

/// Trait seam for the worker supervisor, so the daemon's boot logic (Task 19)
/// can be tested with a mock instead of a real child process.
pub trait WorkerSupervisor: Send {
    /// Load all models; returns when the worker has acked each Load.
    fn load_all(&self, models: &[ModelSpec]) -> Result<Vec<Loaded>, DaemonError>;
    /// Run inference on a frame; returns the embedding vector.
    fn infer(
        &mut self,
        model_idx: usize,
        frame_ref: &FrameRef,
        frame: &[u8],
    ) -> Result<Vec<f32>, DaemonError>;
    /// Shut down the worker cleanly.
    fn shutdown(&mut self) -> Result<(), DaemonError>;
}

/// A mock supervisor for testing (Task 19 boot/cold-camera test).
#[derive(Default)]
pub struct MockSupervisor {}

impl MockSupervisor {
    pub fn new() -> Self {
        Self::default()
    }
}

impl WorkerSupervisor for MockSupervisor {
    fn load_all(&self, models: &[ModelSpec]) -> Result<Vec<Loaded>, DaemonError> {
        Ok(models
            .iter()
            .map(|m| Loaded {
                model_id: m.id.clone(),
                quant: m.quant,
                input_shape: vec![1, 112, 112, 3],
                output_shape: vec![1, 192],
            })
            .collect())
    }

    fn infer(
        &mut self,
        _model_idx: usize,
        _frame_ref: &FrameRef,
        _frame: &[u8],
    ) -> Result<Vec<f32>, DaemonError> {
        Ok(vec![0.0; 192])
    }

    fn shutdown(&mut self) -> Result<(), DaemonError> {
        Ok(())
    }
}

/// Test-only supervisor that counts `shutdown` calls, so teardown wiring can
/// be asserted without a real child process. Shutdown must be *invoked* by
/// the teardown paths — a Drop that depends on whichever thread releases the
/// last `Arc` is exactly the non-determinism being fixed (issue #31).
#[cfg(test)]
#[derive(Default)]
pub struct ShutdownCountingSupervisor {
    pub shutdowns: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl ShutdownCountingSupervisor {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A spawned test child that is reaped through its own `Child` on drop.
///
/// The code under test may reap the child earlier through its own `waitpid`
/// (terminator, bounded teardown) — `wait()` then fails with `ECHILD`, which
/// is exactly the accepted outcome. The guard exists so no test child can
/// outlive the test as a zombie whichever path ran, and so every spawned
/// `Child` is waited on through its owning handle.
#[cfg(test)]
pub(crate) struct ReapOnDrop(std::process::Child);

#[cfg(test)]
impl ReapOnDrop {
    pub(crate) fn spawn(cmd: &mut std::process::Command) -> Self {
        ReapOnDrop(cmd.spawn().expect("spawn test child"))
    }

    pub(crate) fn pid(&self) -> u32 {
        self.0.id()
    }

    /// Explicit reap through the owning handle (ECHILD-tolerant by Drop).
    pub(crate) fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.wait()
    }
}

#[cfg(test)]
impl Drop for ReapOnDrop {
    fn drop(&mut self) {
        // Only kill while our child is still running. ECHILD means the
        // terminator already reaped it; never signal a recycled numeric PID.
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[cfg(test)]
impl WorkerSupervisor for ShutdownCountingSupervisor {
    fn load_all(&self, models: &[ModelSpec]) -> Result<Vec<Loaded>, DaemonError> {
        Ok(models
            .iter()
            .map(|m| Loaded {
                model_id: m.id.clone(),
                quant: m.quant,
                input_shape: vec![1, 112, 112, 3],
                output_shape: vec![1, 192],
            })
            .collect())
    }

    fn infer(
        &mut self,
        _model_idx: usize,
        _frame_ref: &FrameRef,
        _frame: &[u8],
    ) -> Result<Vec<f32>, DaemonError> {
        Ok(vec![0.0; 192])
    }

    fn shutdown(&mut self) -> Result<(), DaemonError> {
        use std::sync::atomic::Ordering;
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Real worker supervisor: spawns `namtor-worker` as a child, communicates
/// over a socketpair, and shares frame bytes via `memfd`.
pub struct ChildSupervisor {
    child: Child,
    daemon_reader: std::fs::File,
    daemon_writer: std::fs::File,
    frame_memfd: std::fs::File,
    frame_region_len: u64,
    loaded: Vec<Loaded>,
}

impl ChildSupervisor {
    /// Spawn the worker child. The child is this same executable re-exec'd as
    /// `namtor worker`, resolved through `/proc/self/exe`; `worker_bin` is the
    /// configured fallback used only when `/proc/self/exe` cannot be read.
    /// `mock_infer` selects the `mock-infer` feature (for CI tests).
    pub fn spawn(
        worker_bin: &Path,
        models: &[ModelSpec],
        mock_infer: bool,
        models_dir: &Path,
    ) -> Result<Self, DaemonError> {
        Self::spawn_with_exe(&self_exe(worker_bin), models, mock_infer, models_dir)
    }

    /// Spawn `<exe> worker ...` as the worker child. [`Self::spawn`] resolves
    /// `exe` for the running daemon; tests pass the built `namtor` binary
    /// directly, because their own `/proc/self/exe` is the test harness.
    ///
    /// Inheritance contract (issue #31):
    ///
    /// * Every descriptor this function creates is close-on-exec. Exactly the
    ///   worker's three IPC descriptors — request-read, reply-write and the
    ///   frame memfd — have the flag cleared in the forked child just before
    ///   exec, so the worker never holds a daemon-side endpoint (its own copy
    ///   of the request-write end would otherwise keep the request pipe open
    ///   forever and it could never observe EOF).
    /// * The child arms `PR_SET_PDEATHSIG(SIGTERM)` *before* exec — i.e.
    ///   before the worker applies its sandbox — and re-checks the parent's
    ///   pid afterwards, closing the fork-to-prctl race. A SIGKILLed daemon
    ///   therefore cannot orphan the worker, even while it is blocked in
    ///   `read(2)` or inference.
    /// * Socket-activation state (`LISTEN_FDS`/`LISTEN_PID`/`LISTEN_FDNAMES`)
    ///   is stripped from the child environment: the worker serves no sockets.
    pub fn spawn_with_exe(
        exe: &Path,
        models: &[ModelSpec],
        mock_infer: bool,
        models_dir: &Path,
    ) -> Result<Self, DaemonError> {
        use nix::sys::memfd::{memfd_create, MemFdCreateFlag};
        use nix::sys::socket::{socketpair, AddressFamily, SockFlag, SockType};
        use std::os::unix::process::CommandExt;

        // Create the frame memfd (64 MiB region).
        let memfd_name = std::ffi::CString::new("namtor-frames")
            .map_err(|e| DaemonError::Worker(format!("memfd name: {e}")))?;
        // CLOEXEC by default: no descriptor crosses exec unless the pre_exec
        // hook below clears the flag for one of the worker's three IPC
        // descriptors.
        let frame_memfd = memfd_create(&memfd_name, MemFdCreateFlag::MFD_CLOEXEC)
            .map_err(|e| DaemonError::Worker(format!("memfd_create: {e}")))?;
        // Size it to 64 MiB.
        nix::unistd::ftruncate(&frame_memfd, 64 * 1024 * 1024)
            .map_err(|e| DaemonError::Worker(format!("ftruncate: {e}")))?;
        let frame_memfd_raw = frame_memfd.as_raw_fd();

        // Socketpairs for daemon↔worker requests and replies, CLOEXEC for the
        // same reason as the memfd.
        let (req_w, req_r) = socketpair(
            AddressFamily::Unix,
            SockType::Stream,
            None,
            SockFlag::SOCK_CLOEXEC,
        )
        .map_err(|e| DaemonError::Worker(format!("socketpair: {e}")))?;
        let (rsp_w, rsp_r) = socketpair(
            AddressFamily::Unix,
            SockType::Stream,
            None,
            SockFlag::SOCK_CLOEXEC,
        )
        .map_err(|e| DaemonError::Worker(format!("socketpair: {e}")))?;

        let req_r_raw = req_r.as_raw_fd();
        let rsp_w_raw = rsp_w.as_raw_fd();

        // Spawn the worker, inheriting the fds via the pre_exec hook below.
        // `namtor worker` is this same executable re-exec'd with the `worker`
        // subcommand — a separate process, not a thread.
        let mut cmd = Command::new(exe);
        cmd.args(worker_args(
            req_r_raw,
            rsp_w_raw,
            frame_memfd_raw,
            64 * 1024 * 1024,
            models_dir,
            models,
        ));
        if mock_infer {
            cmd.env("NAMTOR_WORKER_FEATURES", "mock-infer");
        }
        // The worker serves no sockets: it must never see socket-activation
        // state, whatever the daemon's own environment contains. (A
        // `LISTEN_PID` naming the daemon could otherwise let an
        // `sd_listen_fds`-style consumer in the worker adopt the daemon's
        // listeners.)
        cmd.env_remove("LISTEN_FDS")
            .env_remove("LISTEN_PID")
            .env_remove("LISTEN_FDNAMES");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        // In the forked child, exactly the three IPC descriptors survive exec:
        // their CLOEXEC flag is cleared here, everything else keeps it. Then
        // the parent-death signal is armed *before* exec — before the worker
        // applies its sandbox — and the parent's identity is re-checked to
        // close the fork-to-prctl race.
        let ipc_fds = [req_r_raw, rsp_w_raw, frame_memfd_raw];
        // SAFETY: getpid is async-signal-safe and has no preconditions.
        let parent_pid = unsafe { libc::getpid() };
        // SAFETY: the closure runs in the forked child between fork and exec
        // and calls only async-signal-safe libc functions (fcntl, prctl,
        // getppid); it performs no allocation on the success path.
        unsafe {
            cmd.pre_exec(move || {
                // SAFETY: F_GETFD/F_SETFD on our own still-open descriptors.
                for fd in ipc_fds {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                // SAFETY: single async-signal-safe prctl call. SIGTERM's
                // default disposition ends the process even while it is
                // blocked in read(2); the sandbox cannot affect delivery
                // because this runs before the worker applies it.
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // SAFETY: async-signal-safe. If the parent died between the
                // fork and the prctl above, the signal will never arrive (it
                // covers only deaths after arming) and getppid() no longer
                // returns our parent — refuse to start rather than leak an
                // orphaned worker. ESRCH ("no such process") names the
                // condition without allocating in the fork child.
                if libc::getppid() != parent_pid {
                    return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }

        let child = cmd
            .spawn()
            .map_err(|e| DaemonError::Worker(format!("spawn {exe:?} worker: {e}")))?;

        // The worker now holds its own copies of the read end, the reply end
        // and the memfd. Drop the worker-side ends here and adopt the
        // daemon-side ends as owned files (`From<OwnedFd>` transfers
        // ownership; no double close, no forgotten handles).
        drop(req_r);
        drop(rsp_w);
        let daemon_writer = std::fs::File::from(req_w);
        let daemon_reader = std::fs::File::from(rsp_r);
        let frame_memfd_file = std::fs::File::from(frame_memfd);

        let mut supervisor = Self {
            child,
            daemon_reader,
            daemon_writer,
            frame_memfd: frame_memfd_file,
            frame_region_len: 64 * 1024 * 1024,
            loaded: Vec::new(),
        };

        // Load all models.
        for spec in models {
            supervisor.send_req(&WorkerReq::Load(spec.clone()))?;
            let rsp = supervisor.recv_rsp()?;
            match rsp {
                WorkerRsp::Loaded(l) => supervisor.loaded.push(l),
                WorkerRsp::Error { msg } => {
                    return Err(DaemonError::Worker(format!("load {spec:?}: {msg}")));
                }
                _ => return Err(DaemonError::Worker("unexpected rsp".into())),
            }
        }

        Ok(supervisor)
    }
}

/// Build the full argument vector for the worker child.
///
/// Extracted from [`ChildSupervisor::spawn_with_exe`] so the security-critical
/// parts of the command line are assertable without spawning a process — see
/// `spawn_always_asks_the_worker_to_sandbox_itself`. `spawn_with_exe` builds
/// its `Command` from *this* function and nothing else, so a flag that is
/// missing here is missing in production.
///
/// Two of these arguments are the sandbox contract:
///
/// * `--apply-sandbox` — the worker self-applies seccomp + Landlock. Without
///   it the process that runs ONNX inference on attacker-influenced image
///   bytes runs unconfined, which is the boundary `AGENTS.md` and the systemd
///   unit both claim exists.
/// * `--model <spec>` (one per model) — Landlock denies *every* path, so the
///   worker must open its weights before sealing itself. The daemon therefore
///   names the models up front instead of relying on the `Load` requests that
///   arrive after the seal.
fn worker_args(
    read_fd: RawFd,
    write_fd: RawFd,
    frame_memfd: RawFd,
    frame_len: u64,
    models_dir: &Path,
    models: &[ModelSpec],
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "worker".into(),
        "--read-fd".into(),
        read_fd.to_string().into(),
        "--write-fd".into(),
        write_fd.to_string().into(),
        "--frame-memfd".into(),
        frame_memfd.to_string().into(),
        "--frame-len".into(),
        frame_len.to_string().into(),
        "--models-dir".into(),
        models_dir.as_os_str().to_os_string(),
    ];
    for spec in models {
        args.push("--model".into());
        args.push(namtor_worker::model_arg::encode(spec).into());
    }
    args.push("--apply-sandbox".into());
    args
}

impl ChildSupervisor {
    /// Out-of-band termination handle for this supervisor's worker (see
    /// [`WorkerTerminator`]). Take it before the supervisor moves behind the
    /// dispatcher mutex, so teardown can terminate the worker even when a
    /// wedged verify holds that mutex forever.
    ///
    /// `None` only when `pidfd_open(2)` is unavailable (pre-5.3 kernel): the
    /// warning names it, and teardown fails closed if the mutex is ever
    /// wedged without a handle.
    pub(crate) fn terminator(&self) -> Option<WorkerTerminator> {
        match WorkerTerminator::from_pid(self.child.id()) {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "pidfd unavailable; out-of-band worker termination disabled"
                );
                None
            }
        }
    }

    fn send_req(&mut self, req: &WorkerReq) -> Result<(), DaemonError> {
        let mut writer = std::io::BufWriter::new(&self.daemon_writer);
        encode(req, &mut writer)?;
        writer.flush()?;
        Ok(())
    }

    fn recv_rsp(&mut self) -> Result<WorkerRsp, DaemonError> {
        let mut reader = std::io::BufReader::new(&self.daemon_reader);
        let v = decode(&mut reader)?;
        let rsp: WorkerRsp = serde_json::from_value(v)?;
        Ok(rsp)
    }

    fn write_frame(&mut self, frame: &[u8]) -> Result<FrameRef, DaemonError> {
        if frame.len() as u64 > self.frame_region_len {
            return Err(DaemonError::Worker(format!(
                "frame {} bytes exceeds region {}",
                frame.len(),
                self.frame_region_len
            )));
        }
        self.frame_memfd.seek(std::io::SeekFrom::Start(0))?;
        self.frame_memfd.write_all(frame)?;
        Ok(FrameRef {
            offset: 0,
            len: frame.len() as u32,
            emitter: namtor_capture::frame::EmitterState::On,
            spectrum: namtor_capture::source::Spectrum::Ir,
            pix_fmt: namtor_capture::frame::PixFmt::Grey8,
            w: 0,
            h: 0,
            ts_us: 0,
        })
    }
}

impl WorkerSupervisor for ChildSupervisor {
    fn load_all(&self, _models: &[ModelSpec]) -> Result<Vec<Loaded>, DaemonError> {
        // Models are loaded in spawn(); return the cached list.
        Ok(self.loaded.clone())
    }

    fn infer(
        &mut self,
        model_idx: usize,
        frame_ref: &FrameRef,
        frame: &[u8],
    ) -> Result<Vec<f32>, DaemonError> {
        let _ = model_idx;
        // Write frame bytes to memfd.
        let actual_ref = self.write_frame(frame)?;
        let _ = frame_ref; // Use actual_ref instead.
        self.send_req(&WorkerReq::Infer(actual_ref))?;
        let rsp = self.recv_rsp()?;
        match rsp {
            WorkerRsp::Vector { vec } => Ok(vec),
            WorkerRsp::Error { msg } => Err(DaemonError::Worker(msg)),
            _ => Err(DaemonError::Worker("unexpected rsp".into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), DaemonError> {
        // Ask nicely first — the send is non-blocking, so a worker that is
        // not reading can never delay the bounded reap below (LOW-1). A not
        // delivered request is fine: the reap terminates the worker anyway.
        if !send_shutdown_request(&self.daemon_writer) {
            tracing::debug!(
                pid = self.child.id(),
                "graceful Shutdown request not delivered; falling through to bounded SIGTERM"
            );
        }
        terminate_bounded(&mut self.child, SHUTDOWN_GRACE);
        Ok(())
    }
}

/// How long a worker gets to exit on its own after `Shutdown` before it is
/// SIGKILLed. Bounded so a wedged worker can never hang daemon teardown —
/// `Drop` runs wherever the last supervisor is dropped, including during
/// process exit where a blocking `wait()` would deadlock the shutdown.
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Reap `child` within `grace`; SIGKILL and reap it if it misses the window.
///
/// The `Shutdown` request is not honoured by every possible worker state
/// (an inference blocked in a wedged DQBUF never returns to the request
/// loop), so the wait must be bounded and must always end with a reaped
/// child — no zombie, no orphan.
fn terminate_bounded(child: &mut Child, grace: Duration) {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            // Reaped. `Err` is treated the same way: a `waitpid` failure
            // (e.g. `ECHILD`) means there is nothing left to wait for.
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    tracing::warn!(
        pid = child.id(),
        "worker missed the shutdown grace window; sending SIGKILL"
    );
    let _ = child.kill();
    let _ = child.wait();
}

/// Whether `pid` refers to a live (non-zombie) process. A zombie has already
/// exited — only its exit status is unreaped.
pub(crate) fn process_alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => {
            let after_comm = stat.rsplit_once(')').map(|(_, rest)| rest).unwrap_or("");
            after_comm.split_whitespace().next() != Some("Z")
        }
        Err(_) => false,
    }
}

/// Deliver the graceful `Shutdown` request without ever blocking.
///
/// The request pipe is a socketpair the worker reads; if the worker is wedged
/// and the buffer is full, a plain `write_all` would block *before* the
/// bounded reap is ever reached. `send(2)` with `MSG_DONTWAIT` makes the
/// delivery attempt non-blocking; anything short of a complete framed write
/// is reported as "not delivered" and the caller falls through to the bounded
/// SIGTERM, so a truncated frame is never read by a worker that keeps
/// running (the protocol stays correct: either the whole request arrives or
/// the worker is terminated).
fn send_shutdown_request(writer: &std::fs::File) -> bool {
    let mut buf = Vec::new();
    if encode(&WorkerReq::Shutdown, &mut buf).is_err() {
        return false;
    }
    // SAFETY: send(2) on our own open socket descriptor; MSG_DONTWAIT makes
    // this call non-blocking. Only a complete write counts as delivered.
    let sent = unsafe {
        libc::send(
            writer.as_raw_fd(),
            buf.as_ptr().cast(),
            buf.len(),
            libc::MSG_DONTWAIT,
        )
    };
    sent == buf.len() as isize
}

/// Out-of-band worker termination handle, built on a pidfd.
///
/// The supervisor lives behind the dispatcher mutex, which a wedged verify
/// can hold for an unbounded time; this handle, taken from the supervisor at
/// spawn time (before it moves behind any lock), lets teardown terminate the
/// worker without that lock.
///
/// The handle holds a **pidfd** opened immediately after spawn
/// (`pidfd_open(2)`): the kernel pins the descriptor to that exact task, so
/// `pidfd_send_signal(2)` can never be retargeted by pid reuse, and it
/// reports `ESRCH` once the task is fully gone. Even after the supervisor is
/// dropped and the worker is reaped, a stale handle can only observe
/// [`Terminated::AlreadyGone`] — it can never signal an unrelated process
/// that recycled the pid.
#[derive(Debug)]
pub(crate) struct WorkerTerminator {
    pidfd: std::os::fd::OwnedFd,
    /// Only for logs; the pidfd, not the pid, selects the signal target.
    pid: u32,
}

/// Outcome of an out-of-band termination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Terminated {
    /// SIGTERM (and, if it survived the grace window, SIGKILL) was delivered
    /// to the worker and it was reaped.
    Signaled,
    /// The worker was already gone when teardown ran; nothing was signaled.
    AlreadyGone,
}

impl WorkerTerminator {
    /// Open a pidfd for `pid`. Call immediately after spawn, before anything
    /// can reap the child: the parent has not waited, so the pid cannot yet
    /// have been reused.
    ///
    /// # Errors
    /// `pidfd_open(2)` failure (e.g. `ENOSYS` on a pre-5.3 kernel): the
    /// caller proceeds without an out-of-band handle, and teardown fails
    /// closed if the mutex is ever wedged.
    pub(crate) fn from_pid(pid: u32) -> Result<Self, DaemonError> {
        // SAFETY: pidfd_open(2) on a child of this process that has not been
        // waited on; flags must be 0.
        let fd =
            unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_uint, 0 as libc::c_uint) };
        if fd < 0 {
            return Err(DaemonError::Worker(format!(
                "pidfd_open({pid}): {}",
                std::io::Error::last_os_error()
            )));
        }
        // SAFETY: `fd` is a fresh, owned pidfd returned by the syscall above.
        let pidfd =
            unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd as RawFd) };
        Ok(Self { pidfd, pid })
    }

    /// Terminate the worker within `grace`: SIGTERM first (the worker
    /// installs no handler, so its default disposition ends it even while
    /// blocked in `read(2)`), then SIGKILL if it survives the window. The
    /// child is reaped by `waitpid` — no zombie remains.
    ///
    /// A process that has already exited (zombie or fully reaped) is never
    /// signaled: the zombie is reaped directly, and a fully gone task makes
    /// `pidfd_send_signal` return `ESRCH`, which maps to
    /// [`Terminated::AlreadyGone`].
    ///
    /// # Errors
    /// When the worker survives SIGTERM *and* SIGKILL (e.g. stuck in an
    /// uninterruptible kernel state) — the caller must not report success,
    /// because a worker that cannot be killed is still running.
    pub(crate) fn terminate(&self, grace: Duration) -> Result<Terminated, DaemonError> {
        // A zombie has exited; reaping it directly avoids delivering a
        // (harmless but misleading) signal to a dead task.
        if !process_alive(self.pid) {
            self.reap()?;
            return Ok(Terminated::AlreadyGone);
        }
        tracing::warn!(
            pid = self.pid,
            "terminating the worker out-of-band (SIGTERM)"
        );
        match self.send_signal(libc::SIGTERM) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::ESRCH) => {
                // Fully gone between the liveness check and the signal —
                // never signal anything else.
                self.reap()?;
                return Ok(Terminated::AlreadyGone);
            }
            Err(e) => {
                return Err(DaemonError::Worker(format!(
                    "pidfd_send_signal(SIGTERM) on worker {}: {e}",
                    self.pid
                )));
            }
        }
        let deadline = Instant::now() + grace;
        loop {
            if self.try_reap() {
                return Ok(Terminated::Signaled);
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        tracing::warn!(pid = self.pid, "worker survived SIGTERM; sending SIGKILL");
        match self.send_signal(libc::SIGKILL) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::ESRCH) => {
                self.reap()?;
                return Ok(Terminated::AlreadyGone);
            }
            Err(e) => {
                return Err(DaemonError::Worker(format!(
                    "pidfd_send_signal(SIGKILL) on worker {}: {e}",
                    self.pid
                )));
            }
        }
        let kill_deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < kill_deadline {
            if self.try_reap() {
                return Ok(Terminated::Signaled);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Err(DaemonError::Worker(format!(
            "worker {} survived SIGTERM and SIGKILL (uninterruptible state?); \
             it may still be running",
            self.pid
        )))
    }

    /// Deliver a signal through the pidfd. The kernel pins the descriptor to
    /// the spawned task, so pid reuse can never retarget it.
    fn send_signal(&self, sig: libc::c_int) -> Result<(), std::io::Error> {
        // SAFETY: pidfd_send_signal(2) on our own pidfd; no siginfo, no
        // flags.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                sig,
                std::ptr::null::<libc::c_void>(),
                0 as libc::c_uint,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Non-blocking reap attempt; true when the child was reaped by this
    /// call.
    fn try_reap(&self) -> bool {
        let mut status = 0;
        // SAFETY: waitpid(WNOHANG) on our own child. A reused pid is never
        // our child, so the worst case is `ECHILD`.
        let rc = unsafe { libc::waitpid(self.pid as libc::pid_t, &mut status, libc::WNOHANG) };
        rc == self.pid as libc::pid_t
    }

    /// Reap an exited child. `Ok` when reaped here or already reaped
    /// elsewhere (`ECHILD` — it exited either way).
    fn reap(&self) -> Result<(), DaemonError> {
        let mut status = 0;
        // SAFETY: waitpid(WNOHANG) on our own child, as in `try_reap`.
        let rc = unsafe { libc::waitpid(self.pid as libc::pid_t, &mut status, libc::WNOHANG) };
        if rc == self.pid as libc::pid_t {
            return Ok(());
        }
        if rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
            return Ok(());
        }
        Err(DaemonError::Worker(format!(
            "worker {} could not be reaped",
            self.pid
        )))
    }
}

/// Resolve the executable to re-exec as the worker child.
///
/// `/proc/self/exe` is preferred so the daemon re-execs *itself* whatever the
/// installed binary is called or wherever it was moved to. When `/proc` is not
/// mounted the configured `fallback` (`NAMTOR_WORKER_BIN`, else the install
/// manifest's binary path) is used instead.
fn self_exe(fallback: &Path) -> std::path::PathBuf {
    match std::fs::read_link("/proc/self/exe") {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                error = %e,
                fallback = %fallback.display(),
                "/proc/self/exe unreadable; using configured worker binary"
            );
            fallback.to_path_buf()
        }
    }
}

impl Drop for ChildSupervisor {
    fn drop(&mut self) {
        // Bounded: a hung worker cannot block the drop (see
        // `terminate_bounded` and `SHUTDOWN_GRACE`).
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_proto::worker::Quant;
    use std::os::unix::process::ExitStatusExt;
    use std::time::{Duration, Instant};

    fn specs() -> Vec<ModelSpec> {
        vec![
            ModelSpec {
                id: "yunet_2023mar".into(),
                quant: Quant::Fp32,
                sha256: [0x11; 32],
            },
            ModelSpec {
                id: "sface_2021dec".into(),
                quant: Quant::Int8,
                sha256: [0x22; 32],
            },
        ]
    }

    fn args_for(models: &[ModelSpec]) -> Vec<String> {
        worker_args(3, 4, 5, 64 * 1024 * 1024, Path::new("/models"), models)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// The gap this test exists to keep closed: `--apply-sandbox` was absent
    /// from the spawn command line for the whole life of the worker, so the
    /// seccomp + Landlock boundary the docs and the systemd unit describe was
    /// never actually applied in production. Nothing asserted the flag, so
    /// nothing noticed.
    #[test]
    fn spawn_always_asks_the_worker_to_sandbox_itself() {
        assert!(
            args_for(&specs()).iter().any(|a| a == "--apply-sandbox"),
            "the worker must be spawned with --apply-sandbox; without it the \
             ONNX inference process runs unconfined"
        );
    }

    /// There must be no way to spawn an unsandboxed worker: the flag does not
    /// depend on the model set, so an empty or unusual configuration cannot
    /// drop it.
    #[test]
    fn sandbox_flag_is_not_conditional_on_the_model_set() {
        assert!(args_for(&[]).iter().any(|a| a == "--apply-sandbox"));
    }

    /// Under the sandbox the worker cannot open a model file, so every model
    /// the daemon will later `Load` has to be named at spawn time — in the
    /// exact encoding the worker parses.
    #[test]
    fn spawn_names_every_model_for_preloading() {
        let models = specs();
        let args = args_for(&models);
        for spec in &models {
            let encoded = namtor_worker::model_arg::encode(spec);
            let at = args
                .iter()
                .position(|a| *a == encoded)
                .unwrap_or_else(|| panic!("{encoded} not on the worker command line: {args:?}"));
            assert_eq!(args[at - 1], "--model");
            assert_eq!(
                namtor_worker::model_arg::parse(&encoded).expect("worker parses it"),
                *spec
            );
        }
    }

    /// The fds and the model root still have to reach the child; asserting the
    /// sandbox flag must not come at the cost of the rest of the command line.
    #[test]
    fn worker_args_carry_the_channel_and_model_root() {
        let args = args_for(&specs());
        assert_eq!(args.first().map(String::as_str), Some("worker"));
        for (flag, value) in [
            ("--read-fd", "3"),
            ("--write-fd", "4"),
            ("--frame-memfd", "5"),
            ("--frame-len", "67108864"),
            ("--models-dir", "/models"),
        ] {
            let at = args
                .iter()
                .position(|a| a == flag)
                .unwrap_or_else(|| panic!("{flag} missing from {args:?}"));
            assert_eq!(args[at + 1], value, "{flag}");
        }
    }

    #[test]
    fn mock_supervisor_load_all() {
        let sup = MockSupervisor::new();
        let models = vec![ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        }];
        let loaded = sup.load_all(&models).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].model_id, "m");
    }

    /// A child that exits on its own is reaped by the bounded teardown without
    /// a SIGKILL and without the grace window elapsing.
    #[test]
    fn bounded_teardown_reaps_an_exiting_child() {
        let mut child = Command::new("sleep")
            .arg("0")
            .spawn()
            .expect("spawn sleep 0");
        terminate_bounded(&mut child, Duration::from_secs(2));
        let status = child.try_wait().expect("try_wait after reap");
        assert!(status.is_some(), "child was not reaped");
    }

    /// A child that ignores the graceful request must not hang teardown: the
    /// bounded teardown returns within the grace window plus scheduling slack
    /// and leaves the child reaped (SIGKILLed), so no zombie and no orphan.
    #[test]
    fn bounded_teardown_kills_a_hung_child_within_the_grace_window() {
        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep 30");
        let start = Instant::now();
        terminate_bounded(&mut child, Duration::from_millis(300));
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "teardown took {elapsed:?}; a hung worker must not block it unboundedly"
        );
        let status = child
            .try_wait()
            .expect("try_wait after reap")
            .expect("reaped");
        assert_eq!(
            status.code(),
            None,
            "the hung child must have been killed, not exited cleanly"
        );
        assert_eq!(status.signal(), Some(9), "expected SIGKILL, got {status}");
    }

    /// The graceful `Shutdown` request must never block: on a full request
    /// pipe (worker not reading) the bounded reap must remain the only wait,
    /// so the send is non-blocking and reports "not delivered" instead.
    #[test]
    fn graceful_shutdown_request_on_a_full_pipe_does_not_block() {
        use nix::sys::socket::{socketpair, AddressFamily, SockFlag, SockType};

        let (req_w, req_r) = socketpair(
            AddressFamily::Unix,
            SockType::Stream,
            None,
            SockFlag::empty(),
        )
        .expect("socketpair");
        // Keep the read end open but unread: the pipe must stay full.
        let _read_end = req_r;
        let writer = std::fs::File::from(req_w);
        // SAFETY: fcntl F_SETFL on our own socket descriptor.
        unsafe {
            let flags = libc::fcntl(writer.as_raw_fd(), libc::F_GETFL);
            assert!(flags >= 0);
            assert_eq!(
                libc::fcntl(writer.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK),
                0
            );
        }
        // Fill the socket buffer until the kernel says EAGAIN.
        let chunk = vec![0u8; 65536];
        let mut written = 0usize;
        loop {
            match nix::unistd::write(&writer, &chunk) {
                Ok(n) => written += n,
                Err(nix::errno::Errno::EAGAIN) => break,
                Err(e) => panic!("unexpected write error: {e}"),
            }
        }
        assert!(written > 0, "socket buffer was filled");

        let start = Instant::now();
        assert!(
            !send_shutdown_request(&writer),
            "a full request pipe must report the graceful request as not delivered"
        );
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "the graceful send must not block on a full pipe"
        );
    }

    /// The out-of-band terminator kills and reaps a hung child within the
    /// grace window — this is the path teardown takes when the dispatcher
    /// mutex is wedged and the supervisor is unreachable.
    #[test]
    fn terminator_reaps_a_hung_child_boundedly() {
        let child = ReapOnDrop::spawn(Command::new("sleep").arg("30"));
        let pid = child.pid();
        let terminator = WorkerTerminator::from_pid(pid).expect("pidfd");
        let start = Instant::now();
        let outcome = terminator
            .terminate(Duration::from_millis(300))
            .expect("terminate hung child");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "out-of-band termination took {:?}",
            start.elapsed()
        );
        assert!(
            matches!(outcome, Terminated::Signaled),
            "a live child must be signaled and reaped, got {outcome:?}"
        );
        // Reaped, not left as a zombie: /proc/<pid> must be gone.
        assert!(
            std::fs::metadata(format!("/proc/{pid}")).is_err(),
            "child must be reaped, not left unreaped"
        );
    }

    /// A child that already exited (zombie: exited but unreaped) is reaped
    /// cleanly by the terminator — no signal is delivered to anything.
    #[test]
    fn terminator_reaps_an_already_exited_child_without_signaling() {
        let child = ReapOnDrop::spawn(Command::new("sleep").arg("0"));
        let pid = child.pid();
        while process_alive(pid) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let outcome = WorkerTerminator::from_pid(pid)
            .expect("pidfd of a zombie")
            .terminate(Duration::from_millis(300))
            .expect("already-exited child reaps cleanly");
        assert!(
            matches!(outcome, Terminated::AlreadyGone),
            "a zombie must be reaped, not signaled: {outcome:?}"
        );
        assert!(
            std::fs::metadata(format!("/proc/{pid}")).is_err(),
            "zombie must be reaped"
        );
    }

    /// After the worker is fully reaped (supervisor dropped it), the
    /// terminator must signal nothing at all: the pidfd resolves to a dead
    /// task (ESRCH), so even a reused pid can never be hit by a stray
    /// signal — the handle cannot kill an unrelated process.
    #[test]
    fn terminator_signals_nothing_after_the_child_is_fully_reaped() {
        let mut child = ReapOnDrop::spawn(Command::new("sleep").arg("0"));
        let pid = child.pid();
        let terminator = WorkerTerminator::from_pid(pid).expect("pidfd");
        // Full reap by the supervisor side (as Drop would do).
        child.wait().expect("reap child");
        assert!(std::fs::metadata(format!("/proc/{pid}")).is_err());

        let outcome = terminator
            .terminate(Duration::from_millis(300))
            .expect("terminate after full reap must be a clean no-op");
        assert!(
            matches!(outcome, Terminated::AlreadyGone),
            "nothing may be signaled after a full reap: {outcome:?}"
        );
        // Repeated calls stay clean: the handle stays safe after supervisor
        // drop for the rest of its life.
        let again = terminator
            .terminate(Duration::from_millis(300))
            .expect("repeat terminate stays clean");
        assert!(matches!(again, Terminated::AlreadyGone));
    }
}
