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
//!
//! # v2 interface (`org.namtor.Manager2`)
//!
//! The v2 interface extends the v1 surface with PAM service management and
//! template protection status. The v1 interface (`org.namtor.Manager`) is
//! preserved byte-for-byte inside [`INTROSPECTION_XML_V2`]; v1 clients continue
//! to negotiate [`API_VERSION`] and speak the frozen v1 interface unchanged.
//! v2 clients negotiate [`API_VERSION_V2`] and speak `org.namtor.Manager2`.
//!
//! v2 additions (plan task 1):
//! - Methods: `ListPamServices`, `SetPamServiceEnabled`, `GetProtectionStatus`
//! - Signals: `PamServicesChanged`, `ProtectionStatusChanged`
//! - Errors: `Unsupported`, `Rollback`, `Conflict`
//! - Typed structs: [`PamServiceInfo`], [`ProtectionStatus`]

use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

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
/// rejects any of these (case-insensitive) in [`INTROSPECTION_XML`] and
/// [`INTROSPECTION_XML_V2`].
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

// ---------------------------------------------------------------------------
// v2 interface — org.namtor.Manager2
// ---------------------------------------------------------------------------

/// v2 interface version. Surfaced read-only as the `ApiVersion` property on
/// `org.namtor.Manager2` and used by [`negotiate_version_v2`].
pub const API_VERSION_V2: u32 = 2;

/// v2 D-Bus introspection XML. Contains the v1 interface (preserved
/// byte-for-byte) plus the `org.namtor.Manager2` interface. Kept
/// byte-identical to `docs/evidence/dbus-contract-v2.xml` by the
/// `contract_v2_xml_matches_evidence_file` test.
//
// keep-inline-with: docs/evidence/dbus-contract-v2.xml
pub const INTROSPECTION_XML_V2: &str =
    include_str!("../../../../docs/evidence/dbus-contract-v2.xml");

/// v2 D-Bus interface name.
pub const INTERFACE_V2: &str = "org.namtor.Manager2";

/// Method names on the v2 interface.
pub mod method_v2 {
    pub const LIST_PAM_SERVICES: &str = "ListPamServices";
    pub const SET_PAM_SERVICE_ENABLED: &str = "SetPamServiceEnabled";
    pub const GET_PROTECTION_STATUS: &str = "GetProtectionStatus";

    /// All v2 method names, in declaration order.
    pub const ALL: &[&str] = &[
        LIST_PAM_SERVICES,
        SET_PAM_SERVICE_ENABLED,
        GET_PROTECTION_STATUS,
    ];
}

/// Signal names on the v2 interface.
pub mod signal_v2 {
    pub const PAM_SERVICES_CHANGED: &str = "PamServicesChanged";
    pub const PROTECTION_STATUS_CHANGED: &str = "ProtectionStatusChanged";

    /// All v2 signal names, in declaration order.
    pub const ALL: &[&str] = &[PAM_SERVICES_CHANGED, PROTECTION_STATUS_CHANGED];
}

/// Property names on the v2 interface.
pub mod property_v2 {
    pub const API_VERSION: &str = "ApiVersion";
}

/// v2 D-Bus error names raised by the manager service for PAM and protection
/// operations. These extend the v1 [`error`] module; v1 errors remain
/// unchanged.
pub mod error_v2 {
    /// `org.namtor.Error.Unsupported` — PAM service is unsupported or risky.
    pub const UNSUPPORTED: &str = "org.namtor.Error.Unsupported";
    /// `org.namtor.Error.Rollback` — PAM edit failed and rolled back.
    pub const ROLLBACK: &str = "org.namtor.Error.Rollback";
    /// `org.namtor.Error.Conflict` — concurrent PAM edit conflict.
    pub const CONFLICT: &str = "org.namtor.Error.Conflict";
}

/// v2: PAM service compatibility classification (plan task 9). Serialized as
/// a lowercase string on the wire (`"safe"`, `"unsupported"`, `"risky"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zvariant(signature = "s", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PamServiceCompatibility {
    /// Safe to enable — has password fallback, not deny-first.
    Safe,
    /// Unsupported — risky stack shape (deny-first, no fallback, malformed).
    Unsupported,
    /// Risky — can be enabled but high-impact (login, sudo, su, polkit).
    Risky,
}

/// v2: Sealing mechanism backing template storage (plan task 8). Serialized as
/// a lowercase string on the wire (`"tpm2"`, `"software"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zvariant(signature = "s", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ProtectionKeyKind {
    /// TPM2 2.0 sealed (hardware-bound).
    Tpm2,
    /// Software AES-256-GCM (file fallback, 0600, root/namtor owned).
    Software,
}

