// SPDX-FileCopyrightText: 2026 Nam-tor Project
// SPDX-License-Identifier: GPL-3.0-only

use namtor_client::client::ManagerClient;
use namtor_client::fake_bus::FakeBus;

#[test]
fn list_profiles_over_p2p_bus() {
    let bus = FakeBus::spawn().expect("spawn fake bus");
    let profiles = bus.client.list_profiles().expect("list_profiles");
    assert!(profiles.is_empty());
}

#[test]
fn enroll_and_commit_over_p2p_bus() {
    let bus = FakeBus::spawn().expect("spawn fake bus");
    let session = bus
        .client
        .start_enrollment(1000, "Alice")
        .expect("start_enrollment");
    assert!(!session.0.is_empty());
    let profile_id = bus.client.commit_profile(&session).expect("commit_profile");
    assert!(!profile_id.is_empty());
    let profiles = bus.client.list_profiles().expect("list_profiles");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].name, "Alice");
}
