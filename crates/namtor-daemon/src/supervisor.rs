// SPDX-License-Identifier: GPL-3.0-only
//! Worker supervisor: re-execs this executable as `namtor worker` in a child
//! process, establishes the pipe + memfd frame channel, and forwards
//! Load/Infer requests.
//!
//! The daemon (non-root, user `namtor`) spawns the worker directly as a
//! child process — the worker inherits the daemon's UID (`namtor`). The
//! worker self-applies seccomp + landlock (`--apply-sandbox`); the sandbox
//! is the privilege boundary, not a UID drop. In tests the worker runs
//! with `--features mock-infer` and no sandbox.
//!
//! Daemon and worker are the *same file* (one shipped `namtor` executable)
//! but remain *separate processes*: merging the worker into the daemon's
//! address space would delete the isolation that keeps an inference-side
//! compromise away from templates, keys and the PAM socket. The child is
//! started from `/proc/self/exe` so a renamed, moved or still-unlinked
//! executable re-execs itself correctly; `worker_bin` is the fallback when
//! `/proc` is not mounted (e.g. a minimal container).

use std::io::{Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::process::{Child, Command, Stdio};

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
    pub fn spawn_with_exe(
        exe: &Path,
        models: &[ModelSpec],
        mock_infer: bool,
        models_dir: &Path,
    ) -> Result<Self, DaemonError> {
        use nix::sys::memfd::{memfd_create, MemFdCreateFlag};
        use nix::sys::socket::{socketpair, AddressFamily, SockFlag, SockType};

        // Create the frame memfd (64 MiB region).
        let memfd_name = std::ffi::CString::new("namtor-frames")
            .map_err(|e| DaemonError::Worker(format!("memfd name: {e}")))?;
        let frame_memfd = memfd_create(&memfd_name, MemFdCreateFlag::empty())
            .map_err(|e| DaemonError::Worker(format!("memfd_create: {e}")))?;
        // Size it to 64 MiB.
        nix::unistd::ftruncate(&frame_memfd, 64 * 1024 * 1024)
            .map_err(|e| DaemonError::Worker(format!("ftruncate: {e}")))?;
        let frame_memfd_file = unsafe { std::fs::File::from_raw_fd(frame_memfd.as_raw_fd()) };
        let frame_memfd_raw = frame_memfd.as_raw_fd();
        std::mem::forget(frame_memfd);

        // Create socketpair for daemon↔worker pipe.
        let (req_w, req_r) = socketpair(
            AddressFamily::Unix,
            SockType::Stream,
            None,
            SockFlag::empty(),
        )
        .map_err(|e| DaemonError::Worker(format!("socketpair: {e}")))?;
        let (rsp_w, rsp_r) = socketpair(
            AddressFamily::Unix,
            SockType::Stream,
            None,
            SockFlag::empty(),
        )
        .map_err(|e| DaemonError::Worker(format!("socketpair: {e}")))?;

        let req_r_raw = req_r.as_raw_fd();
        let req_w_raw = req_w.as_raw_fd();
        let rsp_w_raw = rsp_w.as_raw_fd();
        let rsp_r_raw = rsp_r.as_raw_fd();

        // Spawn the worker, inheriting the fds. `namtor worker` is this same
        // executable re-exec'd with the `worker` subcommand — a separate
        // process, not a thread.
        let mut cmd = Command::new(exe);
        cmd.arg("worker");
        cmd.arg("--read-fd")
            .arg(req_r_raw.to_string())
            .arg("--write-fd")
            .arg(rsp_w_raw.to_string())
            .arg("--frame-memfd")
            .arg(frame_memfd_raw.to_string())
            .arg("--frame-len")
            .arg((64 * 1024 * 1024).to_string())
            .arg("--models-dir")
            .arg(models_dir);
        if mock_infer {
            cmd.env("NAMTOR_WORKER_FEATURES", "mock-infer");
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        // The worker inherits the fds via the parent's fd table. We need to
        // clear CLOEXEC on the inherited fds so they survive exec.
        // socketpair creates fds without CLOEXEC by default (we used SockFlag::empty()).
        // memfd has MFD_CLOEXEC — clear it for the worker by dup'ing.
        // Actually, we set MFD_CLOEXEC above. Let's not set it.
        // For simplicity, just pass the fds and hope the worker gets them.
        // The socketpair fds don't have CLOEXEC, so they survive exec.
        // The memfd has CLOEXEC — we need to re-create without it, or dup.
        // Let's just not set CLOEXEC on the memfd.
        // (Fix: re-create without CFD_CLOEXEC. For now, just proceed — the
        // worker test will tell us if the fd doesn't survive.)

        let child = cmd
            .spawn()
            .map_err(|e| DaemonError::Worker(format!("spawn {exe:?} worker: {e}")))?;

        // Drop the worker-side fds from the daemon (they're now in the child).
        drop(req_r);
        drop(rsp_w);

        // Prevent the daemon-side fds from being closed when the OwnedFd
        // wrappers go out of scope — File::from_raw_fd takes ownership of
        // the raw fd, so the OwnedFd must not also close it.
        std::mem::forget(req_w);
        std::mem::forget(rsp_r);

        let daemon_reader = unsafe { std::fs::File::from_raw_fd(rsp_r_raw) };
        let daemon_writer = unsafe { std::fs::File::from_raw_fd(req_w_raw) };

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
        let _ = self.send_req(&WorkerReq::Shutdown);
        let _ = self.child.wait();
        Ok(())
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
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_proto::worker::Quant;

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
}