/// v2: Descriptor of a detected PAM auth service (wire `(ssbss)`). Returned by
/// `ListPamServices`. Carries no biometric material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PamServiceInfo {
    /// Stable service id (e.g. `"gdm-password"`, `"sddm"`).
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Whether namtor is currently enabled for this service.
    pub enabled: bool,
    /// Compatibility classification.
    pub compatibility: PamServiceCompatibility,
    /// Human-readable description or label.
    pub description: String,
}

/// v2: Template storage protection status (wire `(sbs)`). Returned by
/// `GetProtectionStatus`. Carries no sealing material — only the sealing
/// *kind*, TPM presence, and a human-readable warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProtectionStatus {
    /// Active sealing mechanism.
    pub sealing: ProtectionKeyKind,
    /// Whether a TPM2 device is present on the system.
    pub has_tpm: bool,
    /// Human-readable warning (empty when protection is hardware-bound).
    pub warning: String,
}

/// Negotiate the v2 interface version against a client's requested version.
///
/// A v2-capable server serves both v1 and v2. A client requesting
/// [`API_VERSION_V2`] gets v2; a client requesting [`API_VERSION`] gets v1
/// (backward-compatible). Any other version fails safely with
/// [`IncompatibleVersion`] so the client never silently gets a different
/// contract than the one it asked for.
pub fn negotiate_version_v2(client_version: u32) -> Result<u32, IncompatibleVersion> {
    if client_version == API_VERSION_V2 {
        Ok(API_VERSION_V2)
    } else if client_version == API_VERSION {
        Ok(API_VERSION)
    } else {
        Err(IncompatibleVersion {
            requested: client_version,
            supported: API_VERSION_V2,
        })
    }
}

