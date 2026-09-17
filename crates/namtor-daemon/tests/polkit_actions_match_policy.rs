// SPDX-License-Identifier: GPL-3.0-only
//! The polkit action ids the daemon asks about must be the ones the package
//! installs.
//!
//! `SystemPolkit::check` passes an action id to `CheckAuthorization`. polkit
//! does not distinguish "this action is declared and you are not allowed" from
//! "there is no such action" — both come back unauthorised, and the daemon
//! fails closed. So a typo in either the Rust constant or the policy file
//! turns a privileged operation into a permanent "Authentication denied" with
//! no prompt, and every test still passes: the polkit tests all use
//! `SystemPolkit::for_testing()`, which never reaches the bus.
//!
//! This pins both directions.

use std::path::PathBuf;

use namtor_daemon::dbus::action;

/// Actions the policy declares that no Rust constant names, each with the
/// reason it is still there. An entry here is a claim that the action is
/// deliberately unused by this codebase — not that it is unused by anything.
const DECLARED_BUT_UNUSED: &[(&str, &str)] = &[(
    "org.namtor.restart-daemon",
    "declared so a desktop tool can restart the daemon under polkit; the \
     Settings app shows the systemctl command instead of invoking it, so no \
     Rust constant names it",
)];

fn policy() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/polkit-1/actions/org.namtor.policy");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn declared_ids(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find(r#"<action id=""#) {
        let after = &rest[at + r#"<action id=""#.len()..];
        let Some(close) = after.find('"') else { break };
        out.push(after[..close].to_string());
        rest = &after[close..];
    }
    out
}

/// Every id the daemon can hand to polkit.
const USED: &[&str] = &[
    action::ENROLL_SELF,
    action::ENROLL_OTHER,
    action::DELETE,
    action::CONFIGURE,
    action::CONFIGURE_PAM,
];

#[test]
fn every_action_the_daemon_asks_about_is_declared() {
    let declared = declared_ids(&policy());
    assert!(
        !declared.is_empty(),
        "parsed no actions out of the policy file"
    );
    for id in USED {
        assert!(
            declared.iter().any(|d| d == id),
            "the daemon authorises against {id}, which the installed policy \
             does not declare — polkit answers 'not authorised' and the user \
             is denied with no prompt. Declared: {declared:?}"
        );
    }
}

#[test]
fn every_declared_action_is_used_or_accounted_for() {
    for id in declared_ids(&policy()) {
        let used = USED.contains(&id.as_str());
        let excused = DECLARED_BUT_UNUSED.iter().any(|(a, _)| *a == id);
        assert!(
            used || excused,
            "the policy declares {id}, which no Rust constant names. Either \
             wire it up or add it to DECLARED_BUT_UNUSED with the reason."
        );
    }
}

/// An excuse for an action that has since been wired up is stale, and stale
/// excuses are how the check above stops meaning anything.
#[test]
fn no_stale_excuses() {
    for (id, _why) in DECLARED_BUT_UNUSED {
        assert!(
            !USED.contains(id),
            "{id} is excused as unused but a Rust constant names it"
        );
        assert!(
            declared_ids(&policy()).iter().any(|d| d == id),
            "{id} is excused but the policy no longer declares it"
        );
    }
}

/// The ids are spelled in the reverse-DNS form polkit expects; a bare name
/// would be accepted by the parser above and rejected by polkit.
#[test]
fn the_ids_are_namespaced() {
    for id in USED {
        assert!(id.starts_with("org.namtor."), "{id}");
        assert!(!id.ends_with('.'), "{id}");
    }
}
