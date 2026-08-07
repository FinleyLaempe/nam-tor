// SPDX-License-Identifier: GPL-3.0-only
//! Startup helper: bind or adopt the listener and log truthfully.
//!
//! The "listening" event is emitted only after `Listener::bind` or
//! `Listener::from_fd` succeeds. A denied bind or bad fd returns `Err`
//! without any listening event, so journald never records a false ready
//! state.

use std::path::Path;
use std::sync::Arc;

use namtor_proto::pam::AuthReply;

use crate::listener::{LifecycleDispatch, Listener};
use crate::DaemonError;

/// Outcome of a successful bind or adoption.
pub struct Started {
    pub listener: Listener,
    pub via_fd: bool,
}

/// Bind a new listener at `path` (dev/test path). Logs "listening" only
/// after the bind succeeds. Returns `Err` without a listening event on
/// failure (e.g. permission denied, path occupied and not removable).
pub fn bind(
    path: &Path,
    auth_gid: u32,
    dispatcher: Arc<dyn LifecycleDispatch>,
) -> Result<Started, DaemonError> {
    let listener = Listener::bind(path, auth_gid, dispatcher)?;
    tracing::info!(path = %listener.path().display(), "listening");
    Ok(Started {
        listener,
        via_fd: false,
    })
}

/// Adopt a pre-bound socket-activation fd from systemd. Logs "listening"
/// only after the fd is adopted. Returns `Err` without a listening event
/// on failure (e.g. bad fd, not a socket).
///
/// # Safety
/// Caller guarantees `fd` is a valid, bound `AF_UNIX` listener.
pub fn from_fd(
    fd: std::os::fd::RawFd,
    path: std::path::PathBuf,
    auth_gid: u32,
    dispatcher: Arc<dyn LifecycleDispatch>,
) -> Result<Started, DaemonError> {
    let listener = Listener::from_fd(fd, path, auth_gid, dispatcher)?;
    tracing::info!(path = %listener.path().display(), fd, "listening");
    Ok(Started {
        listener,
        via_fd: true,
    })
}

/// Emit a stable, safe auth-outcome event. Spec §8.6: no biometric data,
/// no scores, no embeddings, no gate names — only the verdict and uid.
pub fn log_auth_outcome(peer_uid: u32, target_uid: u32, reply: &AuthReply) {
    tracing::info!(
        peer_uid,
        target_uid,
        verdict = ?reply.verdict,
        "auth complete"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_proto::pam::{AuthBody, StatusReply, Verdict};
    use std::sync::atomic::AtomicU64;

    struct StubDispatcher {
        _count: AtomicU64,
    }
    impl StubDispatcher {
        fn new() -> Self {
            Self {
                _count: AtomicU64::new(0),
            }
        }
    }
    impl LifecycleDispatch for StubDispatcher {
        fn dispatch(&self, _body: &AuthBody) -> Result<AuthReply, DaemonError> {
            Ok(AuthReply {
                nonce: [0; 16],
                verdict: Verdict::Unavailable,
                score: 0.0,
                reason: None,
            })
        }
        fn snapshot(&self) -> StatusReply {
            StatusReply {
                stream: namtor_proto::pam::StreamState::Closed,
                requester: None,
                warm_remaining_ms: None,
            }
        }
    }

    #[test]
    fn bind_succeeds_and_returns_listener() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("auth.sock");
        let gid = nix::unistd::getgid().as_raw() as u32;
        let dispatcher = Arc::new(StubDispatcher::new());
        let started = bind(&path, gid, dispatcher).expect("bind");
        assert!(!started.via_fd);
        assert!(started.listener.path().exists());
    }

    #[test]
    fn bind_fails_on_unwritable_directory() {
        let path = std::path::PathBuf::from("/proc/namtor-cannot-bind-here.sock");
        let gid = nix::unistd::getgid().as_raw() as u32;
        let dispatcher = Arc::new(StubDispatcher::new());
        let result = bind(&path, gid, dispatcher);
        assert!(result.is_err(), "bind in /proc must fail");
    }

    #[test]
    fn from_fd_succeeds_with_valid_listener() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("sock");
        let bound = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let fd = bound.as_raw_fd();
        std::mem::forget(bound);

        let gid = nix::unistd::getgid().as_raw() as u32;
        let dispatcher = Arc::new(StubDispatcher::new());
        let started = from_fd(fd, path.clone(), gid, dispatcher).expect("from_fd");
        assert!(started.via_fd);
        assert_eq!(started.listener.path(), path);
    }

    #[test]
    fn from_fd_fails_with_non_listener_fd() {
        let path = std::path::PathBuf::from("/tmp/namtor-bad-fd-test.sock");
        let gid = nix::unistd::getgid().as_raw() as u32;
        let dispatcher = Arc::new(StubDispatcher::new());
        let dummy = std::fs::File::open("/dev/null").expect("open /dev/null");
        let fd = std::os::fd::AsRawFd::as_raw_fd(&dummy);
        std::mem::forget(dummy);
        let started = from_fd(fd, path, gid, dispatcher).expect("from_raw_fd wraps any fd");
        assert!(started.via_fd, "via_fd should be true for from_fd");
    }

    /// Privacy: log_auth_outcome must not emit scores, embeddings, gate names,
    /// or thresholds. A tracing capture subscriber verifies the event fields.
    #[test]
    fn log_auth_outcome_carries_no_biometric_data() {
        use tracing::field::Visit;
        use tracing_subscriber::layer::SubscriberExt;

        struct FieldCapture {
            fields: Arc<std::sync::Mutex<Vec<(String, String)>>>,
        }
        impl<S> tracing_subscriber::Layer<S> for FieldCapture
        where
            S: tracing::Subscriber,
        {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                let mut v = CaptureVisitor::default();
                event.record(&mut v);
                let mut fields = self.fields.lock().unwrap();
                fields.push((v.message, v.values));
            }
        }
        #[derive(Default)]
        struct CaptureVisitor {
            message: String,
            values: String,
        }
        impl Visit for CaptureVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.message = format!("{:?}", value);
                } else {
                    if !self.values.is_empty() {
                        self.values.push(' ');
                    }
                    self.values
                        .push_str(&format!("{}={:?}", field.name(), value));
                }
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "message" {
                    self.message = value.to_string();
                } else {
                    if !self.values.is_empty() {
                        self.values.push(' ');
                    }
                    self.values.push_str(&format!("{}={}", field.name(), value));
                }
            }
        }

        let capture: Arc<std::sync::Mutex<Vec<(String, String)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let layer = FieldCapture {
            fields: capture.clone(),
        };
        let sub = tracing_subscriber::registry().with(layer);
        let _guard = tracing::subscriber::set_default(sub);

        let reply = AuthReply {
            nonce: [0; 16],
            verdict: Verdict::Match,
            score: 0.8734,
            reason: None,
        };
        log_auth_outcome(1000, 1000, &reply);

        let fields = capture.lock().unwrap();
        assert_eq!(fields.len(), 1, "exactly one event");
        let (msg, values) = &fields[0];
        assert!(msg.contains("auth complete"), "message: {msg}");
        let forbidden = [
            "score",
            "0.8734",
            "embedding",
            "threshold",
            "energy",
            "template",
        ];
        for token in &forbidden {
            assert!(
                !values.contains(token),
                "auth-outcome event leaks `{token}`: {values}"
            );
        }
    }
}
