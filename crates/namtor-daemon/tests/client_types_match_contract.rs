// SPDX-License-Identifier: GPL-3.0-only
//! The client's wire types must have the signatures the contract declares.
//!
//! The daemon's own contract tests pin the introspection XML; the client's
//! `Type` derives were pinned against nothing, and the two crates never meet
//! at compile time. A field added to `StatusReport`, or a `u32` widened to
//! `u64`, changes the signature the client deserialises into, and every
//! affected call then fails at runtime with a deserialise error the CLI
//! reports as "daemon unreachable" — a plausible-looking message that sends
//! the reader after a stopped daemon instead of a broken type.
//!
//! The suite stays green through all of that, because every test substitutes
//! a hand-written source or a trait mock for the wire.

use namtor_client::types::{
    EnrollmentProgressV3, EnrollmentReviewV3, PamServiceInfo, ProfileInfo, ProtectionStatus,
    ReviewOutcome, StatusReport,
};
use namtor_daemon::manager::contract::{
    INTROSPECTION_XML, INTROSPECTION_XML_V2, INTROSPECTION_XML_V3,
};
use zbus::zvariant::Type;

/// The declared type of one argument of one method, as the XML gives it.
fn arg_type(xml: &str, method: &str, arg: &str) -> String {
    let squashed: String = xml.split_whitespace().collect::<Vec<_>>().join(" ");
    let start = squashed
        .find(&format!(r#"<method name="{method}">"#))
        .unwrap_or_else(|| panic!("no <method name=\"{method}\"> in the contract"));
    let rest = &squashed[start..];
    let end = rest.find("</method>").unwrap_or(rest.len());
    let body = &rest[..end];
    let needle = format!(r#"<arg name="{arg}" type=""#);
    let at = body
        .find(&needle)
        .unwrap_or_else(|| panic!("no arg {arg} on {method}: {body}"));
    let after = &body[at + needle.len()..];
    let close = after.find('"').expect("unterminated type attribute");
    after[..close].to_string()
}

fn assert_sig<T: Type>(xml: &str, method: &str, arg: &str) {
    let declared = arg_type(xml, method, arg);
    let actual = T::SIGNATURE.to_string();
    assert_eq!(
        actual, declared,
        "{method}.{arg}: the contract declares {declared}, the client type \
         deserialises {actual}. One of the two has moved."
    );
}

/// As [`assert_sig`], for a method whose reply is several arguments: the
/// Rust tuple's outer parentheses are the argument list, not a struct.
fn assert_body_sig<T: Type>(xml: &str, method: &str, arg: &str) {
    let declared = arg_type(xml, method, arg);
    let sig = T::SIGNATURE.to_string();
    let inner = sig
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or_else(|| panic!("{method}.{arg}: expected a tuple signature, got {sig}"));
    assert_eq!(
        inner, declared,
        "{method}.{arg}: the contract declares {declared}, the client body \
         deserialises {inner}. One of the two has moved."
    );
}

#[test]
fn the_status_report_matches_get_status() {
    assert_sig::<StatusReport>(INTROSPECTION_XML, "GetStatus", "status");
}

#[test]
fn the_profile_list_matches_list_profiles() {
    assert_sig::<Vec<ProfileInfo>>(INTROSPECTION_XML, "ListProfiles", "profiles");
}

#[test]
fn the_review_outcome_matches_review_profile() {
    assert_sig::<ReviewOutcome>(INTROSPECTION_XML, "ReviewProfile", "review");
}

#[test]
fn the_pam_service_list_matches_list_pam_services() {
    assert_sig::<Vec<PamServiceInfo>>(INTROSPECTION_XML_V2, "ListPamServices", "services");
}

#[test]
fn the_protection_status_matches_get_protection_status() {
    assert_sig::<ProtectionStatus>(INTROSPECTION_XML_V2, "GetProtectionStatus", "status");
}

/// `PollProgress` returns two values, and zbus puts them in the message body
/// as two arguments rather than one struct — which is why the contract writes
/// both complete types into a single `type=` attribute. The Rust side
/// deserialises the body into a tuple, whose own signature wraps them in
/// parentheses, so the comparison is against the tuple's contents.
#[test]
fn the_v3_progress_matches_poll_progress() {
    assert_body_sig::<(bool, EnrollmentProgressV3)>(INTROSPECTION_XML_V3, "PollProgress", "reply");
}

#[test]
fn the_v3_review_matches_review_enrollment() {
    assert_sig::<EnrollmentReviewV3>(INTROSPECTION_XML_V3, "ReviewEnrollment", "review");
}

/// The extractor must actually be reading the XML, or every assertion above
/// compares two strings it invented.
#[test]
fn the_extractor_reads_the_contract() {
    assert_eq!(
        arg_type(INTROSPECTION_XML, "GetStatus", "status"),
        "(ua(ssbtt)b)"
    );
    assert_eq!(
        arg_type(INTROSPECTION_XML, "ListProfiles", "profiles"),
        "a(ssbtt)"
    );
    assert_eq!(
        arg_type(INTROSPECTION_XML_V3, "PollProgress", "reply"),
        "b(ssuuuuuys)"
    );
}
