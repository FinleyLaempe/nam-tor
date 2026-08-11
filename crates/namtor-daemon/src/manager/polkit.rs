// SPDX-License-Identifier: GPL-3.0-only
//! Polkit authorization for the `org.namtor.Manager` D-Bus interface (spec §9.1,
//! plan task 12).
//!
//! Two [`PolkitAuthority`](crate::dbus::PolkitAuthority) implementations:
//!
//! - [`SystemPolkit`] — real polkit via `org.freedesktop.PolicyKit1.Authority`
//!   on the system bus. Holds an `Option<zbus::blocking::Connection>`; `None`
//!   when the system bus or polkitd is unavailable → every check fails closed
//!   (`Denied`), **except** self-enrollment (see below). The D-Bus method
//!   `CheckAuthorization(subject, action_id, details, flags, cancellation_id)`
//!   is called with `flags = 0` (no user interaction). If polkit returns
//!   `is_authorized` → [`AuthzDecision::Allowed`]; if `is_challenge` →
//!   [`AuthzDecision::Challenge`]; otherwise [`AuthzDecision::Denied`]. Any
//!   D-Bus error → `Denied` (fail closed).
//!
//!   **Self-enrollment short-circuit.** When `action_id == action::ENROLL_SELF`
//!   and `peer.uid == target_uid` and `peer.bus_name` is non-empty, the check
//!   returns [`AuthzDecision::Allowed`] without contacting polkit. The daemon
//!   runs as user `namtor` and is not a trusted caller for `CheckAuthorization`
//!   with a `system-bus-name` subject belonging to another identity (the GUI
//!   app user); polkit rejects such calls with "Only trusted callers can use
//!   CheckAuthorization() for subjects belonging to other identities". The
//!   peer's uid and bus_name are resolved by [`resolve_peer_identity`](crate::manager::bus::resolve_peer_identity)
//!   from the bus daemon's `GetConnectionCredentials`, which is the trusted
//!   source — not forgeable by the peer. The non-empty `bus_name` requirement
//!   ensures socket-based peers (PAM module, `bus_name == ""`) cannot gain
//!   authorization through this path. Cross-user enrollment, delete, configure,
//!   and configure-pam always go through polkit.
//!
//! - [`StubPolkit`] — deterministic stub for tests that exercise D-Bus
//!   mechanics and profile persistence, not real authorization. Root is admin,
//!   self-enrollment is allowed for own uid, everything else is denied.
//
// allow: SIZE_OK — 97 pure LOC production + 20 tests in a single cohesive
// authorization module. Splitting SystemPolkit/StubPolkit into separate files
// would fragment the PolkitAuthority implementations and their shared test
// harness (MockPolkit, peer helpers).

use std::collections::HashMap;

use zbus::zvariant::Value;

use crate::dbus::{action, AuthzDecision, PeerIdentity, PolkitAuthority};

const POLKIT_BUS_NAME: &str = "org.freedesktop.PolicyKit1";
const POLKIT_OBJECT_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const POLKIT_INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";

/// `CheckAuthorizationFlags = 0` — no user interaction. The daemon checks
/// whether the subject is *currently* authorized; if not but authentication
/// would succeed, polkit returns `is_challenge = true` and the daemon maps that
/// to [`AuthzDecision::Challenge`] so the client can drive interactive auth.
const CHECK_FLAGS_NONE: u32 = 0;

/// Real polkit authority backed by `org.freedesktop.PolicyKit1.Authority` on
/// the system bus.
///
/// Construct with [`SystemPolkit::default`] in production (connects to the
/// system bus, fails closed if unavailable) or [`SystemPolkit::for_testing`]
/// in tests (simulates a missing authority).
pub struct SystemPolkit {
    conn: Option<zbus::blocking::Connection>,
}

impl SystemPolkit {
    /// Simulate a missing polkit authority (no system bus / no polkitd). Every
    /// check returns [`AuthzDecision::Denied`].
    pub fn for_testing() -> Self {
        Self { conn: None }
    }
}

