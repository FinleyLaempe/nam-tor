// SPDX-License-Identifier: GPL-3.0-only
//! Every v1 and v2 method, driven through a real D-Bus connection.
//!
//! `DbusManagerClient` is the only implementation that speaks the wire, and
//! almost nothing exercised it: the presenter and UI tests use
//! `FakeManagerClient`, a trait mock, and the one wire test called
//! `get_status` and nothing else. Argument order, arity and types on the
//! other ten methods were therefore checked by no test at all —
//! `delete_profile` sends `(uid, id)` and a swap would have gone unnoticed
//! until someone deleted the wrong thing or, more likely, got a transport
//! error the CLI reports as a daemon problem.
//!
//! `FakeBus` is a real zbus server on a p2p pair, so a mismatch in any of
//! those fails here the way it would fail against the daemon.

use namtor_client::fake_bus::FakeBus;
use namtor_client::types::{ProfileId, SessionId};
use namtor_client::ManagerClient;

const UID: u32 = 1000;

fn enrol(bus: &FakeBus, name: &str) -> ProfileId {
    let session = bus.client.start_enrollment(UID, name).expect("start");
    ProfileId(bus.client.commit_profile(&session).expect("commit"))
}

#[test]
fn list_profiles_round_trips() {
    let bus = FakeBus::spawn().expect("spawn");
    assert!(bus.client.list_profiles().expect("list").is_empty());
    enrol(&bus, "one");
    let profiles = bus.client.list_profiles().expect("list");
    assert_eq!(profiles.len(), 1);
    assert!(!profiles[0].id.is_empty());
}

#[test]
fn start_and_cancel_enrollment_round_trip() {
    let bus = FakeBus::spawn().expect("spawn");
    let session = bus.client.start_enrollment(UID, "ida").expect("start");
    assert!(!session.0.is_empty(), "the session id must come back");
    bus.client.cancel_enrollment(&session).expect("cancel");
    assert!(
        bus.client.list_profiles().expect("list").is_empty(),
        "a cancelled enrollment writes no profile"
    );
}

/// `RenameProfile(uid, id, name)` — three arguments in that order. A swap of
/// the two strings renames nothing and reports success.
#[test]
fn rename_sends_its_arguments_in_the_declared_order() {
    let bus = FakeBus::spawn().expect("spawn");
    let id = enrol(&bus, "before");
    bus.client
        .rename_profile(UID, &id, "after")
        .expect("rename");
    let profiles = bus.client.list_profiles().expect("list");
    assert_eq!(profiles[0].name, "after");
}

/// `EnableProfile(uid, id, enabled)`.
#[test]
fn enable_round_trips_both_ways() {
    let bus = FakeBus::spawn().expect("spawn");
    let id = enrol(&bus, "ida");
    assert!(bus.client.list_profiles().expect("list")[0].enabled);
    bus.client.enable_profile(UID, &id, false).expect("disable");
    assert!(!bus.client.list_profiles().expect("list")[0].enabled);
    bus.client.enable_profile(UID, &id, true).expect("enable");
    assert!(bus.client.list_profiles().expect("list")[0].enabled);
}

/// `DeleteProfile(uid, id)` — the call behind `namtor cli remove`.
#[test]
fn delete_removes_the_named_profile_and_only_that_one() {
    let bus = FakeBus::spawn().expect("spawn");
    let first = enrol(&bus, "first");
    let second = enrol(&bus, "second");
    assert_ne!(first.0, second.0);

    bus.client.delete_profile(UID, &first).expect("delete");
    let left = bus.client.list_profiles().expect("list");
    assert_eq!(left.len(), 1, "exactly one profile should be gone");
    assert_eq!(left[0].id, second.0, "the wrong profile was deleted");
}

/// `MergeProfiles(uid, src, dst)` — src is absorbed into dst, so the
/// direction matters and a swap silently deletes the wrong one.
#[test]
fn merge_absorbs_the_source_into_the_destination() {
    let bus = FakeBus::spawn().expect("spawn");
    let src = enrol(&bus, "src");
    let dst = enrol(&bus, "dst");
    bus.client.merge_profiles(UID, &src, &dst).expect("merge");
    let left = bus.client.list_profiles().expect("list");
    assert_eq!(left.len(), 1);
    assert_eq!(
        left[0].id, dst.0,
        "the destination must survive, not the source"
    );
}

/// `ReplaceProfile(uid, id, name)`.
#[test]
fn replace_round_trips() {
    let bus = FakeBus::spawn().expect("spawn");
    let id = enrol(&bus, "old");
    bus.client
        .replace_profile(UID, &id, "replacement")
        .expect("replace");
    assert_eq!(
        bus.client.list_profiles().expect("list")[0].name,
        "replacement"
    );
}

