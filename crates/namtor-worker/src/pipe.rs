// SPDX-License-Identifier: GPL-3.0-only
//! Pipe transport over inherited file descriptors.
//!
//! The daemon spawns the worker with two inherited fds: a read end for
//! [`namtor_proto::worker::WorkerReq`] (daemon→worker) and a write end for
//! [`namtor_proto::worker::WorkerRsp`] (worker→daemon). Both ends speak the
//! length-delimited JSON framing from [`namtor_proto::framing`].
//!
//! [`PipeTransport::from_fds`] takes ownership of the fds via `nix`'s
//! `FromRawFd`: the caller must guarantee the fds are valid and not owned by
//! any other `OwnedFd`/`File`. [`PipeTransport::recv`] surfaces a clean peer
//! close as [`crate::WorkerError::Eof`].

use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::os::fd::{FromRawFd, RawFd};

use namtor_proto::framing::{decode, encode};
use namtor_proto::worker::{WorkerReq, WorkerRsp};

use crate::WorkerError;

/// Owns the two pipe ends (read, write) used by the worker.
///
/// The reader/writer are buffered so that multiple framed messages on the
/// same pipe are not lost between calls (a fresh `BufReader` per `recv` would
/// discard bytes it buffered but did not yet consume).
#[derive(Debug)]
pub struct PipeTransport {
    reader: BufReader<File>,
    writer: BufWriter<File>,
}

impl PipeTransport {
    /// Wrap two existing file descriptors.
    ///
    /// # Safety / ownership
    ///
    /// This consumes `read_fd` and `write_fd` via `FromRawFd`; the caller must
    /// ensure neither fd is otherwise owned (no double-close). Both fds must be
    /// valid open file descriptors; an invalid fd yields [`WorkerError::BadFd`].
    pub fn from_fds(read_fd: RawFd, write_fd: RawFd) -> Result<Self, WorkerError> {
        if read_fd < 0 || write_fd < 0 {
            return Err(WorkerError::BadFd(format!(
                "negative fd: read={read_fd} write={write_fd}"
            )));
        }
        // SAFETY: the caller asserts the fds are valid and unowned. We take
        // them into File, which closes the fd on Drop.
        let reader = unsafe { File::from_raw_fd(read_fd) };
        let writer = unsafe { File::from_raw_fd(write_fd) };
        Ok(Self {
            reader: BufReader::new(reader),
            writer: BufWriter::new(writer),
        })
    }