impl Default for SystemPolkit {
    /// Connect to the system bus for polkit checks. If the bus is unavailable,
    /// every [`PolkitAuthority::check`] call returns [`AuthzDecision::Denied`].
    fn default() -> Self {
        let conn = zbus::blocking::Connection::system().ok();
        if conn.is_none() {
            tracing::warn!("system bus unavailable — polkit checks will fail closed");
        }
        Self { conn }
    }
}

impl std::fmt::Debug for SystemPolkit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemPolkit")
            .field("connected", &self.conn.is_some())
            .finish()
    }
}

impl PolkitAuthority for SystemPolkit {
    fn check(&self, action_id: &str, peer: &PeerIdentity, target_uid: u32) -> AuthzDecision {
        // Self-enrollment short-circuit: authorize from the trusted D-Bus
        // credentials resolved by resolve_peer_identity, not from polkit. The
        // daemon (user `namtor`) is not a trusted caller for CheckAuthorization
        // with a subject belonging to another identity. Requires a non-empty
        // bus_name so socket peers cannot gain authorization.
        if action_id == action::ENROLL_SELF && peer.uid == target_uid && !peer.bus_name.is_empty() {
            return AuthzDecision::Allowed;
        }

        let Some(conn) = &self.conn else {
            return AuthzDecision::Denied;
        };
        if peer.bus_name.is_empty() {
            return AuthzDecision::Denied;
        }

        let subject = build_subject(&peer.bus_name);
        let details: HashMap<String, String> = HashMap::new();
        let cancellation_id = String::new();

        let reply = conn.call_method(
            Some(POLKIT_BUS_NAME),
            POLKIT_OBJECT_PATH,
            Some(POLKIT_INTERFACE),
            "CheckAuthorization",
            &(
                subject,
                action_id,
                details,
                CHECK_FLAGS_NONE,
                cancellation_id,
            ),
        );

        match reply {
            Ok(msg) => {
                let (is_authorized, is_challenge, _details): (bool, bool, HashMap<String, String>) =
                    match msg.body().deserialize() {
                        Ok(v) => v,
                        Err(_) => return AuthzDecision::Denied,
                    };
                if is_authorized {
                    AuthzDecision::Allowed
                } else if is_challenge {
                    AuthzDecision::Challenge
                } else {
                    AuthzDecision::Denied
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, action = action_id, "polkit CheckAuthorization failed");
                AuthzDecision::Denied
            }
        }
    }
}

/// Build the polkit subject struct `(sa{sv})` for a system-bus-name subject.
/// Polkit resolves the uid/pid from the bus name via the bus daemon — the
/// daemon's `GetConnectionCredentials` is the trusted source, not the `uid`
/// field on [`PeerIdentity`].
fn build_subject(bus_name: &str) -> (String, HashMap<String, Value<'_>>) {
    let mut props = HashMap::new();
    props.insert("name".to_string(), Value::from(bus_name));
    ("system-bus-name".to_string(), props)
}

/// Stub polkit authority for tests: root (uid 0) is admin, self-enrollment is
/// allowed for own uid, everything else is denied. Does not contact polkitd.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubPolkit;

