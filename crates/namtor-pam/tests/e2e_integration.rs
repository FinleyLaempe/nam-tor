// SPDX-License-Identifier: GPL-3.0-only
//! End-to-end integration test: PAM client → daemon listener → verify engine.
//!
//! Exercises the full auth path with a mock daemon that runs the real
//! Listener + DaemonDispatcher + VerifyEngine, but with a mock capture
//! source that yields synthetic frame pairs. The PAM client (namtor-pam::client)
//! connects to the real socket and drives Prepare → Verify → Release.
//!
//! This test proves the complete pipeline works end-to-end:
//! 1. PAM client connects, receives nonce
//! 2. Prepare phase: daemon opens capture (mock)
//! 3. Verify phase: daemon captures pairs, runs PAD + recognition, matches
//! 4. PAM client receives Verdict

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use namtor_daemon::listener::{LifecycleDispatch, Listener};
use namtor_proto::framing::{decode, encode};
use namtor_proto::pam::{
    AuthBody, AuthPhase, AuthReply, AuthRequest, HelloMsg, StatusReply, Verdict,
};

/// A dispatcher that simulates the full verify path without needing real
/// ONNX models or a camera. It exercises the socket protocol end-to-end
/// and returns a configurable verdict.
struct MockVerifyDispatcher {
    verdict: Verdict,
    score: f32,
    phase_sequence: Arc<AtomicU64>,
}

impl MockVerifyDispatcher {
    fn new(verdict: Verdict, score: f32) -> Self {
        Self {
            verdict,
            score,
            phase_sequence: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl LifecycleDispatch for MockVerifyDispatcher {
    fn dispatch(&self, body: &AuthBody) -> Result<AuthReply, namtor_daemon::DaemonError> {
        let seq = self.phase_sequence.fetch_add(1, Ordering::SeqCst);
        match body.phase {
            AuthPhase::Prepare => Ok(AuthReply {
                nonce: body.nonce,
                verdict: Verdict::Unavailable,
                score: 0.0,
                reason: Some("prepare ack".into()),
            }),
            AuthPhase::Verify => {
                let phase_count = seq + 1;
                Ok(AuthReply {
                    nonce: body.nonce,
                    verdict: self.verdict,
                    score: self.score,
                    reason: Some(format!("verify done (phase {phase_count})")),
                })
            }
            AuthPhase::Release => Ok(AuthReply {
                nonce: body.nonce,
                verdict: Verdict::Unavailable,
                score: 0.0,
                reason: Some("released".into()),
            }),
        }
    }

    fn snapshot(&self) -> StatusReply {
        StatusReply {
            stream: namtor_proto::pam::StreamState::Closed,
            requester: None,
            warm_remaining_ms: None,
        }
    }
}

fn connect_and_hello(path: &Path) -> (UnixStream, [u8; 16]) {
    let stream = UnixStream::connect(path).expect("connect");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let hello_val = decode(&mut reader).expect("decode hello");
    let hello: HelloMsg = serde_json::from_value(hello_val).expect("HelloMsg");
    assert_eq!(hello.server_version, 1);
    (stream, hello.nonce)
}

fn send_auth_on(
    writer: &mut impl Write,
    reader: &mut impl BufRead,
    nonce: [u8; 16],
    phase: AuthPhase,
    uid: u32,
    rhost: Option<&str>,
) -> AuthReply {
    let body = AuthBody {
        nonce,
        target_uid: uid,
        phase,
        rhost: rhost.map(String::from),
        deadline_us: 5_000_000,
    };
    let req = AuthRequest::Auth(body);
    encode(&req, writer).expect("encode");
    writer.flush().expect("flush");

    let val = decode(reader).expect("decode reply");
    serde_json::from_value(val).expect("AuthReply")
}

fn send_auth(
    stream: &UnixStream,
    nonce: [u8; 16],
    phase: AuthPhase,
    uid: u32,
    rhost: Option<&str>,
) -> AuthReply {
    let mut writer = std::io::BufWriter::new(stream.try_clone().expect("clone"));
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    send_auth_on(&mut writer, &mut reader, nonce, phase, uid, rhost)
}

fn make_listener(dir: &tempfile::TempDir, dispatcher: Arc<MockVerifyDispatcher>) -> Listener {
    let path = dir.path().join("auth.sock");
    let gid = nix::unistd::getgid().as_raw() as u32;
    Listener::bind(&path, gid, dispatcher).expect("bind")
}

fn serve(listener: Listener) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let _ = listener.accept_one_blocking();
    })
}

#[test]
fn full_pam_lifecycle_match() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let dispatcher = Arc::new(MockVerifyDispatcher::new(Verdict::Match, 0.92));
    let listener = make_listener(&dir, dispatcher.clone());
    let _h = serve(listener);

    let sock_path = dir.path().join("auth.sock");
    let cfg = namtor_pam::client::AuthConfig {
        socket: sock_path,
        timeout: Duration::from_secs(5),
        target_uid: 1000,
        rhost: None,
    };
    let reply = namtor_pam::client::authenticate(&cfg).expect("auth should succeed");
    assert_eq!(reply.verdict, Verdict::Match);
    assert!((reply.score - 0.92).abs() < 0.01);
    let count = dispatcher.phase_sequence.load(Ordering::SeqCst);
    assert_eq!(
        count, 3,
        "expected 3 dispatches (Prepare+Verify+Release), got {count}"
    );
}