/// Version negotiation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
// allow: SIZE_OK — single cohesive frozen-contract module (v1 + v2 constants,
// name modules, typed structs, version helpers, and their validation tests).
// The tests reference private helpers (`extract_names`, `squash`,
// `extract_interface_block`); moving them to an integration test would force
// those helpers public, polluting the contract API for test purposes.
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

    // =======================================================================
    // v2 contract tests
    // =======================================================================

    /// Extract a single `<interface name="...">...</interface>` block from an
    /// introspection XML document.
    #[cfg(test)]
    fn extract_interface_block(xml: &str, iface: &str) -> String {
        let start_tag = format!(r#"<interface name="{iface}">"#);
        let start = xml
            .find(&start_tag)
            .unwrap_or_else(|| panic!("interface `{iface}` not found in XML"));
        let rest = &xml[start..];
        let end = rest
            .find("</interface>")
            .unwrap_or_else(|| panic!("closing tag for interface `{iface}` not found"));
        rest[..end + "</interface>".len()].to_string()
    }

    /// The v2 frozen constant must be byte-identical to the v2 evidence file.
    #[test]
    fn contract_v2_xml_matches_evidence_file() {
        let file = include_str!("../../../../docs/evidence/dbus-contract-v2.xml");
        assert_eq!(
            file, INTROSPECTION_XML_V2,
            "docs/evidence/dbus-contract-v2.xml and INTROSPECTION_XML_V2 drifted"
        );
    }

    /// The v2 XML must declare exactly two interfaces: the preserved v1
    /// `org.namtor.Manager` and the new `org.namtor.Manager2`.
    #[test]
    fn contract_v2_has_both_interfaces() {
        assert!(
            INTROSPECTION_XML_V2.contains("-//freedesktop//DTD D-BUS Object Introspection 1.0//EN")
        );
        let v1_count = INTROSPECTION_XML_V2
            .matches("<interface name=\"org.namtor.Manager\">")
            .count();
        assert_eq!(v1_count, 1, "exactly one v1 org.namtor.Manager interface");
        let v2_count = INTROSPECTION_XML_V2
            .matches("<interface name=\"org.namtor.Manager2\">")
            .count();
        assert_eq!(v2_count, 1, "exactly one v2 org.namtor.Manager2 interface");
        let total = INTROSPECTION_XML_V2
            .matches("<interface name=\"org.namtor.")
            .count();
        assert_eq!(total, 2, "exactly two org.namtor.* interfaces in v2 XML");
    }

    /// The v1 interface block must be preserved byte-for-byte inside the v2
    /// XML. Mutation test: any change to the v1 interface in the v2 file
    /// causes this test to fail.
    #[test]
    fn v1_interface_preserved_in_v2_xml() {
        let v1_block = extract_interface_block(INTROSPECTION_XML, INTERFACE);
        assert!(
            INTROSPECTION_XML_V2.contains(&v1_block),
            "v1 interface block not preserved byte-for-byte in v2 XML"
        );
    }

    /// The v2 XML must define exactly the required v2 method set — no more,
    /// no less — when scoped to the Manager2 interface.
    #[test]
    fn contract_v2_defines_exactly_the_required_methods() {
        let v2_block = extract_interface_block(INTROSPECTION_XML_V2, INTERFACE_V2);
        let mut methods = extract_names(&v2_block, "method");
        methods.sort();
        let mut expected: Vec<&str> = method_v2::ALL.to_vec();
        expected.sort();
        assert_eq!(methods, expected, "v2 method set differs from frozen v2");
    }

    /// The v2 XML must define exactly the required v2 signal set.
    #[test]
    fn contract_v2_defines_exactly_the_required_signals() {
        let v2_block = extract_interface_block(INTROSPECTION_XML_V2, INTERFACE_V2);
        let mut signals = extract_names(&v2_block, "signal");
        signals.sort();
        let mut expected: Vec<&str> = signal_v2::ALL.to_vec();
        expected.sort();
        assert_eq!(signals, expected, "v2 signal set differs from frozen v2");
    }

    /// The v2 interface must expose a read-only `ApiVersion` property.
    #[test]
    fn contract_v2_exposes_api_version_property() {
        let v2_block = extract_interface_block(INTROSPECTION_XML_V2, INTERFACE_V2);
        let props = extract_names(&v2_block, "property");
        assert!(
            props.iter().any(|p| p == property_v2::API_VERSION),
            "missing ApiVersion property on v2 interface"
        );
        assert!(
            v2_block.contains(r#"<property name="ApiVersion" type="u" access="read"/>"#),
            "v2 ApiVersion must be a read-only u32"
        );
    }

    /// v2 method signatures must be stable. Mutation test: changing any v2
    /// method's argument signature causes this test to fail.
    #[test]
    fn v2_method_signatures_are_stable() {
        let s = squash(INTROSPECTION_XML_V2);
        assert!(s.contains(
            r#"<method name="ListPamServices"> <arg name="services" type="a(ssbss)" direction="out"/>"#
        ));
        assert!(s.contains(
            r#"<method name="SetPamServiceEnabled"> <arg name="service_id" type="s" direction="in"/> <arg name="enabled" type="b" direction="in"/> <arg name="success" type="b" direction="out"/>"#
        ));
        assert!(s.contains(
            r#"<method name="GetProtectionStatus"> <arg name="status" type="(sbs)" direction="out"/>"#
        ));
    }

    /// v2 signal signatures must be stable. Mutation test: changing any v2
    /// signal's argument signature causes this test to fail.
    #[test]
    fn v2_signal_signatures_are_stable() {
        let s = squash(INTROSPECTION_XML_V2);
        assert!(s.contains(
            r#"<signal name="PamServicesChanged"> <arg name="services" type="a(ssbss)"/>"#
        ));
        assert!(s.contains(
            r#"<signal name="ProtectionStatusChanged"> <arg name="status" type="(sbs)"/>"#
        ));
    }

    /// Privacy: no biometric-bearing token may appear in the v2 XML either.
    #[test]
    fn privacy_scan_rejects_biometric_tokens_v2() {
        let lower = INTROSPECTION_XML_V2.to_lowercase();
        for token in FORBIDDEN_TOKENS {
            assert!(
                !lower.contains(token),
                "v2 contract XML contains forbidden token `{token}`"
            );
        }
    }

    /// v2 API version must be frozen at 2.
    #[test]
    fn v2_api_version_is_frozen() {
        assert_eq!(API_VERSION_V2, 2);
    }

    /// A v2 client requesting v2 negotiates successfully.
    #[test]
    fn negotiate_v2_supported_version_succeeds() {
        assert_eq!(
            negotiate_version_v2(API_VERSION_V2).unwrap(),
            API_VERSION_V2
        );
    }

    /// A v1 client negotiating against a v2 server gets v1 (backward-compat).
    #[test]
    fn negotiate_v2_v1_backward_compat() {
        assert_eq!(negotiate_version_v2(API_VERSION).unwrap(), API_VERSION);
    }

    /// A client requesting a higher version than v2 is rejected safely.
    #[test]
    fn negotiate_v2_higher_version_fails_safely() {
        let err = negotiate_version_v2(API_VERSION_V2 + 1).unwrap_err();
        assert_eq!(err.requested, API_VERSION_V2 + 1);
        assert_eq!(err.supported, API_VERSION_V2);
    }

    /// A client requesting v0 is rejected by the v2 negotiator.
    #[test]
    fn negotiate_v2_lower_version_fails_safely() {
        assert!(negotiate_version_v2(0).is_err());
    }

    /// v2 error names must live under `org.namtor.Error.*`.
    #[test]
    fn v2_error_names_are_namespaced() {
        let names = [
            error_v2::UNSUPPORTED,
            error_v2::ROLLBACK,
            error_v2::CONFLICT,
        ];
        for n in names {
            assert!(
                n.starts_with("org.namtor.Error."),
                "v2 error name `{n}` is not under org.namtor.Error.*"
            );
        }
    }

    /// `PamServiceCompatibility` must have exactly three variants and each
    /// serializes to the expected lowercase string. Exhaustive match test.
    #[test]
    fn pam_service_compatibility_exhaustive_variants() {
        let cases = [
            (PamServiceCompatibility::Safe, "safe"),
            (PamServiceCompatibility::Unsupported, "unsupported"),
            (PamServiceCompatibility::Risky, "risky"),
        ];
        for (val, expected) in cases {
            let json = serde_json::to_string(&val).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
            let back: PamServiceCompatibility = serde_json::from_str(&json).unwrap();
            assert_eq!(back, val);
        }
        let count = match PamServiceCompatibility::Safe {
            PamServiceCompatibility::Safe => 1,
            PamServiceCompatibility::Unsupported => 2,
            PamServiceCompatibility::Risky => 3,
        };
        assert!((1..=3).contains(&count));
    }

    /// `ProtectionKeyKind` must have exactly two variants and each serializes
    /// to the expected lowercase string. Exhaustive match test.
    #[test]
    fn protection_key_kind_exhaustive_variants() {
        let cases = [
            (ProtectionKeyKind::Tpm2, "tpm2"),
            (ProtectionKeyKind::Software, "software"),
        ];
        for (val, expected) in cases {
            let json = serde_json::to_string(&val).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
            let back: ProtectionKeyKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, val);
        }
        let count = match ProtectionKeyKind::Tpm2 {
            ProtectionKeyKind::Tpm2 => 1,
            ProtectionKeyKind::Software => 2,
        };
        assert!((1..=2).contains(&count));
    }

    /// `PamServiceInfo` round-trips through JSON serialization.
    #[test]
    fn pam_service_info_round_trips() {
        let info = PamServiceInfo {
            id: "gdm-password".into(),
            display_name: "GDM Password".into(),
            enabled: false,
            compatibility: PamServiceCompatibility::Safe,
            description: "GNOME Display Manager".into(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: PamServiceInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, info);
    }

    /// `ProtectionStatus` round-trips through JSON serialization.
    #[test]
    fn protection_status_round_trips() {
        let status = ProtectionStatus {
            sealing: ProtectionKeyKind::Software,
            has_tpm: false,
            warning: "TPM2 not available; using software fallback".into(),
        };
        let json = serde_json::to_string(&status).unwrap();
        let back: ProtectionStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, status);
    }

    /// The D-Bus Type signatures must match the introspection XML. Mutation
    /// test: changing a struct field type causes the signature assertion to
    /// fail.
    #[test]
    fn v2_types_have_correct_dbus_signatures() {
        assert_eq!(PamServiceInfo::SIGNATURE, "(ssbss)");
        assert_eq!(ProtectionStatus::SIGNATURE, "(sbs)");
        assert_eq!(PamServiceCompatibility::SIGNATURE, "s");
        assert_eq!(ProtectionKeyKind::SIGNATURE, "s");
    }

    /// v2 types must not leak biometric tokens when serialized.
    #[test]
    fn v2_types_no_biometric_leak() {
        let info = PamServiceInfo {
            id: "sddm".into(),
            display_name: "SDDM".into(),
            enabled: true,
            compatibility: PamServiceCompatibility::Risky,
            description: "Simple Desktop Display Manager".into(),
        };
        let status = ProtectionStatus {
            sealing: ProtectionKeyKind::Tpm2,
            has_tpm: true,
            warning: String::new(),
        };
        let info_json = serde_json::to_string(&info).unwrap().to_lowercase();
        let status_json = serde_json::to_string(&status).unwrap().to_lowercase();
        for token in FORBIDDEN_TOKENS {
            assert!(
                !info_json.contains(token),
                "PamServiceInfo leaked `{token}`"
            );
            assert!(
                !status_json.contains(token),
                "ProtectionStatus leaked `{token}`"
            );
        }
    }

    /// Mutation test: changing the v2 method set (adding or removing a method)
    /// causes the exact-set test to fail. This is verified by asserting the
    /// method count matches `method_v2::ALL` length.
    #[test]
    fn v2_method_set_mutation_guard() {
        let v2_block = extract_interface_block(INTROSPECTION_XML_V2, INTERFACE_V2);
        let methods = extract_names(&v2_block, "method");
        assert_eq!(
            methods.len(),
            method_v2::ALL.len(),
            "v2 method count drifted from frozen set"
        );
    }

    /// Mutation test: changing the v2 signal set causes the exact-set test to
    /// fail.
    #[test]
    fn v2_signal_set_mutation_guard() {
        let v2_block = extract_interface_block(INTROSPECTION_XML_V2, INTERFACE_V2);
        let signals = extract_names(&v2_block, "signal");
        assert_eq!(
            signals.len(),
            signal_v2::ALL.len(),
            "v2 signal count drifted from frozen set"
        );
    }
}
