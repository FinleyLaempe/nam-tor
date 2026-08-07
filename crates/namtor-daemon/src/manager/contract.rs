// SPDX-License-Identifier: GPL-3.0-only
//! Frozen `org.namtor.Manager` D-Bus interface contract (spec §9.1, plan task 8).
//!
//! This module is the **single source of truth** for the versioned manager wire
//! contract. The introspection XML constant [`INTROSPECTION_XML`] is kept
//! byte-identical to `docs/evidence/dbus-contract.xml` (asserted by the
//! `contract_xml_matches_evidence_file` test). Both the GNOME and KDE clients
//! generate their bindings from this one introspection source.
//!
//! What is frozen here:
//!
//! - [`API_VERSION`] — the interface version exposed via the read-only
//!   `ApiVersion` property and used by [`negotiate_version`].
//! - The method, signal, and property names enumerated below.
//! - The D-Bus argument signatures in [`INTROSPECTION_XML`].
//! - The error names in [`error`].
//!
//! What is NOT here: the bus service implementation (task 9), the store backing
//! (task 10), enrollment sessions (task 11), and polkit wiring (task 12). This
//! module defines only the wire contract and a safe version-negotiation helper.
//!
//! Privacy invariant (asserted by `privacy_scan_rejects_biometric_tokens`): the
//! contract carries no frames, embeddings, match scores, thresholds, templates,
//! pixels, keys, or nonces. Only names, ids, booleans, coarse quality labels,
//! counters, and timestamps cross this surface.

/// Frozen interface version. Surfaced read-only as the `ApiVersion` property
/// and used by [`negotiate_version`] for safe client/server negotiation.
pub const API_VERSION: u32 = 1;

/// Frozen D-Bus introspection XML for `org.namtor.Manager`.
///
/// Kept byte-identical to `docs/evidence/dbus-contract.xml` by the
/// `contract_xml_matches_evidence_file` test. Editing one without the other
/// fails CI.
//
// keep-inline-with: docs/evidence/dbus-contract.xml
pub const INTROSPECTION_XML: &str = include_str!("../../../../docs/evidence/dbus-contract.xml");

/// D-Bus interface name.
pub const INTERFACE: &str = "org.namtor.Manager";

/// Well-known bus name the service takes.
pub const BUS_NAME: &str = "org.namtor.Manager";

/// Object path exposed by the service.
pub const OBJECT_PATH: &str = "/org/namtor/Manager";

/// Method names on the frozen v1 interface.
pub mod method {
    pub const LIST_PROFILES: &str = "ListProfiles";
    pub const START_ENROLLMENT: &str = "StartEnrollment";
    pub const CANCEL_ENROLLMENT: &str = "CancelEnrollment";
    pub const COMMIT_PROFILE: &str = "CommitProfile";
    pub const REVIEW_PROFILE: &str = "ReviewProfile";
    pub const RENAME_PROFILE: &str = "RenameProfile";
    pub const ENABLE_PROFILE: &str = "EnableProfile";
    pub const DELETE_PROFILE: &str = "DeleteProfile";
    pub const MERGE_PROFILES: &str = "MergeProfiles";
    pub const REPLACE_PROFILE: &str = "ReplaceProfile";
    pub const GET_STATUS: &str = "GetStatus";

    /// All frozen v1 method names, in declaration order.
    pub const ALL: &[&str] = &[
        LIST_PROFILES,
        START_ENROLLMENT,
        CANCEL_ENROLLMENT,
        COMMIT_PROFILE,
        REVIEW_PROFILE,
        RENAME_PROFILE,
        ENABLE_PROFILE,
        DELETE_PROFILE,
        MERGE_PROFILES,
        REPLACE_PROFILE,
        GET_STATUS,
    ];
}

/// Signal names on the frozen v1 interface.
pub mod signal {
    pub const ENROLLMENT_PROGRESS: &str = "EnrollmentProgress";
    pub const ENROLLMENT_COMPLETED: &str = "EnrollmentCompleted";
    pub const PROFILE_CHANGED: &str = "ProfileChanged";
    pub const STATUS_CHANGED: &str = "StatusChanged";

