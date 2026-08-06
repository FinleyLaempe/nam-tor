// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-worker`: the sandboxed inference host.
//!
//! The worker is a child of [`namtor-daemon`] (Task 21 supervisor). It inherits
//! a pair of pipe file descriptors — `--read-fd` (daemon→worker requests) and
//! `--write-fd` (worker→daemon replies) — and an optional `memfd` region
//! carrying frame pixel bytes. It mmaps the frame region read-only, optionally
//! self-applies a seccomp + landlock sandbox, and runs the request loop in
//! [`run_main`] (Task 9): `Load` → `Loaded`, `Infer(FrameRef)` → `Vector`,
//! `Shutdown` → exit.
//!
//! The wire types live in [`namtor_proto::worker`]; framing in
//! [`namtor_proto::framing`]. This crate is deliberately small: transport,
//! the [`infer::InferenceBackend`] trait, the sandbox, and the loop.
//!
//! [`namtor-daemon`]: https://github.com/finley/nam-tor

pub mod backend_eval;
pub mod dispatch;
pub mod infer;
#[cfg(feature = "onnx")]
pub mod infer_onnx;
pub mod learned_pad;
pub mod pipe;
pub mod sandbox;

use std::os::fd::RawFd;

use namtor_proto::worker::{WorkerReq, WorkerRsp};
use thiserror::Error;

/// Worker-side error.
///
/// `Eof` is the clean end-of-stream signal returned by [`pipe::PipeTransport::recv`]
/// when the daemon closed its write end (i.e. there are no more requests). It is
/// distinct from [`WorkerError::Pipe`] so the loop can tell "shutdown requested
/// by close" apart from "frame truncated / io broke".
#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("pipe framing error: {0}")]
    Pipe(#[from] namtor_proto::framing::ProtoError),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// Clean EOF on the read pipe — the daemon closed its write end.
    #[error("read pipe closed (EOF)")]
    Eof,
    #[error("bad file descriptor: {0}")]
    BadFd(String),
    #[error("sandbox error: {0}")]
    Sandbox(String),
    #[error("inference error: {0}")]
    Infer(String),
}

/// Frame-byte channel: a memfd-backed shared region the daemon writes frame
/// pixels into and the worker mmaps read-only before applying its sandbox.
pub struct FrameChannel {
    ptr: *const u8,
    len: usize,
}

impl FrameChannel {
    /// Wrap a raw fd as a read-only memfd region of `len` bytes.
    pub fn from_memfd(fd: RawFd, len: usize) -> Result<Self, WorkerError> {
        use std::num::NonZeroUsize;
        use std::os::fd::FromRawFd;
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let nz_len = NonZeroUsize::new(len)
            .ok_or_else(|| WorkerError::BadFd("zero-length frame region".into()))?;
        let mmap = unsafe {
            nix::sys::mman::mmap(
                None,
                nz_len,
                nix::sys::mman::ProtFlags::PROT_READ,
                nix::sys::mman::MapFlags::MAP_SHARED,
                &file,
                0,
            )
            .map_err(|e| WorkerError::BadFd(format!("mmap: {e}")))?
        };
        drop(file);
        Ok(Self {
            ptr: mmap.as_ptr() as *const u8,
            len,
        })
    }

    /// Borrow the frame bytes at `[offset, offset+len)`.
    pub fn slice(&self, offset: u64, len: u32) -> Result<&[u8], WorkerError> {
        let start = offset as usize;
        let end = start + len as usize;
        if end > self.len {
            return Err(WorkerError::Infer(format!(
                "frame slice [{start},{end}) out of bounds (region {len})",
                len = self.len
            )));
        }
        // SAFETY: mmap is valid for the lifetime of FrameChannel; we only
        // borrow within bounds.
        Ok(unsafe { std::slice::from_raw_parts(self.ptr.add(start), len as usize) })
    }
}

unsafe impl Send for FrameChannel {}
unsafe impl Sync for FrameChannel {}

