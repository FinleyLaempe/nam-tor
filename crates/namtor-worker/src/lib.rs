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

pub mod infer;
#[cfg(feature = "onnx")]
pub mod infer_onnx;
pub mod pipe;

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