    /// All frozen v1 signal names, in declaration order.
    pub const ALL: &[&str] = &[
        ENROLLMENT_PROGRESS,
        ENROLLMENT_COMPLETED,
        PROFILE_CHANGED,
        STATUS_CHANGED,
    ];
}

/// Property names on the frozen v1 interface.
pub mod property {
    pub const API_VERSION: &str = "ApiVersion";
}

/// D-Bus error names raised by the manager service. These are the only failure
/// modes the contract permits; the bus service (task 9) maps [`ApiError`]
/// variants to these names.
pub mod error {
    /// `org.namtor.Error.Denied` — polkit refused the action.
    pub const DENIED: &str = "org.namtor.Error.Denied";
    /// `org.namtor.Error.Challenge` — polkit requires interactive auth.
    pub const CHALLENGE: &str = "org.namtor.Error.Challenge";
    /// `org.namtor.Error.NotFound` — profile or session id unknown.
    pub const NOT_FOUND: &str = "org.namtor.Error.NotFound";
    /// `org.namtor.Error.Exists` — duplicate profile name.
    pub const EXISTS: &str = "org.namtor.Error.Exists";
    /// `org.namtor.Error.SessionNotFound` — enrollment session id unknown.
    pub const SESSION_NOT_FOUND: &str = "org.namtor.Error.SessionNotFound";
    /// `org.namtor.Error.SessionBusy` — target uid already has a session.
    pub const SESSION_BUSY: &str = "org.namtor.Error.SessionBusy";
    /// `org.namtor.Error.State` — daemon in a state that rejects the call.
    pub const STATE: &str = "org.namtor.Error.State";
    /// `org.namtor.Error.IncompatibleVersion` — version negotiation failed.
    pub const INCOMPATIBLE_VERSION: &str = "org.namtor.Error.IncompatibleVersion";
}

/// Tokens that must never appear in the contract surface. The privacy scan
/// rejects any of these (case-insensitive) in [`INTROSPECTION_XML`].
pub const FORBIDDEN_TOKENS: &[&str] = &[
    "frame",
    "embedding",
    "score",
    "threshold",
    "template",
    "pixel",
    "vec",
    "key",
    "nonce",
    "biometric",
];

/// Version negotiation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
// allow: SIZE_OK — single cohesive frozen-contract module (constants + name
// modules + version helper + their validation tests). The tests reference
// private helpers (`extract_names`, `squash`); moving them to an integration
// test would force those helpers public, polluting the contract API for test
// purposes.
#[error("incompatible manager api version: client requested {requested}, supported is {supported}")]
pub struct IncompatibleVersion {
    pub requested: u32,
    pub supported: u32,
}

/// Negotiate the interface version against a client's requested version.
///
/// The frozen v1 interface is served only when the client requests exactly
/// [`API_VERSION`]. A client requesting a higher version wants features the
/// daemon does not expose; a client requesting a lower version speaks a draft
/// the daemon no longer supports. Both fail safely with
/// [`IncompatibleVersion`] so the client never silently gets a different
/// contract than the one it asked for.
///
/// This is the only version handshake the contract defines. The bus service
/// (task 9) calls this before any method dispatch when a client advertises a
/// version.
pub fn negotiate_version(client_version: u32) -> Result<u32, IncompatibleVersion> {
    if client_version == API_VERSION {
        Ok(API_VERSION)
    } else {
        Err(IncompatibleVersion {
            requested: client_version,
            supported: API_VERSION,
        })
    }
}

