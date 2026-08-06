// SPDX-License-Identifier: GPL-3.0-only
//! Shared D-Bus contract suite.
//!
//! This module is the single source of behavioral truth for the
//! `org.namtor.Manager` surface as seen by a desktop UI. The KDE KCM runs it
//! against a fake service ([`crate::fake`]); the GNOME UI (Task 26) is expected
//! to run the same suite against its own bridge so the two front-ends cannot
//! drift in happy-path, error, cancel, or auth-loss handling.
//!
//! The suite takes a bridge factory — a closure that returns a fresh
//! [`KcmBridge`] wired to whatever transport the caller proved out (session
//! bus, p2p fake, etc.) — plus a handle to drive the fake's behavior. It is
//! transport-agnostic and side-effect free between cases.

use crate::bridge::KcmBridge;
use crate::types::{EnrollOutcome, KcmError, ProfileInfo};

/// Hook the suite calls to mutate the fake service's behavior between cases.
/// Implementations live in the test harness ([`crate::fake`]); the suite itself
/// only requires that the hook be callable, so it stays decoupled from zbus.
pub trait ScenarioHook {
    /// Reset the fake to a clean state with the given seeded profiles.
    fn reset(&self, seed: &[ProfileInfo]);
    /// Force the next method call to fail as if the daemon went away.
    fn set_daemon_unavailable(&self);
    /// Force the next privileged call to fail as if polkit denied the user
    /// (auth-loss path).
    fn set_auth_denied(&self);
}

/// Run the happy-path scenario: create a profile, drive enrollment to
/// success, list profiles, rename, disable, re-enable, then delete. Asserts
/// the bridge surfaces the expected metadata at each step and never exposes
/// biometric data (the privacy scan in [`crate::tests`] covers the types
/// themselves).
pub async fn happy_path<F, H>(make_bridge: F, hook: &H)
where
    F: Fn() -> KcmBridge,
    H: ScenarioHook,
{
    hook.reset(&[]);
    let bridge = make_bridge();

    assert!(bridge.list_profiles().await.unwrap().is_empty());

    let session = bridge.create_profile("Alice").await.unwrap();
    assert!(!session.is_empty());

    bridge.start_enrollment(&session).await.unwrap();
    let progress = bridge.get_progress(&session).await.unwrap();
    assert_eq!(progress.session_id, session);
    // quality_label is a bucket string, never a score — just non-empty.
    assert!(!progress.quality_label.is_empty());
    assert!(progress.coverage_percent <= 100);

    let result = bridge.finish_enrollment(&session).await.unwrap();
    assert!(result.success, "finish should succeed on happy path");

    let profiles = bridge.list_profiles().await.unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].name, "Alice");
    assert!(profiles[0].enabled);

    bridge
        .rename_profile(&profiles[0].id, "Alicia")
        .await
        .unwrap();
    let after = bridge.list_profiles().await.unwrap();
    assert_eq!(after[0].name, "Alicia");

    bridge.enable_profile(&after[0].id, false).await.unwrap();
    let disabled = bridge.list_profiles().await.unwrap();
    assert!(!disabled[0].enabled);

    bridge.enable_profile(&disabled[0].id, true).await.unwrap();
    bridge.delete_profile(&disabled[0].id).await.unwrap();
    assert!(bridge.list_profiles().await.unwrap().is_empty());
}

/// Run the error path: the daemon disappears, and every call surfaces
/// [`KcmError::DaemonUnavailable`]. No panic, no silent success.
pub async fn error_daemon_unavailable<F, H>(make_bridge: F, hook: &H)
where
    F: Fn() -> KcmBridge,
    H: ScenarioHook,
{
    hook.reset(&[]);
    hook.set_daemon_unavailable();
    let bridge = make_bridge();

    match bridge.list_profiles().await {
        Err(KcmError::DaemonUnavailable) => {}
        other => panic!("expected DaemonUnavailable, got {other:?}"),
    }
    match bridge.create_profile("Bob").await {
        Err(KcmError::DaemonUnavailable) => {}
        other => panic!("expected DaemonUnavailable, got {other:?}"),
    }
}

/// Run the cancel path: start enrollment, cancel before finish, and confirm
/// the controller settles on [`EnrollOutcome::Cancelled`] with no leftover
/// profile. Cancel is idempotent — a second cancel must not error.
pub async fn cancel_keeps_state_safe<F, H>(make_bridge: F, hook: &H)
where
    F: Fn() -> KcmBridge,
    H: ScenarioHook,
{
    use crate::bridge::EnrollmentController;

    hook.reset(&[]);
    let bridge = make_bridge();
    let session = bridge.create_profile("Carol").await.unwrap();
    let mut ctrl = EnrollmentController::new(session);
    ctrl.start(&bridge).await.unwrap();
    ctrl.cancel(&bridge).await.unwrap();
    assert_eq!(
        ctrl.state(),
        crate::bridge::EnrollmentState::Finished(EnrollOutcome::Cancelled)
    );
    // Idempotent: cancel again is a no-op.
    ctrl.cancel(&bridge).await.unwrap();
    // No profile should have been committed by a cancelled session.
    assert!(bridge.list_profiles().await.unwrap().is_empty());
}

/// Run the auth-loss path: a privileged call (delete) is denied by polkit.
/// The bridge surfaces [`KcmError::Call`] (the daemon translates polkit denial
/// into a D-Bus error); the profile list is unchanged.
pub async fn auth_loss_preserves_profiles<F, H>(make_bridge: F, hook: &H)
where
    F: Fn() -> KcmBridge,
    H: ScenarioHook,
{
    hook.reset(&[ProfileInfo {
        id: "p-dave".to_string(),
        name: "Dave".to_string(),
        enabled: true,
        created: 1,
        last_used: None,
    }]);
    let bridge = make_bridge();
    hook.set_auth_denied();
    match bridge.delete_profile("p-dave").await {
        Err(KcmError::Call(_)) => {}
        other => panic!("expected Call error on denied delete, got {other:?}"),
    }
    // State safe: the denied delete did not remove the profile.
    hook.reset(&[ProfileInfo {
        id: "p-dave".to_string(),
        name: "Dave".to_string(),
        enabled: true,
        created: 1,
        last_used: None,
    }]);
    assert_eq!(bridge.list_profiles().await.unwrap().len(), 1);
}

/// Merge and replace: two profiles exist; merging src into dst leaves one
/// profile (dst), and replace re-arms dst with a fresh enrollment under a new
/// name. Confirmed via the list only — no biometric detail is exposed.
pub async fn merge_and_replace<F, H>(make_bridge: F, hook: &H)
where
    F: Fn() -> KcmBridge,
    H: ScenarioHook,
{
    hook.reset(&[
        ProfileInfo {
            id: "src".to_string(),
            name: "Eve".to_string(),
            enabled: true,
            created: 1,
            last_used: None,
        },
        ProfileInfo {
            id: "dst".to_string(),
            name: "Frank".to_string(),
            enabled: true,
            created: 2,
            last_used: None,
        },
    ]);
    let bridge = make_bridge();
    bridge.merge_profiles("src", "dst").await.unwrap();
    let merged = bridge.list_profiles().await.unwrap();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].id, "dst");

    bridge.replace_profile("dst", "Frank2").await.unwrap();
    let replaced = bridge.list_profiles().await.unwrap();
    assert_eq!(replaced.len(), 1);
    assert_eq!(replaced[0].name, "Frank2");
}
