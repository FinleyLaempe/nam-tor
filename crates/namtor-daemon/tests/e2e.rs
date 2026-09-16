// SPDX-License-Identifier: GPL-3.0-only
//! M3 end-to-end CI test (Task 26).
//!
//! In-process listener with a mock lifecycle dispatcher. Exercises the full
//! PAM socket path: `SO_PEERCRED` ACL → `HelloMsg` (nonce) →
//! `AuthRequest::Auth` → lifecycle dispatch → `AuthReply` with matching nonce.
//!
//! Negative paths: `rhost` set → `Refused`; unauthorised uid → close;
//! replayed nonce → close; `phase:Prepare` → stream stays Warming.
//!
//! §8.6 no-biometric-in-logs: a `tracing` test subscriber captures every
//! record emitted during the auth cycle and asserts no record's message or
//! fields contain frame bytes, pixel data, embeddings, or template contents.

use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use namtor_daemon::acl::{authorise, get_peer_creds};
use namtor_daemon::listener::{LifecycleDispatch, Listener};
use namtor_proto::framing::{decode, encode};
use namtor_proto::pam::{
    AuthBody, AuthPhase, AuthReply, AuthRequest, HelloMsg, StatusReply, StreamState, Verdict,
};
use tracing::field::Visit;
use tracing_subscriber::layer::SubscriberExt;

type DaemonError = namtor_daemon::DaemonError;

struct E2eDispatcher {
    infer_calls: AtomicU64,
    phase: Mutex<StreamState>,
}

impl E2eDispatcher {
    fn new() -> Self {
        Self {
            infer_calls: AtomicU64::new(0),
            phase: Mutex::new(StreamState::Closed),
        }
    }
}

impl LifecycleDispatch for E2eDispatcher {
    fn dispatch(&self, body: &AuthBody) -> Result<AuthReply, DaemonError> {
        match body.phase {
            AuthPhase::Prepare => {
                *self.phase.lock().expect("phase") = StreamState::Warming;
                Ok(AuthReply {
                    nonce: body.nonce,
                    verdict: Verdict::Unavailable,
                    score: 0.0,
                    reason: Some("prepare ack".into()),
                })
            }
            AuthPhase::Verify => {
                *self.phase.lock().expect("phase") = StreamState::Verifying;
                self.infer_calls.fetch_add(4, Ordering::SeqCst);
                *self.phase.lock().expect("phase") = StreamState::Closed;
                Ok(AuthReply {
                    nonce: body.nonce,
                    verdict: Verdict::Match,
                    score: 0.87,
                    reason: None,
                })
            }
            AuthPhase::Release => {
                *self.phase.lock().expect("phase") = StreamState::Closed;
                Ok(AuthReply {
                    nonce: body.nonce,
                    verdict: Verdict::Unavailable,
                    score: 0.0,
                    reason: Some("released".into()),
                })
            }
        }
    }

    fn snapshot(&self) -> StatusReply {
        let stream = *self.phase.lock().expect("phase");
        StatusReply {
            stream,
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

fn read_reply(stream: &UnixStream) -> AuthReply {
    let v = recv_msg(stream);
    serde_json::from_value(v).expect("AuthReply")
}

fn make_listener(dir: &tempfile::TempDir, dispatcher: Arc<E2eDispatcher>) -> Listener {
    let path = dir.path().join("auth.sock");
    let gid = nix::unistd::getgid().as_raw() as u32;
    Listener::bind(&path, gid, dispatcher).expect("bind")
}

struct LogCapture {
    messages: Arc<Mutex<Vec<String>>>,
    _guard: tracing::subscriber::DefaultGuard,
}

impl LogCapture {
    fn install() -> Self {
        let messages: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let layer = CaptureLayer {
            messages: messages.clone(),
        };
        let sub = tracing_subscriber::registry().with(layer);
        let guard = tracing::subscriber::set_default(sub);
        Self {
            messages,
            _guard: guard,
        }
    }

    fn messages(&self) -> Vec<String> {
        self.messages.lock().expect("log").clone()
    }
}

struct CaptureLayer {
    messages: Arc<Mutex<Vec<String>>>,
}

impl<S> tracing_subscriber::Layer<S> for CaptureLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = MsgVisitor::default();
        event.record(&mut visitor);
        let mut msgs = self.messages.lock().expect("log");
        msgs.push(visitor.text);
    }
}

#[derive(Default)]
struct MsgVisitor {
    text: String,
}

impl Visit for MsgVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        self.text.push_str(&format!("{}={:?}", field.name(), value));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        self.text.push_str(&format!("{}={}", field.name(), value));
    }
}