impl PolkitAuthority for StubPolkit {
    fn check(&self, action: &str, peer: &PeerIdentity, target_uid: u32) -> AuthzDecision {
        let admin = peer.uid == 0;
        match action {
            action::ENROLL_SELF if peer.uid == target_uid => AuthzDecision::Allowed,
            action::ENROLL_SELF if admin => AuthzDecision::Allowed,
            action::ENROLL_OTHER | action::DELETE | action::CONFIGURE | action::CONFIGURE_PAM
                if admin =>
            {
                AuthzDecision::Allowed
            }
            _ => AuthzDecision::Denied,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::{action, ApiError, ManagerService, ProfileApi};
    use crate::manager::store::{NullProfileStore, ProfileStore};
    use std::sync::Arc;

    fn peer(uid: u32) -> PeerIdentity {
        PeerIdentity {
            uid,
            pid: 1,
            cmdline: "test".into(),
            bus_name: format!(":1.{}", uid),
        }
    }

    fn peer_no_bus(uid: u32) -> PeerIdentity {
        PeerIdentity {
            uid,
            pid: 1,
            cmdline: "test".into(),
            bus_name: String::new(),
        }
    }

    // ---- StubPolkit behavior ----

    #[test]
    fn stub_self_enroll_allowed_for_own_uid() {
        let svc = ManagerService::new(StubPolkit);
        let sid = svc.start_enrollment(&peer(1000), 1000, "alice").unwrap();
        assert!(sid.starts_with("sess-"));
    }

    #[test]
    fn stub_other_enroll_denied_for_non_admin() {
        let svc = ManagerService::new(StubPolkit);
        let err = svc.start_enrollment(&peer(1000), 2000, "bob").unwrap_err();
        assert!(matches!(err, ApiError::Denied { .. }));
    }

    #[test]
    fn stub_root_allowed_for_enroll_other() {
        let svc = ManagerService::new(StubPolkit);
        let sid = svc.start_enrollment(&peer(0), 1000, "bob").unwrap();
        assert!(sid.starts_with("sess-"));
    }

    #[test]
    fn stub_delete_requires_admin() {
        let svc = ManagerService::new(StubPolkit);
        let err = svc.delete_profile(&peer(1000), 1000, "x").unwrap_err();
        assert!(matches!(err, ApiError::Denied { .. }));
    }

    #[test]
    fn stub_configure_requires_admin() {
        let svc = ManagerService::new(StubPolkit);
        let err = svc.rename_profile(&peer(1000), 1000, "x", "y").unwrap_err();
        assert!(matches!(err, ApiError::Denied { .. }));
    }

    // ---- SystemPolkit self-enrollment short-circuit ----
    //
    // Self-enrollment (ENROLL_SELF with peer.uid == target_uid and a trusted
    // D-Bus bus_name) is authorized directly from the credentials resolved by
    // resolve_peer_identity via GetConnectionCredentials. It does NOT call
    // polkit CheckAuthorization because the daemon (running as user `namtor`)
    // is not a trusted caller for subjects belonging to other identities
    // (the GUI app user's bus name). See the module docs above.

    #[test]
    fn system_self_enroll_allowed_for_own_uid_without_polkit() {
        // Given: missing polkit authority (for_testing → conn=None) and a
        // trusted D-Bus peer whose uid matches the target uid.
        // When: start_enrollment is called for self-enrollment.
        // Then: the session is allowed — the daemon authorizes from the
        // trusted GetConnectionCredentials, not from polkit.
        let svc = ManagerService::new(SystemPolkit::for_testing());
        let sid = svc
            .start_enrollment(&peer(1000), 1000, "alice")
            .expect("self-enroll with trusted D-Bus identity must succeed without polkit");
        assert!(sid.starts_with("sess-"), "got a valid session id: {sid}");
    }

    #[test]
    fn system_self_enroll_denied_for_mismatched_uid() {
        // Given: missing polkit and a peer whose uid differs from target.
        // When: start_enrollment is called (action resolves to ENROLL_OTHER
        // in the service, but we also test SystemPolkit::check directly).
        // Then: denied — cross-user enrollment requires polkit admin auth.
        let polkit = SystemPolkit::for_testing();
        let decision = polkit.check(action::ENROLL_SELF, &peer(1000), 2000);
        assert_eq!(decision, AuthzDecision::Denied);
    }

    #[test]
    fn system_self_enroll_denied_for_empty_bus_name() {
        // Given: a peer identity with an empty bus_name (socket peer, not a
        // D-Bus connection) whose uid matches the target.
        // When: SystemPolkit::check is called with ENROLL_SELF.
        // Then: denied — the self-enroll short-circuit requires a trusted
        // D-Bus bus_name; socket identities must not gain authorization.
        let polkit = SystemPolkit::for_testing();
        let decision = polkit.check(action::ENROLL_SELF, &peer_no_bus(1000), 1000);
        assert_eq!(decision, AuthzDecision::Denied);
    }

    #[test]
    fn system_enroll_other_not_short_circuited_by_self_rule() {
        // Given: missing polkit and a peer whose uid matches the target.
        // When: SystemPolkit::check is called with ENROLL_OTHER.
        // Then: denied — ENROLL_OTHER always goes through polkit, never
        // through the self-enroll short-circuit.
        let polkit = SystemPolkit::for_testing();
        let decision = polkit.check(action::ENROLL_OTHER, &peer(1000), 1000);
        assert_eq!(decision, AuthzDecision::Denied);
    }

    #[test]
    fn missing_authority_fails_closed_on_delete() {
        let svc = ManagerService::new(SystemPolkit::for_testing());
        let err = svc.delete_profile(&peer(0), 0, "x").unwrap_err();
        assert!(
            matches!(err, ApiError::Denied { .. }),
            "missing polkitd must deny even for root, got {err:?}"
        );
    }

    #[test]
    fn missing_authority_fails_closed_on_configure() {
        let svc = ManagerService::new(SystemPolkit::for_testing());
        let err = svc.rename_profile(&peer(0), 0, "x", "y").unwrap_err();
        assert!(
            matches!(err, ApiError::Denied { .. }),
            "missing polkitd must deny, got {err:?}"
        );
    }

    #[test]
    fn vanished_sender_denied() {
        let svc = ManagerService::new(SystemPolkit::for_testing());
        let err = svc
            .start_enrollment(&peer_no_bus(1000), 1000, "alice")
            .unwrap_err();
        assert!(
            matches!(err, ApiError::Denied { .. }),
            "vanished sender (empty bus_name) must deny, got {err:?}"
        );
    }

    #[test]
    fn vanished_sender_denied_even_for_root() {
        let svc = ManagerService::new(SystemPolkit::for_testing());
        let err = svc
            .start_enrollment(&peer_no_bus(0), 1000, "bob")
            .unwrap_err();
        assert!(
            matches!(err, ApiError::Denied { .. }),
            "vanished sender must deny even for root, got {err:?}"
        );
    }

    #[test]
    fn system_polkit_debug_shows_connected_flag() {
        let connected = SystemPolkit::for_testing();
        let dbg = format!("{:?}", connected);
        assert!(dbg.contains("connected"));
        assert!(dbg.contains("false"));
    }

    // ---- build_subject ----

    #[test]
    fn build_subject_uses_system_bus_name_kind() {
        let (kind, props) = build_subject(":1.42");
        assert_eq!(kind, "system-bus-name");
        let name = expect_str_value(props.get("name"));
        assert_eq!(name, ":1.42");
    }

    #[test]
    fn build_subject_preserves_arbitrary_bus_name() {
        let (kind, props) = build_subject(":42.7");
        assert_eq!(kind, "system-bus-name");
        let name = expect_str_value(props.get("name"));
        assert_eq!(name, ":42.7");
    }

    fn expect_str_value(v: Option<&Value<'_>>) -> String {
        match v {
            Some(Value::Str(s)) => s.as_str().to_string(),
            other => panic!("expected Value::Str, got {other:?}"),
        }
    }

    // ---- Challenge + spoof via a configurable mock ----

    /// Closure-backed polkit for testing the ManagerService → AuthzDecision →
    /// ApiError mapping without a real polkitd.
    struct MockPolkit<F: Fn(&str, &PeerIdentity, u32) -> AuthzDecision>(F);

    impl<F: Fn(&str, &PeerIdentity, u32) -> AuthzDecision> PolkitAuthority for MockPolkit<F> {
        fn check(&self, action: &str, peer: &PeerIdentity, target_uid: u32) -> AuthzDecision {
            (self.0)(action, peer, target_uid)
        }
    }

    #[test]
    fn other_enroll_challenged_when_polkit_says_challenge() {
        let polkit = MockPolkit(|action, peer, target_uid| {
            if action == action::ENROLL_SELF && peer.uid == target_uid {
                AuthzDecision::Allowed
            } else if action == action::ENROLL_OTHER && peer.uid != 0 {
                AuthzDecision::Challenge
            } else if peer.uid == 0 {
                AuthzDecision::Allowed
            } else {
                AuthzDecision::Denied
            }
        });
        let svc = ManagerService::new(polkit);
        let err = svc.start_enrollment(&peer(1000), 2000, "bob").unwrap_err();
        assert!(
            matches!(err, ApiError::Challenge(_)),
            "cross-user enroll by non-admin should be challenged, got {err:?}"
        );
    }

    #[test]
    fn spoof_rejected_when_bus_name_mismatches_uid() {
        // Simulate polkit checking the bus_name's real uid (via the bus
        // daemon) rather than trusting the PeerIdentity.uid field. A peer
        // claiming uid=0 but with a non-root bus_name is denied.
        let polkit = MockPolkit(|_action, peer, _target| {
            // Real polkit resolves uid from bus_name, not from our struct.
            // Here we simulate: ":1.0" is root, everything else is uid 1000.
            let real_uid: u32 = if peer.bus_name == ":1.0" { 0 } else { 1000 };
            if real_uid == 0 {
                AuthzDecision::Allowed
            } else {
                AuthzDecision::Denied
            }
        });
        let svc = ManagerService::new(polkit);

        // Peer claims uid=0 but has a non-root bus_name — polkit sees the
        // real uid from the bus name and denies.
        let spoofed = PeerIdentity {
            uid: 0,
            pid: 1,
            cmdline: "spoof".into(),
            bus_name: ":1.999".into(),
        };
        let err = svc.start_enrollment(&spoofed, 1000, "bob").unwrap_err();
        assert!(
            matches!(err, ApiError::Denied { .. }),
            "spoofed uid with non-root bus_name must be denied, got {err:?}"
        );
    }

    #[test]
    fn root_with_correct_bus_name_allowed() {
        let polkit = MockPolkit(|_action, peer, _target| {
            let real_uid: u32 = if peer.bus_name == ":1.0" { 0 } else { 1000 };
            if real_uid == 0 {
                AuthzDecision::Allowed
            } else {
                AuthzDecision::Denied
            }
        });
        let svc = ManagerService::new(polkit);
        let legitimate_root = PeerIdentity {
            uid: 0,
            pid: 1,
            cmdline: "root".into(),
            bus_name: ":1.0".into(),
        };
        let sid = svc.start_enrollment(&legitimate_root, 1000, "bob").unwrap();
        assert!(sid.starts_with("sess-"));
    }

    // ---- SystemPolkit with a real store (integration smoke) ----

    #[test]
    fn system_polkit_with_store_fails_closed_on_all_mutations() {
        let store: Arc<dyn ProfileStore> = Arc::new(NullProfileStore);
        let svc = ManagerService::new_with_store(SystemPolkit::for_testing(), store);

        // Cross-user enrollment (ENROLL_OTHER) goes through polkit and is
        // denied when polkit is missing. Self-enrollment is authorized from
        // trusted D-Bus credentials and tested separately above.
        assert!(matches!(
            svc.start_enrollment(&peer(1000), 2000, "a").unwrap_err(),
            ApiError::Denied { .. }
        ));
        assert!(matches!(
            svc.delete_profile(&peer(0), 0, "x").unwrap_err(),
            ApiError::Denied { .. }
        ));
        assert!(matches!(
            svc.rename_profile(&peer(0), 0, "x", "y").unwrap_err(),
            ApiError::Denied { .. }
        ));
    }

    #[test]
    fn list_profiles_allowed_without_polkit_check() {
        // ListProfiles is a safe read — it does not call authz() and must
        // work even with a fail-closed polkit (returns caller's own profiles).
        let store: Arc<dyn ProfileStore> = Arc::new(NullProfileStore);
        let svc = ManagerService::new_with_store(SystemPolkit::for_testing(), store);
        let list = svc.list_profiles(&peer(1000)).unwrap();
        assert!(list.is_empty());
    }
}