/// Run the worker request loop.
///
/// `read_fd` / `write_fd` are the inherited pipe ends. `backend` is the
/// inference backend (MockBackend for tests, OnnxCpu for production). If
/// `apply_sbx` is true, landlock+seccomp are applied after the frame channel
/// is mapped. `frame_memfd` / `frame_region_len` describe the shared frame
/// region; pass `(0, 0)` and `None` if no frame channel is needed (unit tests
/// that don't exercise Infer).
pub fn run_main(
    read_fd: RawFd,
    write_fd: RawFd,
    mut backend: Box<dyn infer::InferenceBackend>,
    apply_sbx: bool,
    frame_memfd: Option<RawFd>,
    frame_region_len: u64,
) -> Result<(), WorkerError> {
    let mut transport = pipe::PipeTransport::from_fds(read_fd, write_fd)?;

    let frame_channel = match frame_memfd {
        Some(fd) if frame_region_len > 0 => {
            Some(FrameChannel::from_memfd(fd, frame_region_len as usize)?)
        }
        _ => None,
    };

    if apply_sbx {
        sandbox::apply_sandbox()?;
    }

    let mut loaded: Option<namtor_proto::worker::Loaded> = None;

    loop {
        let req = transport.recv();
        match req {
            Ok(WorkerReq::Load(spec)) => match backend.load(&spec) {
                Ok(l) => {
                    let rsp = WorkerRsp::Loaded(l.clone());
                    loaded = Some(l);
                    transport.send(&rsp)?;
                }
                Err(e) => {
                    let rsp = WorkerRsp::Error { msg: e.to_string() };
                    transport.send(&rsp)?;
                }
            },
            Ok(WorkerReq::Infer(frame_ref)) => {
                let Some(ref lc) = loaded else {
                    transport.send(&WorkerRsp::Error {
                        msg: "no model loaded".into(),
                    })?;
                    continue;
                };
                let frame_bytes = match &frame_channel {
                    Some(fc) => fc.slice(frame_ref.offset, frame_ref.len)?,
                    None => &[][..],
                };
                match backend.run(lc, frame_bytes, &frame_ref) {
                    Ok(vec) => {
                        transport.send(&WorkerRsp::Vector { vec })?;
                    }
                    Err(e) => {
                        transport.send(&WorkerRsp::Error { msg: e.to_string() })?;
                    }
                }
            }
            Ok(WorkerReq::Shutdown) => break,
            Err(WorkerError::Eof) => break,
            Err(e) => {
                let rsp = WorkerRsp::Error { msg: e.to_string() };
                let _ = transport.send(&rsp);
                return Err(e);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::run_main;
    use crate::infer::MockBackend;
    use namtor_capture::frame::{EmitterState, PixFmt};
    use namtor_capture::source::Spectrum;
    use namtor_proto::worker::{FrameRef, Loaded, ModelSpec, Quant, WorkerReq, WorkerRsp};
    use nix::sys::memfd::MemFdCreateFlag;
    use std::io::Write;
    use std::os::fd::{AsFd, AsRawFd, FromRawFd};

    struct DaemonEnd {
        r: std::io::BufReader<std::fs::File>,
        w: std::io::BufWriter<std::fs::File>,
    }

    impl DaemonEnd {
        fn send(&mut self, req: &WorkerReq) {
            namtor_proto::framing::encode(req, &mut self.w).unwrap();
            self.w.flush().unwrap();
        }
        fn recv(&mut self) -> WorkerRsp {
            let v = namtor_proto::framing::decode(&mut self.r).unwrap();
            serde_json::from_value(v).unwrap()
        }
    }

    fn pair() -> (i32, i32, DaemonEnd) {
        use nix::sys::socket::{socketpair, AddressFamily, SockFlag, SockType};
        let sp = || {
            socketpair(
                AddressFamily::Unix,
                SockType::Stream,
                None,
                SockFlag::empty(),
            )
            .expect("socketpair")
        };
        let (req_w, req_r) = sp();
        let (rsp_w, rsp_r) = sp();
        let req_w_raw = req_w.as_fd().as_raw_fd();
        let req_r_raw = req_r.as_fd().as_raw_fd();
        let rsp_w_raw = rsp_w.as_fd().as_raw_fd();
        let rsp_r_raw = rsp_r.as_fd().as_raw_fd();
        std::mem::forget((req_w, req_r, rsp_w, rsp_r));
        let daemon = DaemonEnd {
            r: std::io::BufReader::new(unsafe { std::fs::File::from_raw_fd(rsp_r_raw) }),
            w: std::io::BufWriter::new(unsafe { std::fs::File::from_raw_fd(req_w_raw) }),
        };
        (req_r_raw, rsp_w_raw, daemon)
    }

    fn frame_ref(len: u32) -> FrameRef {
        FrameRef {
            offset: 0,
            len,
            emitter: EmitterState::On,
            spectrum: Spectrum::Ir,
            pix_fmt: PixFmt::Grey8,
            w: len,
            h: 1,
            ts_us: 0,
        }
    }

    fn spec() -> ModelSpec {
        ModelSpec {
            id: "test".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        }
    }

    #[test]
    fn run_main_load_infer_shutdown() {
        let (read_fd, write_fd, mut daemon) = pair();

        let backend = Box::new(MockBackend::new(8));
        let handle =
            std::thread::spawn(move || run_main(read_fd, write_fd, backend, false, None, 0));

        daemon.send(&WorkerReq::Load(spec()));
        let rsp = daemon.recv();
        match rsp {
            WorkerRsp::Loaded(Loaded { model_id, .. }) => assert_eq!(model_id, "test"),
            other => panic!("expected Loaded, got {other:?}"),
        }

        daemon.send(&WorkerReq::Infer(frame_ref(5)));
        let rsp = daemon.recv();
        match rsp {
            WorkerRsp::Vector { vec } => assert_eq!(vec.len(), 8),
            other => panic!("expected Vector, got {other:?}"),
        }

        daemon.send(&WorkerReq::Shutdown);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn run_main_infer_without_load_returns_error() {
        let (read_fd, write_fd, mut daemon) = pair();

        let backend = Box::new(MockBackend::new(8));
        let handle =
            std::thread::spawn(move || run_main(read_fd, write_fd, backend, false, None, 0));

        daemon.send(&WorkerReq::Infer(frame_ref(3)));
        let rsp = daemon.recv();
        match rsp {
            WorkerRsp::Error { msg } => assert!(msg.contains("no model loaded")),
            other => panic!("expected Error, got {other:?}"),
        }

        daemon.send(&WorkerReq::Shutdown);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn run_main_eof_exits_cleanly() {
        let (read_fd, write_fd, daemon) = pair();

        let backend = Box::new(MockBackend::new(8));
        let handle =
            std::thread::spawn(move || run_main(read_fd, write_fd, backend, false, None, 0));

        drop(daemon);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn run_main_with_memfd_frame_channel() {
        let (read_fd, write_fd, mut daemon) = pair();

        let memfd_name = std::ffi::CString::new("test-frames").unwrap();
        let memfd = nix::sys::memfd::memfd_create(&memfd_name, MemFdCreateFlag::empty())
            .expect("memfd_create");
        let frame_data = vec![0x42u8; 64];
        nix::unistd::write(&memfd, &frame_data).unwrap();
        let memfd_raw = memfd.as_raw_fd();
        std::mem::forget(memfd);

        let backend = Box::new(MockBackend::new(4));
        let handle = std::thread::spawn(move || {
            run_main(read_fd, write_fd, backend, false, Some(memfd_raw), 64)
        });

        daemon.send(&WorkerReq::Load(spec()));
        let _ = daemon.recv();

        daemon.send(&WorkerReq::Infer(frame_ref(64)));
        let rsp = daemon.recv();
        match rsp {
            WorkerRsp::Vector { vec } => assert_eq!(vec.len(), 4),
            other => panic!("expected Vector, got {other:?}"),
        }

        daemon.send(&WorkerReq::Shutdown);
        handle.join().unwrap().unwrap();
    }
}