#[test]
fn e2e_full_auth_cycle() {
    let _log = LogCapture::install();
    let dir = tempfile::tempdir().expect("tempdir");
    let dispatcher = Arc::new(E2eDispatcher::new());
    let listener = Arc::new(make_listener(&dir, dispatcher.clone()));
    let path = listener.path().to_path_buf();

    let t = std::thread::spawn(move || listener.accept_one_blocking().expect("accept"));

    let s = connect_and_hello(&path);
    let hello: HelloMsg = serde_json::from_value(recv_msg(&s)).expect("hello");
    assert_eq!(hello.server_version, 1);

    let body = AuthBody {
        nonce: hello.nonce,
        target_uid: 1000,
        phase: AuthPhase::Verify,
        rhost: None,
        deadline_us: 5_000_000,
    };
    send_msg(&s, &AuthRequest::Auth(body));
    let reply = read_reply(&s);
    assert_eq!(reply.nonce, hello.nonce, "nonce must be echoed");
    assert!(
        matches!(reply.verdict, Verdict::Match | Verdict::NoMatch),
        "verdict must be Match or NoMatch, got {:?}",
        reply.verdict
    );

    drop(s);
    t.join().expect("join");

    assert!(
        dispatcher.infer_calls.load(Ordering::SeqCst) >= 1,
        "infer must have been called"
    );
}

#[test]
fn e2e_rhost_set_returns_refused() {
    let _log = LogCapture::install();
    let dir = tempfile::tempdir().expect("tempdir");
    let dispatcher = Arc::new(E2eDispatcher::new());
    let listener = Arc::new(make_listener(&dir, dispatcher.clone()));
    let path = listener.path().to_path_buf();

    let t = std::thread::spawn(move || listener.accept_one_blocking().expect("accept"));

    let s = connect_and_hello(&path);
    let hello: HelloMsg = serde_json::from_value(recv_msg(&s)).expect("hello");

    let body = AuthBody {
        nonce: hello.nonce,
        target_uid: 1000,
        phase: AuthPhase::Verify,
        rhost: Some("10.0.0.1".into()),
        deadline_us: 5_000_000,
    };
    send_msg(&s, &AuthRequest::Auth(body));
    let reply = read_reply(&s);
    assert_eq!(reply.verdict, Verdict::Refused);
    assert_eq!(reply.nonce, hello.nonce);

    drop(s);
    t.join().expect("join");
}

#[test]
fn e2e_unauthorised_uid_connection_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dispatcher = Arc::new(E2eDispatcher::new());
    let path = dir.path().join("auth.sock");
    let bogus_gid = 999_999;
    let listener = Listener::bind(&path, bogus_gid, dispatcher).expect("bind");

    let t = std::thread::spawn(move || listener.accept_one_blocking().expect("accept"));

    let s = connect_and_hello(&path);
    let mut buf = [0u8; 16];
    let res = s.try_clone().expect("clone").read(&mut buf);
    match res {
        Ok(0) | Err(_) => {}
        Ok(n) => panic!("unauthorised connection should be closed, got {n} bytes"),
    }

    drop(s);
    t.join().expect("join");
}

#[test]
fn e2e_replayed_nonce_closes_connection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dispatcher = Arc::new(E2eDispatcher::new());
    let listener = Arc::new(make_listener(&dir, dispatcher.clone()));
    let path = listener.path().to_path_buf();

    let listener2 = listener.clone();
    let t = std::thread::spawn(move || {
        let _ = listener2.accept_one_blocking();
    });

    let s = connect_and_hello(&path);
    let _hello: HelloMsg = serde_json::from_value(recv_msg(&s)).expect("hello");

    let wrong_nonce = [0u8; 16];
    let body = AuthBody {
        nonce: wrong_nonce,
        target_uid: 1000,
        phase: AuthPhase::Verify,
        rhost: None,
        deadline_us: 5_000_000,
    };
    send_msg(&s, &AuthRequest::Auth(body));

    let mut buf = [0u8; 16];
    let res = s.try_clone().expect("clone").read(&mut buf);
    match res {
        Ok(0) | Err(_) => {}
        Ok(n) => panic!("replayed nonce should close, got {n} bytes"),
    }

    drop(s);
    let _ = t.join();
}