    /// Send a worker reply on the write end.
    pub fn send(&mut self, rsp: &WorkerRsp) -> Result<(), WorkerError> {
        encode(rsp, &mut self.writer)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Receive the next daemon request from the read end.
    ///
    /// A clean peer close (no bytes before EOF) is reported as
    /// [`WorkerError::Eof`]; a mid-frame truncation surfaces as
    /// [`namtor_proto::framing::ProtoError::Truncated`] (wrapped in
    /// [`WorkerError::Pipe`]).
    pub fn recv(&mut self) -> Result<WorkerReq, WorkerError> {
        let value = match decode(&mut self.reader) {
            Ok(v) => v,
            Err(namtor_proto::framing::ProtoError::Io(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                return Err(WorkerError::Eof);
            }
            Err(e) => return Err(e.into()),
        };
        let req = serde_json::from_value::<WorkerReq>(value)
            .map_err(namtor_proto::framing::ProtoError::Json)?;
        Ok(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_proto::framing::{decode, encode};
    use namtor_proto::worker::{Loaded, ModelSpec, Quant, WorkerReq, WorkerRsp};
    use std::io::Write;
    use std::os::fd::{AsFd, AsRawFd, FromRawFd};

    /// A minimal "daemon end" of the pipe: encodes `WorkerReq` and decodes
    /// `WorkerRsp` directly via framing, so we can exercise the worker's
    /// `PipeTransport` from the other side without a second typed transport.
    struct DaemonEnd {
        r: std::io::BufReader<std::fs::File>,
        w: std::io::BufWriter<std::fs::File>,
    }

    impl DaemonEnd {
        fn send_req(&mut self, req: &WorkerReq) {
            encode(req, &mut self.w).unwrap();
            self.w.flush().unwrap();
        }
        fn recv_rsp(&mut self) -> WorkerRsp {
            let v = decode(&mut self.r).unwrap();
            serde_json::from_value(v).unwrap()
        }
    }

    /// Build a worker `PipeTransport` plus a `DaemonEnd` from two socketpairs:
    /// one for daemon→worker requests, one for worker→daemon replies. This
    /// mirrors the real inherited-pipe topology (two one-way pipes).
    fn pair() -> (PipeTransport, DaemonEnd) {
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
        let (req_w, req_r) = sp(); // daemon writes req_w, worker reads req_r
        let (rsp_w, rsp_r) = sp(); // worker writes rsp_w, daemon reads rsp_r
        let req_w_raw = req_w.as_fd().as_raw_fd();
        let req_r_raw = req_r.as_fd().as_raw_fd();
        let rsp_w_raw = rsp_w.as_fd().as_raw_fd();
        let rsp_r_raw = rsp_r.as_fd().as_raw_fd();
        // The four OwnedFds above are adopted into File/transport via their
        // raw fds; forget the OwnedFds so they don't double-close on drop.
        std::mem::forget((req_w, req_r, rsp_w, rsp_r));
        let worker = PipeTransport::from_fds(req_r_raw, rsp_w_raw).expect("worker transport");
        // SAFETY: req_w_raw and rsp_r_raw are valid, unowned fds from socketpair.
        let daemon = DaemonEnd {
            r: std::io::BufReader::new(unsafe { std::fs::File::from_raw_fd(rsp_r_raw) }),
            w: std::io::BufWriter::new(unsafe { std::fs::File::from_raw_fd(req_w_raw) }),
        };
        (worker, daemon)
    }

    #[test]
    fn round_trips_load_request() {
        let (mut worker, mut daemon) = pair();
        let req = WorkerReq::Load(ModelSpec {
            id: "test".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        });
        daemon.send_req(&req);
        let got = worker.recv().unwrap();
        assert_eq!(got, req);
    }

    #[test]
    fn round_trips_vector_reply() {
        let (mut worker, mut daemon) = pair();
        let rsp = WorkerRsp::Vector {
            vec: vec![0.1, 0.2, 0.3],
        };
        worker.send(&rsp).unwrap();
        let got = daemon.recv_rsp();
        assert_eq!(got, rsp);
    }

    #[test]
    fn round_trips_loaded_reply() {
        let (mut worker, mut daemon) = pair();
        let rsp = WorkerRsp::Loaded(Loaded {
            model_id: "m".into(),
            quant: Quant::Int8,
            input_shape: vec![1, 3, 112, 112],
            output_shape: vec![1, 192],
        });
        worker.send(&rsp).unwrap();
        let got = daemon.recv_rsp();
        assert_eq!(got, rsp);
    }

    #[test]
    fn round_trips_shutdown_request() {
        let (mut worker, mut daemon) = pair();
        daemon.send_req(&WorkerReq::Shutdown);
        let got = worker.recv().unwrap();
        assert_eq!(got, WorkerReq::Shutdown);
    }

    #[test]
    fn recv_returns_eof_when_peer_closes() {
        let (mut worker, daemon) = pair();
        drop(daemon);
        let err = worker.recv().unwrap_err();
        assert!(matches!(err, WorkerError::Eof), "got {err:?}");
    }

    #[test]
    fn from_fds_rejects_negative() {
        let err = PipeTransport::from_fds(-1, -1).unwrap_err();
        assert!(matches!(err, WorkerError::BadFd(_)), "got {err:?}");
    }

    #[test]
    fn multiple_requests_in_sequence() {
        let (mut worker, mut daemon) = pair();
        daemon.send_req(&WorkerReq::Shutdown);
        daemon.send_req(&WorkerReq::Load(ModelSpec {
            id: "x".into(),
            quant: Quant::Fp32,
            sha256: [1; 32],
        }));
        let first = worker.recv().unwrap();
        let second = worker.recv().unwrap();
        assert!(matches!(first, WorkerReq::Shutdown));
        match second {
            WorkerReq::Load(spec) => assert_eq!(spec.id, "x"),
            other => panic!("expected Load, got {other:?}"),
        }
    }
}