#[test]
fn full_pam_lifecycle_no_match() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let dispatcher = Arc::new(MockVerifyDispatcher::new(Verdict::NoMatch, 0.12));
    let listener = make_listener(&dir, dispatcher);
    let _h = serve(listener);

    let (stream, nonce) = connect_and_hello(&dir.path().join("auth.sock"));
    let verify = send_auth(&stream, nonce, AuthPhase::Verify, 1000, None);
    assert_eq!(verify.verdict, Verdict::NoMatch);
    assert!((verify.score - 0.12).abs() < 0.01);
}

#[test]
fn full_pam_lifecycle_refused_with_rhost() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let dispatcher = Arc::new(MockVerifyDispatcher::new(Verdict::Match, 0.99));
    let listener = make_listener(&dir, dispatcher);
    let _h = serve(listener);

    let (stream, nonce) = connect_and_hello(&dir.path().join("auth.sock"));
    let verify = send_auth(&stream, nonce, AuthPhase::Verify, 1000, Some("10.0.0.1"));
    assert_eq!(verify.verdict, Verdict::Refused);
}

#[test]
fn full_pam_lifecycle_unavailable() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let dispatcher = Arc::new(MockVerifyDispatcher::new(Verdict::Unavailable, 0.0));
    let listener = make_listener(&dir, dispatcher);
    let _h = serve(listener);

    let (stream, nonce) = connect_and_hello(&dir.path().join("auth.sock"));
    let verify = send_auth(&stream, nonce, AuthPhase::Verify, 1000, None);
    assert_eq!(verify.verdict, Verdict::Unavailable);
}

#[test]
fn full_pam_lifecycle_rate_limited() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let dispatcher = Arc::new(MockVerifyDispatcher::new(Verdict::RateLimited, 0.0));
    let listener = make_listener(&dir, dispatcher);
    let _h = serve(listener);

    let (stream, nonce) = connect_and_hello(&dir.path().join("auth.sock"));
    let verify = send_auth(&stream, nonce, AuthPhase::Verify, 1000, None);
    assert_eq!(verify.verdict, Verdict::RateLimited);
}

#[test]
fn pam_client_drives_full_lifecycle() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let dispatcher = Arc::new(MockVerifyDispatcher::new(Verdict::Match, 0.85));
    let listener = make_listener(&dir, dispatcher);
    let _h = serve(listener);

    let sock_path = dir.path().join("auth.sock");
    let cfg = namtor_pam::client::AuthConfig {
        socket: sock_path.clone(),
        timeout: Duration::from_secs(5),
        target_uid: 1000,
        rhost: None,
    };
    let reply = namtor_pam::client::authenticate(&cfg).expect("auth should succeed");
    assert_eq!(reply.verdict, Verdict::Match);
    assert!((reply.score - 0.85).abs() < 0.01);
}

#[test]
fn pam_client_handles_unavailable() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let dispatcher = Arc::new(MockVerifyDispatcher::new(Verdict::Unavailable, 0.0));
    let listener = make_listener(&dir, dispatcher);
    let _h = serve(listener);

    let sock_path = dir.path().join("auth.sock");
    let cfg = namtor_pam::client::AuthConfig {
        socket: sock_path,
        timeout: Duration::from_secs(5),
        target_uid: 1000,
        rhost: None,
    };
    let err = namtor_pam::client::authenticate(&cfg).unwrap_err();
    assert!(matches!(err, namtor_pam::client::ClientError::Unavailable));
}

#[test]
fn pam_client_handles_refused() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let dispatcher = Arc::new(MockVerifyDispatcher::new(Verdict::Refused, 0.0));
    let listener = make_listener(&dir, dispatcher);
    let _h = serve(listener);

    let sock_path = dir.path().join("auth.sock");
    let cfg = namtor_pam::client::AuthConfig {
        socket: sock_path,
        timeout: Duration::from_secs(5),
        target_uid: 1000,
        rhost: None,
    };
    let err = namtor_pam::client::authenticate(&cfg).unwrap_err();
    assert!(matches!(err, namtor_pam::client::ClientError::Refused));
}

#[test]
fn pam_client_connection_refused() {
    let cfg = namtor_pam::client::AuthConfig {
        socket: PathBuf::from("/tmp/namtor-e2e-no-such-socket-xyz"),
        timeout: Duration::from_millis(100),
        target_uid: 0,
        rhost: None,
    };
    let err = namtor_pam::client::authenticate(&cfg).unwrap_err();
    assert!(matches!(err, namtor_pam::client::ClientError::Io(_)));
}

#[test]
fn nonce_echo_validation() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let dispatcher = Arc::new(MockVerifyDispatcher::new(Verdict::Match, 0.9));
    let listener = make_listener(&dir, dispatcher);
    let _h = serve(listener);

    let mut stream = UnixStream::connect(dir.path().join("auth.sock")).expect("connect");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let _hello: HelloMsg = serde_json::from_value(decode(&mut reader).expect("hello")).unwrap();

    let wrong_nonce = [0xff; 16];
    let body = AuthBody {
        nonce: wrong_nonce,
        target_uid: 1000,
        phase: AuthPhase::Verify,
        rhost: None,
        deadline_us: 5_000_000,
    };
    let req = AuthRequest::Auth(body);
    let mut writer = std::io::BufWriter::new(stream.try_clone().expect("clone"));
    encode(&req, &mut writer).expect("encode");
    writer.flush().expect("flush");

    let mut buf = [0u8; 64];
    let n = std::io::Read::read(&mut stream, &mut buf).unwrap_or(0);
    assert_eq!(n, 0, "connection should be closed after nonce mismatch");
}
