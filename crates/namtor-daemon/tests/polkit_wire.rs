// SPDX-License-Identifier: GPL-3.0-only
//! The polkit exchange, against a server that answers exactly as polkitd does.
//!
//! `CheckAuthorization` takes a `(sa{sv})` subject and returns a single
//! `(bba{ss})` struct. Both directions were exercised by nothing: every test
//! uses `SystemPolkit::for_testing()`, whose `conn: None` returns before the
//! call. A mismatch in either one makes `check()` fall into its
//! deserialise-error arm and deny every privileged operation — delete, PAM
//! toggle, cross-user enrollment — with no prompt and nothing in the journal
//! to distinguish it from a polkit rule that says no.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;

use namtor_daemon::dbus::{action, AuthzDecision, PeerIdentity, PolkitAuthority};
use namtor_daemon::manager::polkit::SystemPolkit;
use serde::{Deserialize, Serialize};
use zbus::zvariant::{OwnedValue, Type};

/// polkit's `AuthorizationResult`: one struct, `(bba{ss})`.
#[derive(Debug, Serialize, Deserialize, Type)]
struct AuthorizationResult {
    is_authorized: bool,
    is_challenge: bool,
    details: HashMap<String, String>,
}

/// What the fake authority should answer, and what it saw.
#[derive(Clone, Copy)]
enum Answer {
    Authorized,
    Challenge,
    Denied,
}

struct FakeAuthority {
    answer: Answer,
    seen: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl FakeAuthority {
    #[zbus(name = "CheckAuthorization")]
    fn check_authorization(
        &self,
        subject: (String, HashMap<String, OwnedValue>),
        action_id: String,
        _details: HashMap<String, String>,
        _flags: u32,
        _cancellation_id: String,
    ) -> zbus::fdo::Result<AuthorizationResult> {
        let (kind, props) = subject;
        let name = props
            .get("name")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_default();
        self.seen
            .lock()
            .expect("seen poisoned")
            .push((format!("{kind}:{name}"), action_id));
        let (is_authorized, is_challenge) = match self.answer {
            Answer::Authorized => (true, false),
            Answer::Challenge => (false, true),
            Answer::Denied => (false, false),
        };
        Ok(AuthorizationResult {
            is_authorized,
            is_challenge,
            details: HashMap::new(),
        })
    }
}

type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

/// A `SystemPolkit` wired to a fake authority on a private p2p pair, plus the
/// record of what that authority received. The server connection is returned
/// so it stays alive for the life of the test.
fn authority(answer: Answer) -> (SystemPolkit, Seen, zbus::Connection) {
    let (a, b) = UnixStream::pair().expect("socketpair");
    let guid = zbus::Guid::generate();
    let seen: Seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let seen_server = std::sync::Arc::clone(&seen);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("fake_polkit".into())
        .spawn(move || {
            let result = zbus::block_on(async {
                zbus::connection::Builder::unix_stream(a)
                    .server(guid)
                    .map_err(|e| zbus::Error::Failure(e.to_string()))?
                    .p2p()
                    .serve_at(
                        "/org/freedesktop/PolicyKit1/Authority",
                        FakeAuthority {
                            answer,
                            seen: seen_server,
                        },
                    )?
                    .build()
                    .await
            });
            let _ = tx.send(result);
        })
        .expect("spawn fake polkit");

    let client = zbus::blocking::connection::Builder::unix_stream(b)
        .p2p()
        .build()
        .expect("client connection");
    let server = rx.recv().expect("server channel").expect("server built");
    (SystemPolkit::with_connection(client), seen, server)
}

fn peer() -> PeerIdentity {
    PeerIdentity::from_bus(1000, 4242, "namtor-enroll-wizard".into(), ":1.42".into())
}

/// The fake is only worth anything if it answers in polkit's shape. polkit's
/// `AuthorizationResult` is documented as `(bba{ss})`; if this fake drifted
/// to three separate out-arguments, the tests below would pass against a
/// reply the real authority never sends.
#[test]
fn the_fake_answers_in_polkits_shape() {
    assert_eq!(AuthorizationResult::SIGNATURE.to_string(), "(bba{ss})");
}

/// And the subject goes out in polkit's shape too.
#[test]
fn the_subject_is_sent_in_polkits_shape() {
    type Subject = (String, HashMap<String, OwnedValue>);
    assert_eq!(Subject::SIGNATURE.to_string(), "(sa{sv})");
}

/// The whole point: an authorized answer must come back as `Allowed`. If the
/// reply struct did not deserialise, this would be `Denied` and look exactly
/// like a polkit rule refusing.
#[test]
fn an_authorized_reply_is_allowed() {
    let (pk, seen, _server) = authority(Answer::Authorized);
    assert_eq!(
        pk.check(action::DELETE, &peer(), 1000),
        AuthzDecision::Allowed
    );
    let seen = seen.lock().expect("seen");
    assert_eq!(seen.len(), 1, "exactly one CheckAuthorization call");
    assert_eq!(
        seen[0],
        (
            "system-bus-name::1.42".to_string(),
            action::DELETE.to_string()
        ),
        "the subject and action must arrive as polkit expects them"
    );
}

#[test]
fn a_challenge_reply_is_a_challenge() {
    let (pk, _seen, _server) = authority(Answer::Challenge);
    assert_eq!(
        pk.check(action::CONFIGURE_PAM, &peer(), 1000),
        AuthzDecision::Challenge
    );
}

#[test]
fn a_denied_reply_is_denied() {
    let (pk, _seen, _server) = authority(Answer::Denied);
    assert_eq!(
        pk.check(action::CONFIGURE, &peer(), 1000),
        AuthzDecision::Denied
    );
}

/// Every action the daemon can ask about must survive the round trip. A
/// serialisation that worked for one and not another would be worse than one
/// that never worked.
#[test]
fn every_action_round_trips() {
    let (pk, seen, _server) = authority(Answer::Authorized);
    let actions = [
        action::ENROLL_OTHER,
        action::DELETE,
        action::CONFIGURE,
        action::CONFIGURE_PAM,
    ];
    for a in actions {
        assert_eq!(pk.check(a, &peer(), 999), AuthzDecision::Allowed, "{a}");
    }
    let seen = seen.lock().expect("seen");
    assert_eq!(seen.len(), actions.len());
    for (i, a) in actions.iter().enumerate() {
        assert_eq!(&seen[i].1, a);
    }
}

/// A peer with no bus name is a socket peer — the PAM module — and must never
/// reach polkit at all.
#[test]
fn a_peer_without_a_bus_name_never_reaches_polkit() {
    let (pk, seen, _server) = authority(Answer::Authorized);
    let socket_peer = PeerIdentity::from_bus(0, 1, "pam_namtor".into(), String::new());
    assert_eq!(
        pk.check(action::DELETE, &socket_peer, 0),
        AuthzDecision::Denied
    );
    assert!(
        seen.lock().expect("seen").is_empty(),
        "a socket peer must not be asked about"
    );
}

/// Self-enrollment is answered from the bus daemon's credentials, not by
/// polkit — the daemon is not a trusted caller for a subject belonging to
/// another identity.
#[test]
fn self_enrollment_does_not_ask_polkit() {
    let (pk, seen, _server) = authority(Answer::Denied);
    assert_eq!(
        pk.check(action::ENROLL_SELF, &peer(), 1000),
        AuthzDecision::Allowed
    );
    assert!(seen.lock().expect("seen").is_empty());
}
