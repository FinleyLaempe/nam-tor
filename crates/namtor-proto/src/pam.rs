// SPDX-License-Identifier: GPL-3.0-only
//! PAM wire: daemon↔PAM socket messages.
//!
//! The daemon issues a nonce on connect ([`HelloMsg`]); the client echoes it in
//! [`AuthBody::nonce`] and the daemon returns it in [`AuthReply::nonce`], so a
//! captured success reply cannot be replayed against a new request (spec §3.2).
//!
//! [`AuthRequest`] is an enum carrying the three-phase lifecycle
//! ([`AuthPhase`]) for auth, plus a read-only [`Status`](AuthRequest::Status)
//! query used by `namtor-cli status` (no `camera_lock`, no serialisation
//! against auth — spec §3.2 "requests are serialised; the camera is an
//! exclusive resource" applies to auth, not status).

use serde::{Deserialize, Serialize};

/// Which lifecycle phase an auth request targets (spec §5.5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthPhase {
    /// Open the stream, apply emitter mode, let AE settle.
    Prepare,
    /// Score frames that are already arriving.
    Verify,
    /// Close the stream, emitter off.
    Release,
}

/// The body of an authentication request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthBody {
    /// Nonce issued by the daemon in [`HelloMsg`]; must be echoed back.
    pub nonce: [u8; 16],
    /// Which user to authenticate.
    pub target_uid: u32,
    /// Which lifecycle phase to drive.
    pub phase: AuthPhase,
    /// `PAM_RHOST`, if set. Presence forces [`Verdict::Refused`] (spec §8.2:
    /// remote authentication is never accepted).
    pub rhost: Option<String>,
    /// Hard deadline in microseconds since request issue; the daemon honours
    /// it and returns [`Verdict::Unavailable`] if it cannot complete in time.
    pub deadline_us: u64,
}

/// Who requested a stream open, for status reporting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requester {
    pub uid: u32,
    pub label: String,
}

/// Client → daemon request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthRequest {
    /// Drive an authentication lifecycle phase.
    Auth(AuthBody),
    /// Read-only status query (no camera lock).
    Status,
}

/// Daemon → client reply for an auth request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthReply {
    /// Echoes the request nonce.
    pub nonce: [u8; 16],
    pub verdict: Verdict,
    /// Scalar match score; meaningless for non-`Match`/`NoMatch` verdicts.
    pub score: f32,
    /// Optional human-readable reason; safe to surface (no biometric data).
    pub reason: Option<String>,
}

/// Auth outcome. Distinct from `Option<Match>` because `Refused` and
/// `Unavailable` must be distinguishable to the PAM module and in the journal.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Matched within the operating threshold.
    Match,
    /// Below threshold or no face found.
    NoMatch,
    /// Daemon/worker/camera unavailable; PAM falls through.
    Unavailable,
    /// Rate-limited; PAM should back off.
    RateLimited,
    /// Refused (e.g. `PAM_RHOST` set, spec §8.2).
    Refused,
}

/// Daemon → client status snapshot, in reply to [`AuthRequest::Status`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusReply {
    pub stream: StreamState,
    pub requester: Option<Requester>,
    /// Remaining warm-stream time in milliseconds, if a warm stream is open.
    pub warm_remaining_ms: Option<u64>,
}

/// Stream open state, mirrored from `namtor_cli::status` so the wire type is
/// self-contained (the CLI crate re-exports its own `StreamState` and converts
/// at the boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamState {
    Closed,
    Warming,
    Verifying,
}

/// First message the daemon sends after `SO_PEERCRED` passes: issues the nonce
/// the client must echo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloMsg {
    pub server_version: u16,
    pub nonce: [u8; 16],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(
        v: &T,
    ) -> T {
        let mut buf = Vec::new();
        crate::framing::encode(v, &mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let val = crate::framing::decode(&mut cursor).unwrap();
        serde_json::from_value(val).unwrap()
    }

    #[test]
    fn hello_round_trip() {
        let h = HelloMsg {
            server_version: 1,
            nonce: [0xff; 16],
        };
        let back = round_trip(&h);
        assert_eq!(back, h);
        assert_eq!(back.nonce.len(), 16);
    }

    #[test]
    fn auth_request_status_round_trip() {
        let req = AuthRequest::Status;
        let back = round_trip(&req);
        assert_eq!(back, req);
    }

    #[test]
    fn auth_request_auth_prepare_round_trip() {
        let body = AuthBody {
            nonce: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            target_uid: 1000,
            phase: AuthPhase::Prepare,
            rhost: None,
            deadline_us: 5_000_000,
        };
        let req = AuthRequest::Auth(body.clone());
        let back = round_trip(&req);
        assert_eq!(back, req);
    }

    #[test]
    fn auth_request_auth_verify_with_rhost_round_trip() {
        let body = AuthBody {
            nonce: [0; 16],
            target_uid: 0,
            phase: AuthPhase::Verify,
            rhost: Some("10.0.0.1".into()),
            deadline_us: 5_000_000,
        };
        let req = AuthRequest::Auth(body);
        let back: AuthRequest = round_trip(&req);
        match back {
            AuthRequest::Auth(b) => {
                assert_eq!(b.rhost.as_deref(), Some("10.0.0.1"));
                assert_eq!(b.phase, AuthPhase::Verify);
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[test]
    fn auth_reply_match_round_trip() {
        let reply = AuthReply {
            nonce: [0xab; 16],
            verdict: Verdict::Match,
            score: 0.83,
            reason: None,
        };
        let back = round_trip(&reply);
        assert_eq!(back, reply);
        assert_eq!(back.verdict, Verdict::Match);
    }

    #[test]
    fn auth_reply_refused_round_trip() {
        let reply = AuthReply {
            nonce: [0; 16],
            verdict: Verdict::Refused,
            score: 0.0,
            reason: Some("rhost set".into()),
        };
        let back = round_trip(&reply);
        assert_eq!(back, reply);
    }

    #[test]
    fn status_reply_round_trip() {
        let s = StatusReply {
            stream: StreamState::Warming,
            requester: Some(Requester {
                uid: 1000,
                label: "gdm".into(),
            }),
            warm_remaining_ms: Some(12300),
        };
        let back = round_trip(&s);
        assert_eq!(back, s);
    }

    #[test]
    fn auth_phase_snake_case() {
        assert_eq!(
            serde_json::to_string(&AuthPhase::Prepare).unwrap(),
            r#""prepare""#
        );
        assert_eq!(
            serde_json::to_string(&AuthPhase::Verify).unwrap(),
            r#""verify""#
        );
        assert_eq!(
            serde_json::to_string(&AuthPhase::Release).unwrap(),
            r#""release""#
        );
    }

    #[test]
    fn auth_request_tag_is_kind() {
        let json = serde_json::to_string(&AuthRequest::Status).unwrap();
        assert_eq!(json, r#"{"kind":"status"}"#);
    }

    #[test]
    fn verdict_snake_case() {
        assert_eq!(
            serde_json::to_string(&Verdict::NoMatch).unwrap(),
            r#""no_match""#
        );
        assert_eq!(
            serde_json::to_string(&Verdict::RateLimited).unwrap(),
            r#""rate_limited""#
        );
    }
}
