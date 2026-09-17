// SPDX-License-Identifier: GPL-3.0-only
//! Every D-Bus method the client sends must exist in the frozen contract.
//!
//! `DbusManagerClient` names its methods as string literals; the daemon
//! declares them in `INTROSPECTION_XML*`. Nothing tied the two together, and
//! the fake the whole suite runs against implements the trait, not the wire —
//! so a client method could name something no interface exports and every
//! test would still pass. One did: `negotiate_v3` called
//! `org.namtor.Manager3.NegotiateVersion`, a method that has never existed on
//! any of the three interfaces, and only a real daemon would have said so.
//!
//! Reading the client's source is the point: a constant shared between the
//! two crates would only prove they agree with each other, not that the call
//! sites use it.

use std::path::PathBuf;

use namtor_daemon::manager::contract::{
    INTROSPECTION_XML, INTROSPECTION_XML_V2, INTROSPECTION_XML_V3,
};

fn client_source() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../namtor-client/src/dbus_client.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// One `self.<helper>(…)` call site in the client's source.
struct CallSite {
    helper: String,
    /// The method name, when it is a string literal at the call site.
    /// `None` for an internal forwarder that takes the name as a parameter.
    method: Option<String>,
}

/// The four call helpers, longest first so `call_v2` is not read as `call`.
const HELPERS: &[&str] = &["call_unit", "call_v2", "call_v3", "call"];

/// Every `self.<helper>` call site in the client's source.
///
/// Parses rather than line-matches, because the real source has all of:
/// `self.call::<Vec<ProfileInfo>, _>("ListProfiles", &())`, a turbofish whose
/// own arguments contain parentheses (`::<(bool, EnrollmentProgressV3), _>`),
/// a method name `rustfmt` has wrapped onto the following line, a helper with
/// no turbofish at all (`call_unit`), and one internal forwarder that takes
/// the name as a parameter. A line-scoped or prefix-matched extractor gets
/// some of those wrong and silently proves less than it looks like it does.
fn call_sites(src: &str) -> Vec<CallSite> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(at) = src[i..].find("self.") {
        let pos = i + at + "self.".len();
        i = pos;
        let Some(helper) = HELPERS.iter().find(|h| src[pos..].starts_with(*h)) else {
            continue;
        };
        let mut j = pos + helper.len();
        // Skip a turbofish, matching angle brackets so nested generics and
        // any parentheses inside them are stepped over.
        if src[j..].starts_with("::<") {
            j += 3;
            let mut depth = 1usize;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    b'<' => depth += 1,
                    b'>' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
        }
        // The argument list.
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'(' {
            continue;
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let method = if j < bytes.len() && bytes[j] == b'"' {
            let after = &src[j + 1..];
            after.find('"').map(|close| after[..close].to_string())
        } else {
            None
        };
        out.push(CallSite {
            helper: (*helper).to_string(),
            method,
        });
        i = j;
    }
    out
}

/// The method names sent on one interface. `call_unit` is a thin wrapper over
/// `call`, so its names belong to v1 too.
fn methods_called(src: &str, helper: &str) -> Vec<String> {
    let aliases: &[&str] = if helper == "call" {
        &["call", "call_unit"]
    } else {
        std::slice::from_ref(&helper)
    };
    call_sites(src)
        .into_iter()
        .filter(|c| aliases.contains(&c.helper.as_str()))
        .filter_map(|c| c.method)
        .collect()
}

fn declares(xml: &str, method: &str) -> bool {
    xml.contains(&format!(r#"<method name="{method}""#))
}

fn check(helper: &str, xml: &str, interface: &str) {
    let src = client_source();
    let called = methods_called(&src, helper);
    assert!(
        !called.is_empty(),
        "found no {helper} call sites — the extractor has drifted from the source"
    );
    for m in &called {
        assert!(
            declares(xml, m),
            "the client calls {interface}.{m}, which the frozen contract does \
             not declare. Either the interface is missing the method or the \
             client is calling something that has never existed."
        );
    }
}

#[test]
fn every_v1_method_the_client_calls_exists() {
    check("call", INTROSPECTION_XML, "org.namtor.Manager");
}

#[test]
fn every_v2_method_the_client_calls_exists() {
    check("call_v2", INTROSPECTION_XML_V2, "org.namtor.Manager2");
}

#[test]
fn every_v3_method_the_client_calls_exists() {
    check("call_v3", INTROSPECTION_XML_V3, "org.namtor.Manager3");
}

/// The extractor has to actually find the names, or the three tests above
/// pass by looking at nothing. This pins a method that is definitely there.
#[test]
fn the_extractor_finds_real_call_sites() {
    let src = client_source();
    let v3 = methods_called(&src, "call_v3");
    assert!(
        v3.iter().any(|m| m == "StartEnrollment"),
        "expected StartEnrollment among the v3 calls, got {v3:?}"
    );
    // And the helpers must not bleed into one another: `self.call` is a
    // prefix of `self.call_v2`.
    let v1 = methods_called(&src, "call");
    assert!(
        !v1.iter().any(|m| m == "ListPamServices"),
        "a v2 method was attributed to v1: {v1:?}"
    );
    assert!(
        v1.iter().any(|m| m == "ListProfiles"),
        "expected ListProfiles among the v1 calls, got {v1:?}"
    );
    // Wrapped call sites count too: rustfmt breaks the line before the
    // method name when the turbofish is long, and a line-scoped extractor
    // skipped exactly those.
    // Exactly one call site takes its method name as a parameter: the
    // `call_unit` forwarder. More than that means a name is reaching the bus
    // without passing this check.
    let dynamic = call_sites(&src)
        .into_iter()
        .filter(|c| c.method.is_none())
        .count();
    assert_eq!(
        dynamic, 1,
        "expected exactly one forwarder with a parameterised method name"
    );
    // Names sent through that forwarder still have to be checked, and they
    // are: `call_unit`'s own call sites pass literals.
    assert!(
        v1.iter().any(|m| m == "DeleteProfile"),
        "call_unit's literals must be attributed to v1: {v1:?}"
    );
    assert!(
        v3.iter().any(|m| m == "PollProgress"),
        "the extractor misses call sites whose method name is on the next \
         line, so it proves less than it appears to: {v3:?}"
    );
    assert!(
        !v3.iter().any(|m| m == "NegotiateVersion"),
        "NegotiateVersion is not a method on any interface: {v3:?}"
    );
}
