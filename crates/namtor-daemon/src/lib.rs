// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-daemon`: the privilege-separated authentication daemon.
//!
//! Runs as user `namtor` (groups `video`, `tss`). Owns the camera, the emitter,
//! and the worker supervisor. Listens on an `AF_UNIX` socket (`SO_PEERCRED`
//! ACL, nonce-bound replies — spec §3.2) and drives the warm-models /
//! cold-camera lifecycle (spec §5.5).

pub mod acl;
pub mod backoff;
pub mod daemon;
pub mod dbus;
pub mod dispatch;
pub mod fusion;
pub mod licence;
pub mod lifecycle;
pub mod listener;
pub mod manager;
pub mod models;
pub mod nonce;
pub mod policy;
pub mod presence;
pub mod startup;
pub mod supervisor;
pub mod verify;
pub mod znorm;

use thiserror::Error;

/// Daemon-wide error.
#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("socket error: {0}")]
    Socket(String),
    #[error("ACL error: {0}")]
    Acl(String),
    #[error("protocol error: {0}")]
    Proto(#[from] namtor_proto::framing::ProtoError),
    #[error("worker error: {0}")]
    Worker(String),
    #[error("state error: {0}")]
    State(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("capture error: {0}")]
    Capture(String),
}