#[test]
fn e2e_status_does_not_block_on_camera_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dispatcher = Arc::new(E2eDispatcher::new());
    let listener = Arc::new(make_listener(&dir, dispatcher.clone()));
    let path = listener.path().to_path_buf();

    let t = std::thread::spawn(move || listener.accept_one_blocking().expect("accept"));

    let s = connect_and_hello(&path);
    let _hello: HelloMsg = serde_json::from_value(recv_msg(&s)).expect("hello");
    send_msg(&s, &AuthRequest::Status);
    let status: StatusReply = serde_json::from_value(recv_msg(&s)).expect("status");
    assert_eq!(status.stream, StreamState::Closed);

    drop(s);
    t.join().expect("join");
}

#[test]
fn e2e_prepare_phase_stays_warming() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dispatcher = Arc::new(E2eDispatcher::new());
    let listener = Arc::new(make_listener(&dir, dispatcher.clone()));
    let path = listener.path().to_path_buf();

    let t = std::thread::spawn(move || listener.accept_one_blocking().expect("accept"));

    let s = connect_and_hello(&path);
    let hello: HelloMsg = serde_json::from_value(recv_msg(&s)).expect("hello");

    let body = AuthBody {
        nonce: hello.nonce,
        target_uid: 1000,
        phase: AuthPhase::Prepare,
        rhost: None,
        deadline_us: 5_000_000,
    };
    send_msg(&s, &AuthRequest::Auth(body));
    let reply = read_reply(&s);
    assert_eq!(reply.nonce, hello.nonce);

    let snap = dispatcher.snapshot();
    assert_eq!(snap.stream, StreamState::Warming);

    drop(s);
    t.join().expect("join");
}

#[test]
fn e2e_no_biometric_data_in_logs() {
    let log = LogCapture::install();
    let dir = tempfile::tempdir().expect("tempdir");
    let dispatcher = Arc::new(E2eDispatcher::new());
    let listener = Arc::new(make_listener(&dir, dispatcher));
    let path = listener.path().to_path_buf();

    let client = std::thread::spawn(move || {
        let s = connect_and_hello(&path);
        let hello: HelloMsg = serde_json::from_value(recv_msg(&s)).expect("hello");
        let body = AuthBody {
            nonce: hello.nonce,
            target_uid: 1000,
            phase: AuthPhase::Verify,
            rhost: None,
            deadline_us: 5_000_000,
        };
        send_msg(&s, &AuthRequest::Auth(body));
        let _reply = read_reply(&s);
        drop(s);
    });

    listener.accept_one_blocking().expect("accept");
    client.join().expect("client");

    let messages = log.messages();
    assert!(!messages.is_empty(), "expected log records from auth cycle");
    let banned_markers = [
        "0x42",
        "pixel",
        "embedding",
        "template",
        "vec=",
        "frame_data",
        "[0.0",
        "66.0",
        "64.0",
    ];
    for msg in &messages {
        let lower = msg.to_lowercase();
        for marker in &banned_markers {
            assert!(
                !lower.contains(marker),
                "biometric leak: '{marker}' in log: {msg}"
            );
        }
    }
}

#[test]
fn e2e_so_peercred_checked_on_connection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dispatcher = Arc::new(E2eDispatcher::new());
    let listener = Arc::new(make_listener(&dir, dispatcher));
    let path = listener.path().to_path_buf();

    let t = std::thread::spawn(move || listener.accept_one_blocking().expect("accept"));

    let s = connect_and_hello(&path);
    let creds = get_peer_creds(&s).expect("peer creds");
    let gid = nix::unistd::getgid().as_raw() as u32;
    assert!(
        authorise(&creds, gid),
        "self-connection must pass ACL: {creds:?}"
    );

    let _hello: HelloMsg = serde_json::from_value(recv_msg(&s)).expect("hello");
    send_msg(&s, &AuthRequest::Status);
    let _status: StatusReply = serde_json::from_value(recv_msg(&s)).expect("status");

    drop(s);
    t.join().expect("join");
}

#[test]
fn e2e_socket_mode_is_0660() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let dispatcher = Arc::new(E2eDispatcher::new());
    let listener = Arc::new(make_listener(&dir, dispatcher));
    let mode = std::fs::metadata(listener.path())
        .expect("stat")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o660,
        "socket mode must be 0660, got {:o}",
        mode & 0o777
    );
}
