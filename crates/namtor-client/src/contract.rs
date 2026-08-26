// SPDX-FileCopyrightText: 2026 Nam-tor Project
// SPDX-License-Identifier: GPL-3.0-only

//! Shared D-Bus contract suite — transport-agnostic behavioral tests.
//!
//! This module is the single source of behavioral truth for the
//! `org.namtor.Manager` surface as seen by a desktop UI. Both the GNOME
//! and KDE front-ends run these scenarios against [`crate::fake::FakeManagerClient`]
//! so the two UIs cannot drift in happy-path, error, cancel, or auth-loss
//! handling.

use crate::client::ManagerClient;
use crate::fake::{FakeManagerClient, Mode};
use crate::types::{ManagerError, ProfileId, ProfileInfo};

/// UID used by the contract suite (matches a typical desktop user).
const TEST_UID: u32 = 1000;

/// Hook the suite calls to mutate the fake service's behavior between cases.
pub trait ScenarioHook {
    /// Reset the fake to a clean state with the given seeded profiles.
    fn reset(&self, seed: &[ProfileInfo]);
    /// Force the next method call to fail as if the daemon went away.
    fn set_daemon_unavailable(&self);
    /// Force the next privileged call to fail as if polkit denied the user.
    fn set_auth_denied(&self);
}

/// [`ScenarioHook`] implementation for [`FakeManagerClient`].
impl ScenarioHook for FakeManagerClient {
    fn reset(&self, seed: &[ProfileInfo]) {
        self.reset(seed);
    }
    fn set_daemon_unavailable(&self) {
        self.set_mode(Mode::DaemonDown("daemon unavailable".into()));
    }
    fn set_auth_denied(&self) {
        self.set_mode(Mode::PolkitDenied("polkit denied".into()));
    }
}

/// Run the happy-path scenario: start enrollment, commit, list, rename,
/// disable, re-enable, delete.
pub fn happy_path<C: ManagerClient, H: ScenarioHook>(client: &C, hook: &H) {
    hook.reset(&[]);
    assert!(client.list_profiles().unwrap().is_empty());
    let session = client.start_enrollment(TEST_UID, "Alice").unwrap();
    let _pid = client.commit_profile(&session).unwrap();
    let profiles = client.list_profiles().unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].name, "Alice");
    assert!(profiles[0].enabled);
    client
        .rename_profile(TEST_UID, &ProfileId(profiles[0].id.clone()), "Alicia")
        .unwrap();
    let after = client.list_profiles().unwrap();
    assert_eq!(after[0].name, "Alicia");
    client
        .enable_profile(TEST_UID, &ProfileId(after[0].id.clone()), false)
        .unwrap();
    assert!(!client.list_profiles().unwrap()[0].enabled);
    client
        .enable_profile(TEST_UID, &ProfileId(after[0].id.clone()), true)
        .unwrap();
    client
        .delete_profile(TEST_UID, &ProfileId(after[0].id.clone()))
        .unwrap();
    assert!(client.list_profiles().unwrap().is_empty());
}

/// Daemon unavailable: every call returns [`ManagerError::DaemonUnavailable`].
pub fn error_daemon_unavailable<C: ManagerClient, H: ScenarioHook>(client: &C, hook: &H) {
    hook.reset(&[]);
    hook.set_daemon_unavailable();
    match client.list_profiles() {
        Err(ManagerError::DaemonUnavailable(_)) => {}
        other => panic!("expected DaemonUnavailable, got {other:?}"),
    }
    match client.start_enrollment(TEST_UID, "Bob") {
        Err(ManagerError::DaemonUnavailable(_)) => {}
        other => panic!("expected DaemonUnavailable, got {other:?}"),
    }
}

/// Cancel before commit: no profile is left behind.
pub fn cancel_keeps_state_safe<C: ManagerClient, H: ScenarioHook>(client: &C, hook: &H) {
    hook.reset(&[]);
    let session = client.start_enrollment(TEST_UID, "Carol").unwrap();
    client.cancel_enrollment(&session).unwrap();
    assert!(client.list_profiles().unwrap().is_empty());
}

/// Auth loss: a privileged call is denied; profiles are preserved.
pub fn auth_loss_preserves_profiles<C: ManagerClient, H: ScenarioHook>(client: &C, hook: &H) {
    hook.reset(&[ProfileInfo {
        id: "p-dave".into(),
        name: "Dave".into(),
        enabled: true,
        created: 1,
        last_used: 0,
    }]);
    hook.set_auth_denied();
    match client.delete_profile(TEST_UID, &ProfileId("p-dave".into())) {
        Err(ManagerError::Denied(_)) => {}
        other => panic!("expected Denied, got {other:?}"),
    }
    hook.reset(&[ProfileInfo {
        id: "p-dave".into(),
        name: "Dave".into(),
        enabled: true,
        created: 1,
        last_used: 0,
    }]);
    assert_eq!(client.list_profiles().unwrap().len(), 1);
}

/// Merge src into dst, then replace dst with a new name.
pub fn merge_and_replace<C: ManagerClient, H: ScenarioHook>(client: &C, hook: &H) {
    hook.reset(&[
        ProfileInfo {
            id: "src".into(),
            name: "Eve".into(),
            enabled: true,
            created: 1,
            last_used: 0,
        },
        ProfileInfo {
            id: "dst".into(),
            name: "Frank".into(),
            enabled: true,
            created: 2,
            last_used: 0,
        },
    ]);
    client
        .merge_profiles(TEST_UID, &ProfileId("src".into()), &ProfileId("dst".into()))
        .unwrap();
    let merged = client.list_profiles().unwrap();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].id, "dst");
    client
        .replace_profile(TEST_UID, &ProfileId("dst".into()), "Frank2")
        .unwrap();
    let replaced = client.list_profiles().unwrap();
    assert_eq!(replaced.len(), 1);
    assert_eq!(replaced[0].name, "Frank2");
}
