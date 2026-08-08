// SPDX-License-Identifier: GPL-3.0-only
//! Socket client to the Nam-tor daemon.
//!
//! Connects to `/run/namtor/auth.sock` (override with `NAMTOR_SOCKET`),
//! reads the daemon's [`HelloMsg`] nonce, drives a single auth lifecycle
//! (Prepare → Verify → Release), and returns the [`AuthReply`] from Verify.
//!
//! Fail-closed (spec §3.4): any I/O, framing, or protocol error yields
//! [`ClientError`], which the PAM module maps to `PAM_AUTHINFO_UNAVAIL`.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use namtor_proto::framing::{decode, encode, ProtoError};
use namtor_proto::pam::{AuthBody, AuthPhase, AuthReply, AuthRequest, HelloMsg, Verdict};

use crate::ffi;

/// Default daemon socket (spec §3.2).
pub const DEFAULT_SOCKET: &str = "/run/namtor/auth.sock";

/// Default hard timeout (spec §3.4: 5 s).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Errors that can occur while talking to the daemon. All map to
/// `PAM_AUTHINFO_UNAVAIL` in the PAM module.
#[derive(Debug)]
pub enum ClientError {
    Io(std::io::Error),
    Proto(ProtoError),
    /// Daemon sent a nonce we could not parse.
    BadHello,
    /// Daemon returned a verdict without a matching nonce echo.
    NonceMismatch,
    /// Daemon returned `Verdict::Refused` (e.g. `PAM_RHOST` set, spec §8.2).
    Refused,
    /// Daemon returned `Verdict::RateLimited` (spec §8.5).
    RateLimited,
    /// Daemon returned `Verdict::Unavailable` (camera/worker/TPM unavailable).
    Unavailable,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(e) => write!(f, "io: {e}"),
            ClientError::Proto(e) => write!(f, "proto: {e}"),
            ClientError::BadHello => write!(f, "bad hello"),
            ClientError::NonceMismatch => write!(f, "nonce mismatch"),
            ClientError::Refused => write!(f, "refused"),
            ClientError::RateLimited => write!(f, "rate-limited"),
            ClientError::Unavailable => write!(f, "unavailable"),
        }
    }
}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        ClientError::Io(e)
    }
}
impl From<ProtoError> for ClientError {
    fn from(e: ProtoError) -> Self {
        ClientError::Proto(e)
    }
}

/// Configuration for a single auth attempt.
pub struct AuthConfig {
    pub socket: PathBuf,
    pub timeout: Duration,
    pub target_uid: u32,
    pub rhost: Option<String>,
}