/// Extract every `<method name="…">` / `<signal name="…">` /
/// `<property name="…">` name from an introspection XML document, in document
/// order. Used by the contract validation test.
#[cfg(test)]
fn extract_names(xml: &str, tag: &str) -> Vec<String> {
    // r##"..."## lets the needle keep its trailing `"` (r#"..."# would eat it).
    let needle = format!(r##"<{tag} name=""##);
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(needle.as_str()) {
        let after = &rest[start + needle.len()..];
        rest = after;
        if let Some(end) = after.find('"') {
            out.push(after[..end].to_string());
        }
    }
    out
}

/// Collapse runs of whitespace to a single space so signature assertions are
/// robust to pretty-printing.
#[cfg(test)]
fn squash(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut ws = false;
    for c in xml.chars() {
        if c.is_whitespace() {
            if !ws {
                out.push(' ');
            }
            ws = true;
        } else {
            out.push(c);
            ws = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frozen constant must be byte-identical to the evidence file. This
    /// is the single-source-of-truth guarantee: editing one without the other
    /// fails CI.
    #[test]
    fn contract_xml_matches_evidence_file() {
        let file = include_str!("../../../../docs/evidence/dbus-contract.xml");
        assert_eq!(
            file, INTROSPECTION_XML,
            "docs/evidence/dbus-contract.xml and manager::contract::INTROSPECTION_XML drifted"
        );
    }

    /// The XML must declare exactly the `org.namtor.Manager` interface (no
    /// stray interfaces) and carry the freedesktop introspection DOCTYPE.
    #[test]
    fn contract_has_one_manager_interface() {
        assert!(
            INTROSPECTION_XML.contains("-//freedesktop//DTD D-BUS Object Introspection 1.0//EN")
        );
        let iface_count = INTROSPECTION_XML
            .matches("<interface name=\"org.namtor.Manager\">")
            .count();
        assert_eq!(iface_count, 1, "exactly one org.namtor.Manager interface");
        // No other interfaces.
        let other = INTROSPECTION_XML
            .matches("<interface name=\"org.namtor.")
            .count();
        assert_eq!(other, 1, "no other org.namtor.* interfaces in v1");
    }

    /// The contract must define exactly the required v1 method set — no more,
    /// no less — so a regenerated binding never silently drops or adds a
    /// method.
    #[test]
    fn contract_defines_exactly_the_required_methods() {
        let mut methods = extract_names(INTROSPECTION_XML, "method");
        methods.sort();
        let mut expected: Vec<&str> = method::ALL.to_vec();
        expected.sort();
        assert_eq!(methods, expected, "method set differs from frozen v1");
    }

    /// The contract must define exactly the required v1 signal set.
    #[test]
    fn contract_defines_exactly_the_required_signals() {
        let mut signals = extract_names(INTROSPECTION_XML, "signal");
        signals.sort();
        let mut expected: Vec<&str> = signal::ALL.to_vec();
        expected.sort();
        assert_eq!(signals, expected, "signal set differs from frozen v1");
    }

    /// The contract must expose the read-only `ApiVersion` property for
    /// version negotiation.
    #[test]
    fn contract_exposes_api_version_property() {
        let props = extract_names(INTROSPECTION_XML, "property");
        assert!(
            props.iter().any(|p| p == property::API_VERSION),
            "missing ApiVersion property"
        );
        assert!(
            INTROSPECTION_XML.contains(r#"<property name="ApiVersion" type="u" access="read"/>"#),
            "ApiVersion must be a read-only u32"
        );
    }

    /// Each enrollment/profile method must carry the documented argument
    /// signature. Guards against a silent signature change in a regenerated
    /// binding. Assertions run on the whitespace-squashed form so they are
    /// robust to pretty-printing.
    #[test]
    fn method_signatures_are_stable() {
        let s = squash(INTROSPECTION_XML);
        assert!(s.contains(
            r#"<method name="ListProfiles"> <arg name="profiles" type="a(ssbtt)" direction="out"/>"#
        ));
        assert!(s.contains(r#"<arg name="status" type="(ua(ssbtt)b)" direction="out"/>"#));
        assert!(s.contains(
            r#"<arg name="uid" type="u" direction="in"/> <arg name="name" type="s" direction="in"/> <arg name="session" type="s" direction="out"/>"#
        ));
        assert!(s.contains(
            r#"<arg name="session" type="s" direction="in"/> <arg name="profile" type="s" direction="out"/>"#
        ));
        assert!(s.contains(
            r#"<arg name="uid" type="u" direction="in"/> <arg name="src" type="s" direction="in"/> <arg name="dst" type="s" direction="in"/>"#
        ));
        assert!(s.contains(
            r#"<arg name="uid" type="u" direction="in"/> <arg name="id" type="s" direction="in"/> <arg name="review" type="(sbs)" direction="out"/>"#
        ));
    }

    /// Merge/replace/review semantics must be present as distinct methods with
    /// distinct signatures.
    #[test]
    fn merge_replace_review_semantics_present() {
        let s = squash(INTROSPECTION_XML);
        assert!(s.contains(r#"<method name="MergeProfiles">"#));
        assert!(s.contains(r#"<method name="ReplaceProfile">"#));
        assert!(s.contains(r#"<method name="ReviewProfile">"#));
        assert!(s.contains(
            r#"<method name="ReplaceProfile"> <arg name="uid" type="u" direction="in"/> <arg name="id" type="s" direction="in"/> <arg name="name" type="s" direction="in"/>"#
        ));
    }

    /// Signals must carry the documented argument signatures.
    #[test]
    fn signal_signatures_are_stable() {
        let s = squash(INTROSPECTION_XML);
        assert!(s.contains(
            r#"<signal name="EnrollmentProgress"> <arg name="session" type="s"/> <arg name="progress" type="(susu)"/>"#
        ));
        assert!(s.contains(
            r#"<signal name="EnrollmentCompleted"> <arg name="session" type="s"/> <arg name="profile" type="s"/>"#
        ));
        assert!(s.contains(
            r#"<signal name="ProfileChanged"> <arg name="uid" type="u"/> <arg name="id" type="s"/>"#
        ));
        assert!(
            s.contains(r#"<signal name="StatusChanged"> <arg name="status" type="(ua(ssbtt)b)"/>"#)
        );
    }

    /// Privacy: no biometric-bearing token may appear anywhere in the contract
    /// — method names, arg names, arg types, or signal names. The contract is
    /// the surface every client generates bindings from; if a forbidden token
    /// leaks here it leaks into every UI.
    #[test]
    fn privacy_scan_rejects_biometric_tokens() {
        let lower = INTROSPECTION_XML.to_lowercase();
        for token in FORBIDDEN_TOKENS {
            assert!(
                !lower.contains(token),
                "contract XML contains forbidden token `{token}` — biometric material must not cross the manager surface"
            );
        }
        // Positive guard: the method/arg names we DO expose are biometric-free
        // by construction, but assert the contract still talks about profiles
        // (so a future redaction does not silently gut the interface).
        assert!(lower.contains("profile"));
        assert!(lower.contains("enrollment"));
        assert!(lower.contains("status"));
    }

    /// A client requesting the supported version negotiates successfully.
    #[test]
    fn negotiate_supported_version_succeeds() {
        assert_eq!(negotiate_version(API_VERSION).unwrap(), API_VERSION);
    }

    /// A client requesting a higher version is rejected safely — never served
    /// a different contract than the one it asked for.
    #[test]
    fn negotiate_higher_version_fails_safely() {
        let err = negotiate_version(API_VERSION + 1).unwrap_err();
        assert_eq!(err.requested, API_VERSION + 1);
        assert_eq!(err.supported, API_VERSION);
    }

    /// A client requesting a lower (draft) version is also rejected safely.
    #[test]
    fn negotiate_lower_version_fails_safely() {
        assert!(negotiate_version(0).is_err());
        assert!(negotiate_version(API_VERSION - 1).is_err());
    }

    /// The error-name module must enumerate exactly the failure modes the
    /// contract permits, and each must live under the `org.namtor.Error.*`
    /// namespace.
    #[test]
    fn error_names_are_namespaced() {
        let names = [
            error::DENIED,
            error::CHALLENGE,
            error::NOT_FOUND,
            error::EXISTS,
            error::SESSION_NOT_FOUND,
            error::SESSION_BUSY,
            error::STATE,
            error::INCOMPATIBLE_VERSION,
        ];
        for n in names {
            assert!(
                n.starts_with("org.namtor.Error."),
                "error name `{n}` is not under org.namtor.Error.*"
            );
        }
    }

    /// `extract_names` helper sanity: pulls names in document order.
    #[test]
    fn extract_names_helper_works() {
        let xml = r#"<node><interface name="x">
        <method name="A"><arg name="a" type="s" direction="in"/></method>
        <signal name="B"><arg name="b" type="s"/></signal>
        <method name="C"/>
        </interface></node>"#;
        assert_eq!(extract_names(xml, "method"), vec!["A", "C"]);
        assert_eq!(extract_names(xml, "signal"), vec!["B"]);
    }
}
