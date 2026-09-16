// SPDX-License-Identifier: GPL-3.0-only
//! Socket listener: `SO_PEERCRED` ACL + nonce + serialised request dispatch.
//!
//! The listener binds an `AF_UNIX` socket (mode 0660, owner root:namtor-auth),
//! accepts connections, checks credentials, issues a nonce, and dispatches
//! `AuthRequest`s to a `LifecycleDispatch` implementation. The camera is an
//! exclusive resource: auth requests are serialised by `camera_lock` (spec
//! §3.2). Status requests bypass the lock (read-only).

use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use namtor_proto::framing::{decode, encode};
use namtor_proto::pam::{
    AuthBody, AuthPhase, AuthReply, AuthRequest, HelloMsg, StatusReply, Verdict,
};

use crate::acl::{authorise, get_peer_creds};
use crate::nonce::NonceService;
use crate::DaemonError;

/// Trait the listener dispatches to. The real lifecycle implementation lives in
/// [`crate::daemon::Daemon`] via `DaemonDispatcher`; tests use a stub so the
/// ACL/nonce/framing can be verified independently.
pub trait LifecycleDispatch: Send + Sync {
    /// Drive an auth request. The caller already holds `camera_lock`.
    fn dispatch(&self, body: &AuthBody) -> Result<AuthReply, DaemonError>;
    /// Read-only status snapshot; does not need `camera_lock`.
    fn snapshot(&self) -> StatusReply;
}

/// How long [`Listener::accept_one`] waits before returning `Ok(None)` so the
/// caller can re-check its shutdown flag.
const POLL_TIMEOUT_MS: i32 = 200;

/// The socket listener.
pub struct Listener {
    path: PathBuf,
    listener: std::os::unix::net::UnixListener,
    auth_gid: u32,
    nonce: NonceService,
    camera_lock: Mutex<()>,
    dispatcher: Arc<dyn LifecycleDispatch>,
}