impl AuthConfig {
    /// Build an auth config from a live PAM handle.
    ///
    /// # Safety
    /// `pamh` must be a valid PAM handle from the framework, valid for the
    /// duration of this call.
    pub unsafe fn from_pam(
        pamh: *mut ffi::PamHandle,
        target_uid: u32,
    ) -> Result<Self, std::ffi::c_int> {
        let rhost = ffi::get_item_string(pamh, ffi::PAM_RHOST).ok().flatten();
        let socket = std::env::var("NAMTOR_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.into());
        Ok(Self {
            socket: PathBuf::from(socket),
            timeout: DEFAULT_TIMEOUT,
            target_uid,
            rhost,
        })
    }
}

/// Drive one full auth lifecycle (Prepare → Verify → Release) against the
/// daemon. Returns the `AuthReply` from the Verify phase, or a
/// [`ClientError`] on any failure.
pub fn authenticate(cfg: &AuthConfig) -> Result<AuthReply, ClientError> {
    let mut stream = UnixStream::connect(&cfg.socket)?;
    stream.set_read_timeout(Some(cfg.timeout))?;
    stream.set_write_timeout(Some(cfg.timeout))?;

    // 1. Read HelloMsg (nonce).
    let hello_val = decode(&mut stream)?;
    let hello: HelloMsg = serde_json::from_value(hello_val).map_err(|_| ClientError::BadHello)?;
    let nonce = hello.nonce;

    // 2. Prepare phase. Prepare returns Unavailable as a phase ack — only
    // the Verify verdict is classified.
    let deadline_us = (cfg.timeout.as_micros() as u64).max(1);
    let prepare = AuthRequest::Auth(AuthBody {
        nonce,
        target_uid: cfg.target_uid,
        phase: AuthPhase::Prepare,
        rhost: cfg.rhost.clone(),
        deadline_us,
    });
    encode(&prepare, &mut stream)?;
    let prep_val = decode(&mut stream)?;
    let prep_reply: AuthReply =
        serde_json::from_value(prep_val).map_err(|_| ClientError::BadHello)?;
    check_nonce(&prep_reply, &nonce)?;

    // 3. Verify phase — the verdict that matters.
    let verify = AuthRequest::Auth(AuthBody {
        nonce,
        target_uid: cfg.target_uid,
        phase: AuthPhase::Verify,
        rhost: cfg.rhost.clone(),
        deadline_us,
    });
    encode(&verify, &mut stream)?;
    let ver_val = decode(&mut stream)?;
    let verify_reply: AuthReply =
        serde_json::from_value(ver_val).map_err(|_| ClientError::BadHello)?;
    let verdict = classify(&verify_reply, &nonce);

    // 4. Release phase — symmetric bounded acknowledgment. We send Release
    // and read the daemon's ack, bounded by the socket timeout. The verdict
    // is already in hand, so any failure here is swallowed quietly: the
    // daemon's connection-end cleanup guarantees capture/emitter release.
    let release = AuthRequest::Auth(AuthBody {
        nonce,
        target_uid: cfg.target_uid,
        phase: AuthPhase::Release,
        rhost: cfg.rhost.clone(),
        deadline_us,
    });
    if encode(&release, &mut stream).is_ok() {
        let _ = stream.flush();
        let _ = decode(&mut stream);
    }

    verdict?;
    Ok(verify_reply)
}

/// Check the nonce echo and map non-Match/NoMatch verdicts to errors.
fn check_nonce(reply: &AuthReply, expected_nonce: &[u8; 16]) -> Result<(), ClientError> {
    if reply.nonce != *expected_nonce {
        return Err(ClientError::NonceMismatch);
    }
    Ok(())
}

fn classify(reply: &AuthReply, expected_nonce: &[u8; 16]) -> Result<(), ClientError> {
    check_nonce(reply, expected_nonce)?;
    match reply.verdict {
        Verdict::Match | Verdict::NoMatch => Ok(()),
        Verdict::Refused => Err(ClientError::Refused),
        Verdict::RateLimited => Err(ClientError::RateLimited),
        Verdict::Unavailable => Err(ClientError::Unavailable),
    }
}

/// Whether the socket exists. Cheap pre-check before we try to connect.
pub fn daemon_reachable(socket: &Path) -> bool {
    Path::new(socket).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_proto::pam::{AuthRequest, HelloMsg};
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    #[test]
    fn default_socket_matches_layout_contract() {
        assert_eq!(
            DEFAULT_SOCKET,
            namtor_layout::PAM_DEFAULT_SOCKET,
            "PAM DEFAULT_SOCKET must match the install-layout contract constant"
        );
    }

    /// A tiny in-process daemon that speaks the protocol and returns a fixed verdict.
    fn mock_daemon(path: &Path, verdict: Verdict) -> std::thread::JoinHandle<std::io::Result<()>> {
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let (mut conn, _) = listener.accept()?;
            let hello = HelloMsg {
                server_version: 1,
                nonce: [0xaa; 16],
            };
            encode(&hello, &mut conn).map_err(|e| std::io::Error::other(e.to_string()))?;
            while let Ok(val) = decode(&mut conn) {
                let req: AuthRequest = match serde_json::from_value(val) {
                    Ok(r) => r,
                    Err(_) => break,
                };
                let nonce = match req {
                    AuthRequest::Auth(b) => b.nonce,
                    AuthRequest::Status => break,
                };
                let reply = AuthReply {
                    nonce,
                    verdict,
                    score: 0.0,
                    reason: None,
                };
                encode(&reply, &mut conn).map_err(|e| std::io::Error::other(e.to_string()))?;
            }
            Ok(())
        })
    }

    /// In-process daemon that records each received phase on `phase_tx` and
    /// replies with `verdict`, breaking after Release so the client can
    /// observe all three phases (Prepare → Verify → Release).
    fn mock_daemon_recording(
        listener: UnixListener,
        verdict: Verdict,
        phase_tx: mpsc::Sender<AuthPhase>,
    ) -> std::thread::JoinHandle<std::io::Result<()>> {
        std::thread::spawn(move || -> std::io::Result<()> {
            let (mut conn, _) = listener.accept()?;
            let hello = HelloMsg {
                server_version: 1,
                nonce: [0xaa; 16],
            };
            encode(&hello, &mut conn).map_err(|e| std::io::Error::other(e.to_string()))?;
            while let Ok(val) = decode(&mut conn) {
                let req: AuthRequest = serde_json::from_value(val)
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
                let AuthRequest::Auth(body) = req else {
                    continue;
                };
                phase_tx
                    .send(body.phase)
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
                let reply = AuthReply {
                    nonce: body.nonce,
                    verdict,
                    score: 0.0,
                    reason: None,
                };
                encode(&reply, &mut conn).map_err(|e| std::io::Error::other(e.to_string()))?;
                if body.phase == AuthPhase::Release {
                    break;
                }
            }
            Ok(())
        })
    }

    /// Assert the client drives Prepare → Verify → Release for `verdict`,
    /// regardless of whether the verdict is a success or an error variant.
    fn assert_full_lifecycle(verdict: Verdict) {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("auth.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let (phase_tx, phase_rx) = mpsc::channel();
        let h = mock_daemon_recording(listener, verdict, phase_tx);
        let cfg = make_cfg(&sock);
        let _ = authenticate(&cfg);
        assert_eq!(phase_rx.recv().unwrap(), AuthPhase::Prepare);
        assert_eq!(phase_rx.recv().unwrap(), AuthPhase::Verify);
        assert_eq!(phase_rx.recv().unwrap(), AuthPhase::Release);
        drop(dir);
        let _ = h.join();
    }

    fn make_cfg(path: &Path) -> AuthConfig {
        AuthConfig {
            socket: path.to_path_buf(),
            timeout: Duration::from_secs(2),
            target_uid: 1000,
            rhost: None,
        }
    }

    #[test]
    fn authenticate_match_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("auth.sock");
        let h = mock_daemon(&sock, Verdict::Match);
        let cfg = make_cfg(&sock);
        let reply = authenticate(&cfg).expect("should succeed");
        assert_eq!(reply.verdict, Verdict::Match);
        drop(dir);
        let _ = h.join();
    }

    #[test]
    fn authenticate_no_match_is_returned_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("auth.sock");
        let h = mock_daemon(&sock, Verdict::NoMatch);
        let cfg = make_cfg(&sock);
        let reply = authenticate(&cfg).expect("NoMatch is a valid reply");
        assert_eq!(reply.verdict, Verdict::NoMatch);
        drop(dir);
        let _ = h.join();
    }

    #[test]
    fn authenticate_unavailable_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("auth.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let (phase_tx, phase_rx) = mpsc::channel();
        let h = mock_daemon_recording(listener, Verdict::Unavailable, phase_tx);
        let cfg = make_cfg(&sock);
        let err = authenticate(&cfg).unwrap_err();
        assert!(matches!(err, ClientError::Unavailable));
        assert_eq!(phase_rx.recv().unwrap(), AuthPhase::Prepare);
        assert_eq!(phase_rx.recv().unwrap(), AuthPhase::Verify);
        assert_eq!(phase_rx.recv().unwrap(), AuthPhase::Release);
        drop(dir);
        let _ = h.join();
    }

    #[test]
    fn authenticate_refused_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("auth.sock");
        let h = mock_daemon(&sock, Verdict::Refused);
        let cfg = make_cfg(&sock);
        let err = authenticate(&cfg).unwrap_err();
        assert!(matches!(err, ClientError::Refused));
        drop(dir);
        let _ = h.join();
    }

    #[test]
    fn authenticate_rate_limited_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("auth.sock");
        let h = mock_daemon(&sock, Verdict::RateLimited);
        let cfg = make_cfg(&sock);
        let err = authenticate(&cfg).unwrap_err();
        assert!(matches!(err, ClientError::RateLimited));
        drop(dir);
        let _ = h.join();
    }

    #[test]
    fn release_phase_sent_for_every_verdict_variant() {
        // The Release phase must be sent before the verdict is returned to
        // the caller, for every verdict — including the success variants.
        assert_full_lifecycle(Verdict::Match);
        assert_full_lifecycle(Verdict::NoMatch);
        assert_full_lifecycle(Verdict::Unavailable);
        assert_full_lifecycle(Verdict::RateLimited);
        assert_full_lifecycle(Verdict::Refused);
    }

    #[test]
    fn authenticate_connection_refused() {
        let cfg = AuthConfig {
            socket: PathBuf::from("/tmp/namtor-definitely-missing.sock"),
            timeout: Duration::from_millis(100),
            target_uid: 0,
            rhost: None,
        };
        let err = authenticate(&cfg).unwrap_err();
        assert!(matches!(err, ClientError::Io(_)));
    }

    #[test]
    fn daemon_reachable_checks_existence() {
        assert!(!daemon_reachable(Path::new(
            "/tmp/namtor-no-such-socket-xyz"
        )));
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("s.sock");
        std::fs::write(&sock, b"x").unwrap();
        assert!(daemon_reachable(&sock));
    }

    #[test]
    fn nonce_mismatch_detected() {
        let reply = AuthReply {
            nonce: [0xbb; 16],
            verdict: Verdict::Match,
            score: 0.0,
            reason: None,
        };
        let err = classify(&reply, &[0xaa; 16]).unwrap_err();
        assert!(matches!(err, ClientError::NonceMismatch));
    }

    /// The client must read the Release ack (symmetric bounded Release).
    /// A daemon that closes the socket immediately after the Verify reply
    /// (no Release ack) must not cause the client to fail — the verdict is
    /// already in hand and errors are swallowed quietly.
    #[test]
    fn release_ack_failure_is_swallowed_quietly() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("auth.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let h = std::thread::spawn(move || -> std::io::Result<()> {
            let (mut conn, _) = listener.accept()?;
            let hello = HelloMsg {
                server_version: 1,
                nonce: [0xaa; 16],
            };
            encode(&hello, &mut conn).map_err(|e| std::io::Error::other(e.to_string()))?;
            for _ in 0..2 {
                let val = decode(&mut conn).map_err(|e| std::io::Error::other(e.to_string()))?;
                let req: AuthRequest = serde_json::from_value(val)
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
                let AuthRequest::Auth(body) = req else {
                    continue;
                };
                let reply = AuthReply {
                    nonce: body.nonce,
                    verdict: if body.phase == AuthPhase::Verify {
                        Verdict::Match
                    } else {
                        Verdict::Unavailable
                    },
                    score: 0.0,
                    reason: None,
                };
                encode(&reply, &mut conn).map_err(|e| std::io::Error::other(e.to_string()))?;
                if body.phase == AuthPhase::Verify {
                    break;
                }
            }
            Ok(())
        });

        let cfg = make_cfg(&sock);
        let reply = authenticate(&cfg).expect("Match verdict must be returned");
        assert_eq!(reply.verdict, Verdict::Match);
        drop(dir);
        let _ = h.join();
    }

    /// The client must send Release even when the Verify verdict is an error
    /// variant (Unavailable/Refused/RateLimited). The full lifecycle must
    /// complete on every path.
    #[test]
    fn release_sent_after_unavailable_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("auth.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let (phase_tx, phase_rx) = mpsc::channel();
        let h = mock_daemon_recording(listener, Verdict::Unavailable, phase_tx);
        let cfg = make_cfg(&sock);
        let _ = authenticate(&cfg);
        assert_eq!(phase_rx.recv().unwrap(), AuthPhase::Prepare);
        assert_eq!(phase_rx.recv().unwrap(), AuthPhase::Verify);
        assert_eq!(phase_rx.recv().unwrap(), AuthPhase::Release);
        drop(dir);
        let _ = h.join();
    }
}