/// `ReviewProfile(uid, id) -> (sbs)` — the only v1 method with a struct
/// reply besides GetStatus, so its deserialisation is worth crossing the
/// wire for.
#[test]
fn review_deserialises_its_struct_reply() {
    let bus = FakeBus::spawn().expect("spawn");
    let id = enrol(&bus, "ida");
    let review = bus.client.review_profile(UID, &id).expect("review");
    assert_eq!(review.profile_id, id.0);
    assert!(!review.needs_reenroll);
}

#[test]
fn get_status_deserialises_its_nested_reply() {
    let bus = FakeBus::spawn().expect("spawn");
    enrol(&bus, "ida");
    let status = bus.client.get_status().expect("status");
    assert_eq!(status.api_version, 1);
    assert_eq!(status.profiles.len(), 1);
    assert!(!status.emitter_armed);
}

/// A method error must arrive as a typed `ManagerError`, not as a transport
/// failure — the CLI prints those very differently.
#[test]
fn a_missing_profile_comes_back_as_an_error_not_a_transport_failure() {
    let bus = FakeBus::spawn().expect("spawn");
    let err = bus
        .client
        .review_profile(UID, &ProfileId("no-such-profile".into()))
        .unwrap_err();
    assert!(
        matches!(err, namtor_client::types::ManagerError::NotFound(_)),
        "the daemon's org.namtor.Error.NotFound must map to ManagerError::NotFound, \
         not to a transport failure the CLI reports as a daemon problem: {err:?}"
    );
}

/// v1 progress is delivered by signal, not polled, so `poll_progress` reads
/// a cache that is empty until one arrives. It must say so rather than
/// erroring or blocking.
#[test]
fn poll_progress_is_empty_before_any_signal() {
    let bus = FakeBus::spawn().expect("spawn");
    let progress = bus
        .client
        .poll_progress(&SessionId("sess-1".into()))
        .expect("poll must not fail with an empty cache");
    assert!(progress.is_none());
}

// ---------------------------------------------------------------------------
// v2 — org.namtor.Manager2
// ---------------------------------------------------------------------------

/// `ListPamServices` returns `a(ssbss)`, which nests an enum rendered as a
/// string. Only a real message proves the enum survives the round trip.
#[test]
fn the_pam_service_list_round_trips_with_its_enum() {
    use namtor_client::types::PamServiceCompatibility;
    let bus = FakeBus::spawn().expect("spawn");
    let services = bus.client.list_pam_services().expect("list");
    assert!(!services.is_empty());
    let kinds: Vec<_> = services.iter().map(|s| s.compatibility).collect();
    assert!(kinds.contains(&PamServiceCompatibility::Safe), "{kinds:?}");
    assert!(kinds.contains(&PamServiceCompatibility::Risky), "{kinds:?}");
    assert!(
        kinds.contains(&PamServiceCompatibility::Unsupported),
        "{kinds:?}"
    );
}

/// `SetPamServiceEnabled(service_id, enabled)` — two arguments, and the
/// change has to be visible to a later list.
#[test]
fn enabling_a_pam_service_round_trips() {
    use namtor_client::types::PamServiceCompatibility;
    let bus = FakeBus::spawn().expect("spawn");
    let target = bus
        .client
        .list_pam_services()
        .expect("list")
        .into_iter()
        .find(|s| s.compatibility == PamServiceCompatibility::Safe)
        .expect("a safe service");
    assert!(!target.enabled);

    assert!(bus
        .client
        .set_pam_service_enabled(&target.id, true)
        .expect("enable"));
    let after = bus
        .client
        .list_pam_services()
        .expect("list")
        .into_iter()
        .find(|s| s.id == target.id)
        .expect("still listed");
    assert!(after.enabled, "the toggle did not reach the daemon");
}

/// An incompatible service refuses with a typed error, not a transport
/// failure — the UI shows the two very differently.
#[test]
fn an_unsupported_pam_service_refuses_with_a_typed_error() {
    use namtor_client::types::{ManagerError, PamServiceCompatibility};
    let bus = FakeBus::spawn().expect("spawn");
    let target = bus
        .client
        .list_pam_services()
        .expect("list")
        .into_iter()
        .find(|s| s.compatibility == PamServiceCompatibility::Unsupported)
        .expect("an unsupported service");
    let err = bus
        .client
        .set_pam_service_enabled(&target.id, true)
        .unwrap_err();
    assert!(
        matches!(err, ManagerError::Unsupported(_)),
        "expected Unsupported, got {err:?}"
    );
}

/// `GetProtectionStatus` returns `(sbs)` with an enum in the first slot.
#[test]
fn the_protection_status_round_trips() {
    use namtor_client::types::ProtectionKeyKind;
    let bus = FakeBus::spawn().expect("spawn");
    let status = bus.client.get_protection_status().expect("status");
    assert_eq!(status.sealing, ProtectionKeyKind::Tpm2);
    assert!(status.has_tpm);
}