impl Listener {
    /// Bind a new listener at `path` with the given ACL gid and dispatcher.
    /// Sets socket mode 0660 (chown to root:namtor-auth is left to systemd or
    /// the installer; tests skip chown if not root).
    pub fn bind(
        path: &std::path::Path,
        auth_gid: u32,
        dispatcher: Arc<dyn LifecycleDispatch>,
    ) -> Result<Self, DaemonError> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let listener = std::os::unix::net::UnixListener::bind(path)?;
        // `accept_one` polls explicitly; a blocking listener would swallow
        // EINTR inside `accept()` and never hand control back to the caller.
        listener.set_nonblocking(true)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
        Ok(Self {
            path: path.to_path_buf(),
            listener,
            auth_gid,
            nonce: NonceService::new(),
            camera_lock: Mutex::new(()),
            dispatcher,
        })
    }

    /// Socket-activation seam: adopt a pre-bound listener fd handed off by
    /// systemd (`LISTEN_FDS=1`). The fd must already be bound to the right
    /// path with the right mode/owner; systemd's `namtor-daemon.socket` unit
    /// handles that. `path` is recorded for status/diagnostics only.
    pub fn from_fd(
        fd: std::os::fd::RawFd,
        path: PathBuf,
        auth_gid: u32,
        dispatcher: Arc<dyn LifecycleDispatch>,
    ) -> Result<Self, DaemonError> {
        // SAFETY: caller guarantees `fd` is a valid, bound AF_UNIX listener
        // that we now own (systemd passes it with CLOEXEC cleared on the
        // LISTEN_FDS fds).
        let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd) };
        // See `bind`: `accept_one` polls, so the listener must not block.
        listener.set_nonblocking(true)?;
        Ok(Self {
            path,
            listener,
            auth_gid,
            nonce: NonceService::new(),
            camera_lock: Mutex::new(()),
            dispatcher,
        })
    }

    /// Test seam: override the auth_gid for ACL-branch testing without
    /// needing a real `namtor-auth` group.
    pub fn with_auth_gid(mut self, gid: u32) -> Self {
        self.auth_gid = gid;
        self
    }

    /// Wait up to [`POLL_TIMEOUT_MS`] for one connection and serve it.
    ///
    /// Returns `Ok(Some(()))` when a connection was accepted and served to
    /// completion, and `Ok(None)` when the poll timed out or was interrupted
    /// by a signal — the caller must re-check its shutdown flag and call
    /// again. `accept()` on a blocking listener retries `EINTR` internally
    /// and would never return, which is why this polls explicitly; without it
    /// `SIGTERM` can only stop the daemon via `SIGKILL`.
    pub fn accept_one(&self) -> Result<Option<()>, DaemonError> {
        let mut fds = [libc::pollfd {
            fd: self.listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: FFI call into `poll(2)`. `fds` is a valid, correctly-sized
        // array of `pollfd` that outlives the call, the length matches, and
        // the fd is owned by `self.listener` for the whole call.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 1, POLL_TIMEOUT_MS) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(err.into());
        }
        if rc == 0 {
            return Ok(None);
        }
        let stream = match self.listener.accept() {
            Ok((stream, _)) => stream,
            // Another thread won the race for this connection.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        self.serve_connection(stream)?;
        Ok(Some(()))
    }

    /// Block until one connection has been accepted and served.
    ///
    /// One-shot seam for callers with no shutdown flag to re-check (tests,
    /// single-request harnesses). The daemon's main loop uses
    /// [`Listener::accept_one`] instead so a signal can interrupt it.
    pub fn accept_one_blocking(&self) -> Result<(), DaemonError> {
        loop {
            if self.accept_one()?.is_some() {
                return Ok(());
            }
        }
    }

    /// Serve one accepted connection, looping over requests until the client
    /// closes the connection or an error occurs. The PAM client sends
    /// Prepare → Verify → Release on a single connection.
    fn serve_connection(&self, stream: UnixStream) -> Result<(), DaemonError> {
        let creds = get_peer_creds(&stream)?;
        tracing::debug!(peer_uid = creds.uid, peer_gid = creds.gid, "connection");
        if !authorise(&creds, self.auth_gid) {
            tracing::warn!(peer_uid = creds.uid, "unauthorised connection refused");
            drop(stream);
            return Ok(());
        }

        let nonce = self.nonce.issue();
        let hello = HelloMsg {
            server_version: 1,
            nonce,
        };
        let writer = stream.try_clone()?;
        let mut writer = std::io::BufWriter::new(writer);
        encode(&hello, &mut writer)?;
        writer.flush()?;

        let reader = stream.try_clone()?;
        let mut reader = std::io::BufReader::new(reader);

        let mut nonce_validated = false;
        let mut last_auth_body: Option<AuthBody> = None;
        let mut released = false;

        loop {
            let value = match decode(&mut reader) {
                Ok(v) => v,
                Err(namtor_proto::framing::ProtoError::Io(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(e) => {
                    self.ensure_release(&last_auth_body, &mut released);
                    return Err(e.into());
                }
            };
            let req: AuthRequest = match serde_json::from_value(value) {
                Ok(r) => r,
                Err(e) => {
                    self.ensure_release(&last_auth_body, &mut released);
                    return Err(e.into());
                }
            };

            match req {
                AuthRequest::Status => {
                    tracing::debug!(peer_uid = creds.uid, "status request");
                    let status = self.dispatcher.snapshot();
                    if let Err(e) = encode(&status, &mut writer) {
                        self.ensure_release(&last_auth_body, &mut released);
                        return Err(e.into());
                    }
                    if let Err(e) = writer.flush() {
                        self.ensure_release(&last_auth_body, &mut released);
                        return Err(e.into());
                    }
                }
                AuthRequest::Auth(body) => {
                    if body.rhost.is_some() {
                        tracing::warn!(peer_uid = creds.uid, "rhost set — refused");
                        let reply = AuthReply {
                            nonce: body.nonce,
                            verdict: Verdict::Refused,
                            score: 0.0,
                            reason: Some("remote authentication refused".into()),
                        };
                        let _ = encode(&reply, &mut writer);
                        let _ = writer.flush();
                        return Ok(());
                    }

                    if !nonce_validated {
                        if let Err(e) = self.nonce.validate_reply(nonce, body.nonce) {
                            tracing::warn!(peer_uid = creds.uid, error = %e, "nonce validation failed");
                            drop(stream);
                            return Err(e);
                        }
                        nonce_validated = true;
                    }

                    tracing::debug!(
                        peer_uid = creds.uid,
                        target_uid = body.target_uid,
                        phase = ?body.phase,
                        "auth request"
                    );
                    let _lock = self.camera_lock.lock().expect("camera_lock poisoned");

                    if body.phase == AuthPhase::Release {
                        released = true;
                    }
                    last_auth_body = Some(body.clone());

                    match self.dispatcher.dispatch(&body) {
                        Ok(reply) => {
                            tracing::info!(
                                peer_uid = creds.uid,
                                target_uid = body.target_uid,
                                verdict = ?reply.verdict,
                                "auth phase complete"
                            );
                            if let Err(e) = encode(&reply, &mut writer) {
                                self.ensure_release(&last_auth_body, &mut released);
                                return Err(e.into());
                            }
                            if let Err(e) = writer.flush() {
                                self.ensure_release(&last_auth_body, &mut released);
                                return Err(e.into());
                            }
                        }
                        Err(e) => {
                            let reply = AuthReply {
                                nonce: body.nonce,
                                verdict: Verdict::Unavailable,
                                score: 0.0,
                                reason: Some(e.to_string()),
                            };
                            let _ = encode(&reply, &mut writer);
                            let _ = writer.flush();
                        }
                    }
                }
            }
        }

        self.ensure_release(&last_auth_body, &mut released);
        Ok(())
    }

    /// Guarantee capture/emitter release if the client disconnects after
    /// Prepare/Verify without sending Release. Idempotent: safe to call
    /// after a real Release (released=true) or when no auth started.
    fn ensure_release(&self, last: &Option<AuthBody>, released: &mut bool) {
        if *released {
            return;
        }
        let Some(body) = last else {
            return;
        };
        let cleanup = AuthBody {
            nonce: body.nonce,
            target_uid: body.target_uid,
            phase: AuthPhase::Release,
            rhost: None,
            deadline_us: body.deadline_us,
        };
        let _lock = self.camera_lock.lock().expect("camera_lock poisoned");
        let _ = self.dispatcher.dispatch(&cleanup);
        *released = true;
        tracing::debug!(peer_uid = body.target_uid, "cleanup release dispatched");
    }

    /// Path the listener is bound to.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_proto::pam::AuthPhase;
    use std::io::{BufReader, Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    /// Stub dispatcher: returns Unavailable for auth, closed status for status.
    struct StubDispatcher {
        call_count: AtomicU64,
    }

    impl StubDispatcher {
        fn new() -> Self {
            Self {
                call_count: AtomicU64::new(0),
            }
        }
    }

    impl LifecycleDispatch for StubDispatcher {
        fn dispatch(&self, body: &AuthBody) -> Result<AuthReply, DaemonError> {
            self.call_count.fetch_add(1, Ordering::Relaxed);
            Ok(AuthReply {
                nonce: body.nonce,
                verdict: Verdict::Unavailable,
                score: 0.0,
                reason: Some("stub".into()),
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

    fn connect_and_hello(path: &Path) -> UnixStream {
        UnixStream::connect(path).expect("connect")
    }

    fn recv_msg(stream: &UnixStream) -> serde_json::Value {
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        decode(&mut reader).expect("decode")
    }

    fn send_msg(stream: &UnixStream, msg: &impl serde::Serialize) {
        let mut writer = std::io::BufWriter::new(stream.try_clone().expect("clone"));
        encode(msg, &mut writer).expect("encode");
        writer.flush().expect("flush");
    }

    /// Spawn `accept_one_blocking` in a thread and return the handle.
    fn serve(listener: Listener) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let _ = listener.accept_one_blocking();
        })
    }

    #[test]
    fn accept_one_returns_none_on_poll_timeout() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let sock = tmp.path().join("auth.sock");
        let dispatcher = Arc::new(StubDispatcher::new());
        let listener =
            Listener::bind(&sock, nix::unistd::getgid().as_raw() as u32, dispatcher).expect("bind");
        let start = Instant::now();
        // No client connects. The call must return within the poll timeout
        // instead of blocking forever, so the caller can observe SHUTDOWN.
        let got = listener.accept_one().expect("accept_one");
        assert!(got.is_none(), "expected timeout, got a connection");
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn status_request_round_trips() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let sock = tmp.path().join("auth.sock");
        let dispatcher = Arc::new(StubDispatcher::new());
        let listener =
            Listener::bind(&sock, nix::unistd::getgid().as_raw() as u32, dispatcher).expect("bind");

        let _h = serve(listener);
        let stream = connect_and_hello(&sock);
        let hello: HelloMsg = serde_json::from_value(recv_msg(&stream)).expect("hello");
        assert_eq!(hello.server_version, 1);
        assert_eq!(hello.nonce.len(), 16);

        send_msg(&stream, &AuthRequest::Status);
        let status: StatusReply = serde_json::from_value(recv_msg(&stream)).expect("status");
        assert_eq!(status.stream, namtor_proto::pam::StreamState::Closed);
    }

    #[test]
    fn unauthorised_uid_connection_closed() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let sock = tmp.path().join("auth.sock");
        let dispatcher = Arc::new(StubDispatcher::new());
        if nix::unistd::getuid().is_root() {
            return;
        }
        let listener = Listener::bind(&sock, 999, dispatcher)
            .expect("bind")
            .with_auth_gid(999);

        let handle = std::thread::spawn(move || {
            listener.accept_one_blocking().expect("accept");
        });

        let mut stream = connect_and_hello(&sock);
        handle.join().expect("thread");

        // After accept_one returns, the connection is closed. A read should
        // return 0 (EOF) — no HelloMsg was sent.
        let mut buf = [0u8; 64];
        let n = stream.read(&mut buf).unwrap_or(0);
        assert_eq!(
            n, 0,
            "unauthorised connection should be closed with no data"
        );
    }

    #[test]
    fn rhost_set_returns_refused() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let sock = tmp.path().join("auth.sock");
        let dispatcher = Arc::new(StubDispatcher::new());
        let our_gid = nix::unistd::getgid().as_raw() as u32;
        let listener = Listener::bind(&sock, our_gid, dispatcher).expect("bind");

        let _h = serve(listener);
        let stream = connect_and_hello(&sock);
        let hello: HelloMsg = serde_json::from_value(recv_msg(&stream)).expect("hello");

        let body = AuthBody {
            nonce: hello.nonce,
            target_uid: 1000,
            phase: AuthPhase::Verify,
            rhost: Some("10.0.0.1".into()),
            deadline_us: 5_000_000,
        };
        send_msg(&stream, &AuthRequest::Auth(body));

        let reply: AuthReply = serde_json::from_value(recv_msg(&stream)).expect("reply");
        assert_eq!(reply.verdict, Verdict::Refused);
    }

    #[test]
    fn auth_request_round_trips_with_stub() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let sock = tmp.path().join("auth.sock");
        let dispatcher = Arc::new(StubDispatcher::new());
        let our_gid = nix::unistd::getgid().as_raw() as u32;
        let listener = Listener::bind(&sock, our_gid, dispatcher).expect("bind");

        let _h = serve(listener);
        let stream = connect_and_hello(&sock);
        let hello: HelloMsg = serde_json::from_value(recv_msg(&stream)).expect("hello");

        let body = AuthBody {
            nonce: hello.nonce,
            target_uid: 1000,
            phase: AuthPhase::Verify,
            rhost: None,
            deadline_us: 5_000_000,
        };
        send_msg(&stream, &AuthRequest::Auth(body));

        let reply: AuthReply = serde_json::from_value(recv_msg(&stream)).expect("reply");
        assert_eq!(reply.nonce, hello.nonce);
        assert_eq!(reply.verdict, Verdict::Unavailable);
    }

    #[test]
    fn first_auth_consumes_nonce() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let sock = tmp.path().join("auth.sock");
        let dispatcher = Arc::new(StubDispatcher::new());
        let our_gid = nix::unistd::getgid().as_raw() as u32;
        let listener = Listener::bind(&sock, our_gid, dispatcher).expect("bind");

        let _h = serve(listener);
        let stream = connect_and_hello(&sock);
        let hello: HelloMsg = serde_json::from_value(recv_msg(&stream)).expect("hello");

        let body = AuthBody {
            nonce: hello.nonce,
            target_uid: 1000,
            phase: AuthPhase::Verify,
            rhost: None,
            deadline_us: 5_000_000,
        };
        send_msg(&stream, &AuthRequest::Auth(body));
        let reply: AuthReply = serde_json::from_value(recv_msg(&stream)).expect("reply");
        assert_eq!(reply.verdict, Verdict::Unavailable);
    }

    #[test]
    fn socket_mode_is_0660() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let sock = tmp.path().join("auth.sock");
        let dispatcher = Arc::new(StubDispatcher::new());
        let _listener = Listener::bind(&sock, 999, dispatcher).expect("bind");

        let meta = std::fs::metadata(&sock).expect("metadata");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o660, "socket mode should be 0660, got {mode:o}");
    }

    #[test]
    fn concurrent_connections_serialise_on_camera_lock() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let sock = tmp.path().join("auth.sock");
        let dispatcher = Arc::new(SlowDispatcher::new());
        let our_gid = nix::unistd::getgid().as_raw() as u32;
        let listener = Listener::bind(&sock, our_gid, dispatcher).expect("bind");

        // SlowDispatcher sleeps 100ms in dispatch, so two concurrent auth
        // requests should take ~200ms total (serialised by camera_lock).
        let listener = Arc::new(listener);
        let l1 = Arc::clone(&listener);
        let l2 = Arc::clone(&listener);
        let t1 = std::thread::spawn(move || l1.accept_one_blocking());
        let t2 = std::thread::spawn(move || l2.accept_one_blocking());

        let s1 = connect_and_hello(&sock);
        let s2 = connect_and_hello(&sock);

        let h1: HelloMsg = serde_json::from_value(recv_msg(&s1)).expect("hello1");
        let h2: HelloMsg = serde_json::from_value(recv_msg(&s2)).expect("hello2");

        let body1 = AuthBody {
            nonce: h1.nonce,
            target_uid: 1,
            phase: AuthPhase::Verify,
            rhost: None,
            deadline_us: 5_000_000,
        };
        let body2 = AuthBody {
            nonce: h2.nonce,
            target_uid: 2,
            phase: AuthPhase::Verify,
            rhost: None,
            deadline_us: 5_000_000,
        };

        let start = Instant::now();
        send_msg(&s1, &AuthRequest::Auth(body1));
        send_msg(&s2, &AuthRequest::Auth(body2));

        let _r1: AuthReply = serde_json::from_value(recv_msg(&s1)).expect("reply1");
        let _r2: AuthReply = serde_json::from_value(recv_msg(&s2)).expect("reply2");
        let elapsed = start.elapsed();

        drop(s1);
        drop(s2);

        assert!(
            elapsed >= Duration::from_millis(180),
            "expected serialised dispatch (~200ms), got {elapsed:?}"
        );

        t1.join().expect("t1").expect("accept1");
        t2.join().expect("t2").expect("accept2");
    }

    struct SlowDispatcher;
    impl SlowDispatcher {
        fn new() -> Self {
            SlowDispatcher
        }
    }
    impl LifecycleDispatch for SlowDispatcher {
        fn dispatch(&self, body: &AuthBody) -> Result<AuthReply, DaemonError> {
            std::thread::sleep(Duration::from_millis(100));
            Ok(AuthReply {
                nonce: body.nonce,
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
    fn from_fd_adopts_pre_bound_listener() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sock");
        let bound = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let fd = bound.as_raw_fd();
        std::mem::forget(bound);

        let dispatcher = Arc::new(StubDispatcher::new());
        let gid = nix::unistd::getgid().as_raw() as u32;
        let listener = Listener::from_fd(fd, path.clone(), gid, dispatcher).expect("from_fd");

        assert_eq!(listener.path(), path);

        let t = std::thread::spawn(move || listener.accept_one_blocking().expect("accept"));
        let s = connect_and_hello(&path);
        let hello: HelloMsg = serde_json::from_value(recv_msg(&s)).expect("hello");
        assert_eq!(hello.server_version, 1);
        send_msg(&s, &AuthRequest::Status);
        let _status: StatusReply = serde_json::from_value(recv_msg(&s)).expect("status");
        drop(s);
        t.join().expect("join");
    }

    /// Dispatcher that records the sequence of phases it received.
    struct ReleaseTrackingDispatcher {
        phases: Arc<Mutex<Vec<AuthPhase>>>,
    }

    impl ReleaseTrackingDispatcher {
        fn new() -> (Self, Arc<Mutex<Vec<AuthPhase>>>) {
            let phases = Arc::new(Mutex::new(Vec::new()));
            let d = Self {
                phases: phases.clone(),
            };
            (d, phases)
        }
    }

    impl LifecycleDispatch for ReleaseTrackingDispatcher {
        fn dispatch(&self, body: &AuthBody) -> Result<AuthReply, DaemonError> {
            self.phases.lock().expect("phases").push(body.phase);
            Ok(AuthReply {
                nonce: body.nonce,
                verdict: match body.phase {
                    AuthPhase::Verify => Verdict::Match,
                    _ => Verdict::Unavailable,
                },
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

    /// If the client disconnects after Verify without sending Release, the
    /// listener must dispatch a synthetic Release to guarantee capture/emitter
    /// cleanup.
    #[test]
    fn cleanup_release_on_disconnect_after_verify() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let sock = tmp.path().join("auth.sock");
        let (dispatcher, phases) = ReleaseTrackingDispatcher::new();
        let our_gid = nix::unistd::getgid().as_raw() as u32;
        let listener = Listener::bind(&sock, our_gid, Arc::new(dispatcher)).expect("bind");

        let h = serve(listener);

        let s = connect_and_hello(&sock);
        let hello: HelloMsg = serde_json::from_value(recv_msg(&s)).expect("hello");
        let body = AuthBody {
            nonce: hello.nonce,
            target_uid: 1000,
            phase: AuthPhase::Verify,
            rhost: None,
            deadline_us: 5_000_000,
        };
        send_msg(&s, &AuthRequest::Auth(body));
        let _reply: AuthReply = serde_json::from_value(recv_msg(&s)).expect("reply");
        drop(s);
        h.join().expect("join");

        let recorded = phases.lock().expect("phases").clone();
        assert_eq!(
            recorded,
            vec![AuthPhase::Verify, AuthPhase::Release],
            "listener must dispatch cleanup Release after disconnect"
        );
    }

    /// If the client sends Release explicitly, the listener must NOT dispatch
    /// a second cleanup Release.
    #[test]
    fn explicit_release_prevents_cleanup_release() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let sock = tmp.path().join("auth.sock");
        let (dispatcher, phases) = ReleaseTrackingDispatcher::new();
        let our_gid = nix::unistd::getgid().as_raw() as u32;
        let listener = Listener::bind(&sock, our_gid, Arc::new(dispatcher)).expect("bind");

        let h = serve(listener);

        let s = connect_and_hello(&sock);
        let hello: HelloMsg = serde_json::from_value(recv_msg(&s)).expect("hello");
        for phase in [AuthPhase::Verify, AuthPhase::Release] {
            let body = AuthBody {
                nonce: hello.nonce,
                target_uid: 1000,
                phase,
                rhost: None,
                deadline_us: 5_000_000,
            };
            send_msg(&s, &AuthRequest::Auth(body));
            let _reply: AuthReply = serde_json::from_value(recv_msg(&s)).expect("reply");
        }
        drop(s);
        h.join().expect("join");

        let recorded = phases.lock().expect("phases").clone();
        assert_eq!(
            recorded,
            vec![AuthPhase::Verify, AuthPhase::Release],
            "explicit Release must suppress cleanup Release"
        );
    }

    /// If no auth was started (only Status), no cleanup Release is dispatched.
    #[test]
    fn no_cleanup_release_without_auth() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let sock = tmp.path().join("auth.sock");
        let (dispatcher, phases) = ReleaseTrackingDispatcher::new();
        let our_gid = nix::unistd::getgid().as_raw() as u32;
        let listener = Listener::bind(&sock, our_gid, Arc::new(dispatcher)).expect("bind");

        let h = serve(listener);

        let s = connect_and_hello(&sock);
        let _hello: HelloMsg = serde_json::from_value(recv_msg(&s)).expect("hello");
        send_msg(&s, &AuthRequest::Status);
        let _status: StatusReply = serde_json::from_value(recv_msg(&s)).expect("status");
        drop(s);
        h.join().expect("join");

        let recorded = phases.lock().expect("phases").clone();
        assert!(recorded.is_empty(), "no auth → no cleanup Release");
    }
}
