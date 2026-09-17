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
    /// The failure mode, as a closed set, so the client can render the
    /// message spec §8.3 requires without matching on `reason` text.
    ///
    /// `#[serde(default)]`: a daemon older than this field simply omits it and
    /// the client falls back to its verdict-level message.
    #[serde(default)]
    pub failure: Option<FailureReason>,
}

/// The failure modes of spec §8.3, as a closed enum.
///
/// The spec fixes a user-visible message per condition. `reason` is a free
/// string for the journal and cannot carry that mapping: deciding what to show
/// a user by substring-matching prose is how the wrong message gets shown
/// after an unrelated reword. This is the typed channel.
///
/// Note what is deliberately NOT distinguishable here at the *message* level:
/// [`Self::PadVeto`] and a plain non-match both read "Face not recognised", so
/// the system never tells an attacker which signal caught them. The variants
/// stay distinct so the journal records the truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    /// No IR camera present on the system.
    NoCamera,
    /// A camera is present but another application holds it.
    CameraBusy,
    /// The camera's privacy shutter is closed.
    PrivacyShutter,
    /// This user has no enrolled face. Silent by design — announcing it would
    /// tell anyone at the machine who is and is not enrolled.
    NotEnrolled,
    /// The client could not reach the daemon.
    DaemonUnreachable,
    /// Templates are sealed and the TPM is unavailable to unseal them.
    TemplatesSealed,
    /// The attempt ran out of time before a decision.
    Timeout,
    /// Presentation-attack detection vetoed the frame.
    PadVeto,
    /// Too many recent attempts for this user.
    RateLimited,
}

impl FailureReason {
    /// The message spec §8.3 requires for this condition.
    ///
    /// `None` means show nothing: the table marks "no enrollment for this
    /// user" as a silent fall-through.
    pub fn user_message(self) -> Option<&'static str> {
        match self {
            Self::NoCamera => Some("No infrared camera found"),
            Self::CameraBusy => Some("Camera is busy"),
            Self::PrivacyShutter => Some("Camera privacy shutter is closed"),
            Self::NotEnrolled => None,
            Self::DaemonUnreachable | Self::TemplatesSealed => Some("Face unlock unavailable"),
            Self::Timeout | Self::PadVeto => Some("Face not recognised"),
            Self::RateLimited => Some("Too many attempts"),
        }
    }

    /// Stable token for the journal. Distinct per condition even where two
    /// conditions share a user-visible message.
    pub fn journal_code(self) -> &'static str {
        match self {
            Self::NoCamera => "no_camera",
            Self::CameraBusy => "camera_busy",
            Self::PrivacyShutter => "privacy_shutter",
            Self::NotEnrolled => "not_enrolled",
            Self::DaemonUnreachable => "daemon_unreachable",
            Self::TemplatesSealed => "templates_sealed",
            Self::Timeout => "timeout",
            Self::PadVeto => "pad_veto",
            Self::RateLimited => "rate_limited",
        }
    }

    /// Every variant, so tests and the fault-injection matrix cannot silently
    /// miss one added later.
    pub const ALL: [Self; 9] = [
        Self::NoCamera,
        Self::CameraBusy,
        Self::PrivacyShutter,
        Self::NotEnrolled,
        Self::DaemonUnreachable,
        Self::TemplatesSealed,
        Self::Timeout,
        Self::PadVeto,
        Self::RateLimited,
    ];
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
            failure: None,
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
            failure: None,
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

#[cfg(test)]
mod failure_reason_tests {
    use super::*;

    /// Spec §8.3 fixes a message per condition. This is that table, verbatim.
    #[test]
    fn every_condition_renders_the_message_the_spec_requires() {
        let table = [
            (FailureReason::NoCamera, Some("No infrared camera found")),
            (FailureReason::CameraBusy, Some("Camera is busy")),
            (
                FailureReason::PrivacyShutter,
                Some("Camera privacy shutter is closed"),
            ),
            (FailureReason::NotEnrolled, None),
            (
                FailureReason::DaemonUnreachable,
                Some("Face unlock unavailable"),
            ),
            (
                FailureReason::TemplatesSealed,
                Some("Face unlock unavailable"),
            ),
            (FailureReason::Timeout, Some("Face not recognised")),
            (FailureReason::PadVeto, Some("Face not recognised")),
            (FailureReason::RateLimited, Some("Too many attempts")),
        ];
        assert_eq!(
            table.len(),
            FailureReason::ALL.len(),
            "a variant was added without a §8.3 message"
        );
        for (reason, expected) in table {
            assert_eq!(reason.user_message(), expected, "{reason:?}");
        }
    }

    /// A PAD veto must be indistinguishable from any other non-recognition in
    /// what the user sees, so the system never tells an attacker which signal
    /// caught them — while staying distinct in the journal.
    #[test]
    fn a_pad_veto_looks_exactly_like_a_plain_non_match() {
        assert_eq!(
            FailureReason::PadVeto.user_message(),
            FailureReason::Timeout.user_message(),
            "a PAD veto must not be distinguishable by its message"
        );
        assert_ne!(
            FailureReason::PadVeto.journal_code(),
            FailureReason::Timeout.journal_code(),
            "but the journal must record which it really was"
        );
    }

    #[test]
    fn journal_codes_are_unique_and_machine_readable() {
        let mut codes: Vec<&str> = FailureReason::ALL
            .iter()
            .map(|r| r.journal_code())
            .collect();
        codes.sort_unstable();
        let before = codes.len();
        codes.dedup();
        assert_eq!(before, codes.len(), "duplicate journal code: {codes:?}");
        for c in codes {
            assert!(
                c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'),
                "journal code {c:?} should be a snake_case token"
            );
        }
    }

    /// The field is optional so a daemon older than it can still be parsed.
    #[test]
    fn a_reply_without_the_field_still_parses() {
        let json = r#"{"nonce":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
                       "verdict":"unavailable","score":0.0,"reason":"old daemon"}"#;
        let reply: AuthReply = serde_json::from_str(json).expect("parse");
        assert_eq!(reply.failure, None);
    }

    #[test]
    fn the_field_round_trips() {
        let reply = AuthReply {
            nonce: [7u8; 16],
            verdict: Verdict::Unavailable,
            score: 0.0,
            reason: Some("camera busy".into()),
            failure: Some(FailureReason::CameraBusy),
        };
        let back: AuthReply =
            serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
        assert_eq!(back.failure, Some(FailureReason::CameraBusy));
    }
}
