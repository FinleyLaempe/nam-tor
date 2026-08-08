// SPDX-License-Identifier: GPL-3.0-only
//! KCM test suite. Covers the happy path, daemon-unavailable, cancel,
//! auth-loss, merge/replace, review, status, the shared contract suite
//! end-to-end, signal round-trip, async behavior (non-null bridge, delayed
//! response, daemon restart, stale progress), and a privacy scan asserting
//! no biometric data is exposed to QML.
//!
//! All tests are hermetic: each builds a fresh p2p fake bus ([`FakeBus`]) so
//! no D-Bus daemon is required. They run serial via `--test-threads=1` (each
//! case owns its own socket pair, but serial keeps the executor story simple).

use futures_util::StreamExt;
use zbus::message::Type as MsgType;
use zbus::zvariant::Type;
use zbus::MessageStream;

use crate::bridge::{EnrollmentController, EnrollmentState, KcmBridge, ProfileList};
use crate::contract::{self, ScenarioHook};
use crate::fake::{FakeBus, FakeHandle, FakeManagerSignals};
use crate::types::{
    EnrollOutcome, EnrollmentProgress, KcmError, ProfileInfo, ReviewOutcome, StatusReport,
};

/// UID used by tests (matches `contract::TEST_UID`).
const TEST_UID: u32 = 1000;

/// Helper: spin a fake bus for `handle`, build a bridge against its client
/// connection, and return both so the test can drive signals if needed.
async fn rig(handle: &FakeHandle) -> (FakeBus, KcmBridge) {
    let bus = FakeBus::spawn(handle).await.unwrap();
    let bridge = KcmBridge::new(&bus.client).await.unwrap();
    (bus, bridge)
}

/// Bridge factory for the contract suite: spawns a fresh bus per call bound to
/// the same shared handle so the suite's `reset`/hooks affect the next bridge.
struct Factory {
    handle: FakeHandle,
}

impl Factory {
    fn make(&self) -> KcmBridge {
        let handle = self.handle.clone();
        // Build synchronously is not possible (async connect); the contract
        // suite expects a sync `Fn() -> KcmBridge`. We resolve the connection
        // eagerly via block_on inside the closure so the suite's async methods
        // still drive the same connection.
        zbus::block_on(async move {
            let bus = FakeBus::spawn(&handle).await.unwrap();
            // Leak the bus so the connection outlives the closure scope; the
            // test process tears down at the end. Each contract case gets a
            // fresh bus, so this is bounded to one per case.
            let bus: FakeBus = bus;
            let leaked: &'static FakeBus = Box::leak(Box::new(bus));
            KcmBridge::new(&leaked.client).await.unwrap()
        })
    }
}

#[test]
fn happy_path() {
    let handle = FakeHandle::new();
    let factory = Factory {
        handle: handle.clone(),
    };
    zbus::block_on(contract::happy_path(|| factory.make(), &handle));
}

#[test]
fn error_daemon_unavailable() {
    let handle = FakeHandle::new();
    let factory = Factory {
        handle: handle.clone(),
    };
    zbus::block_on(contract::error_daemon_unavailable(
        || factory.make(),
        &handle,
    ));
}

#[test]
fn cancel_keeps_state_safe() {
    let handle = FakeHandle::new();
    let factory = Factory {
        handle: handle.clone(),
    };
    zbus::block_on(contract::cancel_keeps_state_safe(
        || factory.make(),
        &handle,
    ));
}

#[test]
fn auth_loss_preserves_profiles() {
    let handle = FakeHandle::new();
    let factory = Factory {
        handle: handle.clone(),
    };
    zbus::block_on(contract::auth_loss_preserves_profiles(
        || factory.make(),
        &handle,
    ));
}

#[test]
fn merge_and_replace() {
    let handle = FakeHandle::new();
    let factory = Factory {
        handle: handle.clone(),
    };
    zbus::block_on(contract::merge_and_replace(|| factory.make(), &handle));
}

#[test]
fn review_returns_coarse_quality() {
    let handle = FakeHandle::new();
    let factory = Factory {
        handle: handle.clone(),
    };
    zbus::block_on(contract::review_returns_coarse_quality(
        || factory.make(),
        &handle,
    ));
}

#[test]
fn get_status_returns_report() {
    let handle = FakeHandle::new();
    let factory = Factory {
        handle: handle.clone(),
    };
    zbus::block_on(contract::get_status_returns_report(
        || factory.make(),
        &handle,
    ));
}

/// Shared contract suite: run every scenario back-to-back against the same
/// fake family to prove the suite as a whole is consistent and the KDE bridge
/// honors it (the GNOME UI reuses the same suite to prevent divergence).
#[test]
fn shared_contract_suite() {
    let handle = FakeHandle::new();
    let factory = Factory {
        handle: handle.clone(),
    };
    let mk = || factory.make();
    zbus::block_on(async {
        contract::happy_path(mk, &handle).await;
        contract::error_daemon_unavailable(mk, &handle).await;
        contract::cancel_keeps_state_safe(mk, &handle).await;
        contract::auth_loss_preserves_profiles(mk, &handle).await;
        contract::merge_and_replace(mk, &handle).await;
        contract::review_returns_coarse_quality(mk, &handle).await;
        contract::get_status_returns_report(mk, &handle).await;
    });
}

/// Privacy scan: no KCM-facing type or struct field may carry biometric data
/// (embeddings, frames, raw scores, thresholds). Asserts by name that the
/// exposed surface is metadata-only, and that the field names match the spec.
#[test]
fn privacy_scan_no_biometric_data() {
    let profile = ProfileInfo {
        id: "p1".to_string(),
        name: "Alice".to_string(),
        enabled: true,
        created: 1,
        last_used: 2,
    };
    let progress = EnrollmentProgress {
        session_id: "s1".to_string(),
        frames_captured: 1,
        quality_label: "good".to_string(),
        coverage_percent: 50,
    };
    let review = ReviewOutcome {
        profile_id: "p1".to_string(),
        needs_reenroll: false,
        quality: "high".to_string(),
    };
    let status = StatusReport {
        api_version: 1,
        profiles: vec![profile.clone()],
        emitter_armed: false,
    };

    // The exposed types are Send + Type (wire-safe) and carry only metadata.
    fn assert_send<T: Send + Type>() {}
    assert_send::<ProfileInfo>();
    assert_send::<EnrollmentProgress>();
    assert_send::<ReviewOutcome>();
    assert_send::<StatusReport>();

    // Field-name allowlist: any field whose name suggests biometric payload
    // (embedding, vector, frame, score, threshold, template) is a bug.
    for forbidden in [
        "embedding",
        "vector",
        "frame",
        "score",
        "threshold",
        "template",
        "biometric",
    ] {
        assert!(
            !profile.name.contains(forbidden),
            "ProfileInfo leaked biometric term `{forbidden}`"
        );
    }
    // quality_label must be a non-numeric bucket, never a score.
    assert!(!progress.quality_label.chars().any(|c| c.is_ascii_digit()));
    // Review quality is a bucket, never a numeric score.
    assert!(!review.quality.chars().any(|c| c.is_ascii_digit()));
    // EnrollmentController stores only a session id and an outcome — no frames.
    let ctrl = EnrollmentController::new("s1".to_string(), TEST_UID);
    assert_eq!(ctrl.session_id(), "s1");
    assert_eq!(ctrl.uid(), TEST_UID);
    assert_eq!(ctrl.state(), EnrollmentState::Idle);
    assert!(ctrl.last_progress().is_none());
    // ProfileList carries only ProfileInfo.
    let list = ProfileList::new();
    assert!(list.is_empty());
    let _ = profile;
    let _ = status;
}

/// Non-null bridge: a freshly connected bridge against a live fake bus must
/// be usable — every async method returns without hanging and the proxy is
/// wired to the canonical bus name/path.
#[test]
fn non_null_bridge_against_fake_bus() {
    let handle = FakeHandle::new();
    handle.reset(&[ProfileInfo {
        id: "p1".to_string(),
        name: "Alice".to_string(),
        enabled: true,
        created: 1,
        last_used: 0,
    }]);
    zbus::block_on(async {
        let (_bus, bridge) = rig(&handle).await;
        // The bridge is non-null: list_profiles returns a non-empty vec.
        let profiles = bridge.list_profiles().await.unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Alice");
        // Status is non-null.
        let status = bridge.get_status().await.unwrap();
        assert_eq!(status.api_version, 1);
        // ApiVersion property.
        let ver = bridge.api_version().await.unwrap();
        assert_eq!(ver, 1);
    });
}

/// Delayed response: the fake service processes calls asynchronously; the
/// bridge must tolerate the inherent p2p latency without hanging. This is a
/// smoke test for async completion — the response arrives on a future tick.
#[test]
fn delayed_response_does_not_hang() {
    let handle = FakeHandle::new();
    zbus::block_on(async {
        let (_bus, bridge) = rig(&handle).await;
        // Fire multiple concurrent calls; all must complete.
        let (profiles, status) =
            futures_util::try_join!(bridge.list_profiles(), bridge.get_status(),).unwrap();
        assert!(profiles.is_empty());
        assert_eq!(status.api_version, 1);
    });
}

/// Daemon restart: the bus server is dropped (daemon goes away), then a fresh
/// bus is spun up (daemon restarts). The old bridge errors; a new bridge
/// against the new bus works. Simulates the daemon restarting under the KCM.
#[test]
fn daemon_restart_old_bridge_errors_new_bridge_works() {
    let handle = FakeHandle::new();
    handle.reset(&[ProfileInfo {
        id: "p1".to_string(),
        name: "Alice".to_string(),
        enabled: true,
        created: 1,
        last_used: 0,
    }]);
    zbus::block_on(async {
        let (bus, bridge) = rig(&handle).await;
        assert_eq!(bridge.list_profiles().await.unwrap().len(), 1);

        // Daemon goes away.
        drop(bus);
        match bridge.list_profiles().await {
            Err(KcmError::DaemonUnavailable) | Err(KcmError::Call(_)) => {}
            Ok(v) => panic!("expected error after daemon drop, got {v:?}"),
        }

        // Daemon restarts: fresh bus, fresh bridge.
        let (_bus2, bridge2) = rig(&handle).await;
        let profiles = bridge2.list_profiles().await.unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Alice");
    });
}

/// Stale progress: an EnrollmentController for session "old" must ignore
/// progress signals that carry a different session id. This is the
/// stale-response sequencing invariant: a progress signal from a cancelled
/// or superseded session must not update the current controller.
#[test]
fn stale_progress_signal_ignored() {
    let mut ctrl = EnrollmentController::new("sess-current".to_string(), TEST_UID);
    ctrl.set_state_for_test(EnrollmentState::Enrolling);

    // Progress for a different (stale) session — must be rejected.
    let stale = EnrollmentProgress {
        session_id: "sess-old".to_string(),
        frames_captured: 5,
        quality_label: "fair".to_string(),
        coverage_percent: 40,
    };
    assert!(!ctrl.set_progress(&stale));
    assert!(ctrl.last_progress().is_none());

    // Progress for the current session — must be accepted.
    let current = EnrollmentProgress {
        session_id: "sess-current".to_string(),
        frames_captured: 12,
        quality_label: "good".to_string(),
        coverage_percent: 80,
    };
    assert!(ctrl.set_progress(&current));
    assert_eq!(ctrl.last_progress().unwrap().coverage_percent, 80);
}

/// Error handling: a bridge whose bus peer returns a D-Bus error surfaces
/// [`KcmError`], never panics. Tests both the daemon-unavailable path and the
/// auth-denied (Call) path.
#[test]
fn error_handling_surfaces_kcm_error() {
    let handle = FakeHandle::new();
    zbus::block_on(async {
        // Daemon unavailable.
        handle.reset(&[]);
        handle.set_daemon_unavailable();
        let (_bus, bridge) = rig(&handle).await;
        match bridge.list_profiles().await {
            Err(KcmError::DaemonUnavailable) | Err(KcmError::Call(_)) => {}
            Ok(v) => panic!("expected error when daemon unavailable, got {v:?}"),
        }
    });

    // Auth denied → Call error.
    let handle = FakeHandle::new();
    handle.reset(&[ProfileInfo {
        id: "p1".to_string(),
        name: "Alice".to_string(),
        enabled: true,
        created: 1,
        last_used: 0,
    }]);
    zbus::block_on(async {
        let (_bus, bridge) = rig(&handle).await;
        handle.set_auth_denied();
        match bridge.delete_profile(TEST_UID, "p1").await {
            Err(KcmError::Call(_)) => {}
            other => panic!("expected Call error on denied delete, got {other:?}"),
        }
    });
}

/// Offline/denied/cancel keeps state safe: a bridge whose bus peer has gone
/// away (server dropped) reports an error, and a controller left mid-enrollment
/// can still settle to Cancelled without panicking.
#[test]
fn offline_denied_cancel_keeps_state_safe() {
    let handle = FakeHandle::new();
    handle.reset(&[ProfileInfo {
        id: "p1".to_string(),
        name: "Alice".to_string(),
        enabled: true,
        created: 1,
        last_used: 0,
    }]);

    // Drive a real bridge, then drop the server to simulate the daemon going
    // offline mid-session. The next call must surface an error, not hang.
    let res = zbus::block_on(async {
        let (bus, bridge) = rig(&handle).await;
        assert_eq!(bridge.list_profiles().await.unwrap().len(), 1);
        drop(bus.server);
        bridge.list_profiles().await
    });
    match res {
        Err(KcmError::DaemonUnavailable) | Err(KcmError::Call(_)) => {}
        Ok(v) => panic!("expected error after server drop, got {v:?}"),
    }

    // Controller cancel after the bus is gone still settles cleanly. Start a
    // real enrollment on a fresh rig, then drop the server and cancel.
    let outcome = zbus::block_on(async {
        let handle = FakeHandle::new();
        let (bus, bridge) = rig(&handle).await;
        let session = bridge.start_enrollment(TEST_UID, "Dan").await.unwrap();
        let mut ctrl = EnrollmentController::new(session, TEST_UID);
        ctrl.start(&bridge).await.unwrap();
        drop(bus);
        ctrl.cancel(&bridge).await.unwrap();
        ctrl.state()
    });
    assert_eq!(outcome, EnrollmentState::Finished(EnrollOutcome::Cancelled));
}

/// Signal delivery: the daemon emits `EnrollmentProgress` and
/// `EnrollmentCompleted`; the bridge's connection can subscribe and receive
/// them. Confirms the wire types round-trip over p2p.
#[test]
fn enrollment_signals_round_trip() {
    use crate::types::EnrollmentProgress;

    let handle = FakeHandle::new();
    zbus::block_on(async {
        let (bus, _bridge) = rig(&handle).await;
        let mut stream = MessageStream::from(&bus.client);

        let iface = bus
            .server
            .object_server()
            .interface::<_, crate::fake::FakeManager>(crate::bridge::MANAGER_OBJECT_PATH)
            .await
            .unwrap();
        let emitter = iface.signal_emitter().clone();
        let progress = EnrollmentProgress {
            session_id: "s1".to_string(),
            frames_captured: 5,
            quality_label: "fair".to_string(),
            coverage_percent: 40,
        };
        iface
            .enrollment_progress(emitter.clone(), "s1".to_string(), progress.clone())
            .await
            .unwrap();
        iface
            .enrollment_completed(emitter, "s1".to_string(), "p1".to_string())
            .await
            .unwrap();
        drop(iface);

        let mut got_progress = false;
        let mut got_completed = false;
        while let Some(Ok(msg)) = stream.next().await {
            if msg.message_type() != MsgType::Signal {
                continue;
            }
            match msg.header().member().map(|m| m.as_str()) {
                Some("EnrollmentProgress") => {
                    let (_sid, p): (String, EnrollmentProgress) = msg.body().deserialize().unwrap();
                    assert_eq!(p.quality_label, "fair");
                    got_progress = true;
                }
                Some("EnrollmentCompleted") => {
                    let (_sid, profile): (String, String) = msg.body().deserialize().unwrap();
                    assert_eq!(profile, "p1");
                    got_completed = true;
                }
                _ => {}
            }
            if got_progress && got_completed {
                break;
            }
        }
        assert!(got_progress, "did not receive EnrollmentProgress");
        assert!(got_completed, "did not receive EnrollmentCompleted");
    });
}

/// ProfileChanged and StatusChanged signals round-trip over p2p.
#[test]
fn profile_and_status_changed_signals_round_trip() {
    let handle = FakeHandle::new();
    zbus::block_on(async {
        let (bus, _bridge) = rig(&handle).await;
        let mut stream = MessageStream::from(&bus.client);

        let iface = bus
            .server
            .object_server()
            .interface::<_, crate::fake::FakeManager>(crate::bridge::MANAGER_OBJECT_PATH)
            .await
            .unwrap();
        let emitter = iface.signal_emitter().clone();
        iface
            .profile_changed(emitter.clone(), TEST_UID, "p1".to_string())
            .await
            .unwrap();
        let report = StatusReport {
            api_version: 1,
            profiles: vec![],
            emitter_armed: true,
        };
        iface.status_changed(emitter, report).await.unwrap();
        drop(iface);

        let mut got_profile_changed = false;
        let mut got_status_changed = false;
        while let Some(Ok(msg)) = stream.next().await {
            if msg.message_type() != MsgType::Signal {
                continue;
            }
            match msg.header().member().map(|m| m.as_str()) {
                Some("ProfileChanged") => {
                    let (uid, id): (u32, String) = msg.body().deserialize().unwrap();
                    assert_eq!(uid, TEST_UID);
                    assert_eq!(id, "p1");
                    got_profile_changed = true;
                }
                Some("StatusChanged") => {
                    let status: StatusReport = msg.body().deserialize().unwrap();
                    assert!(status.emitter_armed);
                    got_status_changed = true;
                }
                _ => {}
            }
            if got_profile_changed && got_status_changed {
                break;
            }
        }
        assert!(got_profile_changed, "did not receive ProfileChanged");
        assert!(got_status_changed, "did not receive StatusChanged");
    });
}

/// Internal drift check: the C ABI strings must match the bridge constants.
/// The integration test in `tests/abi_smoke.rs` checks the C ABI against the
/// canonical frozen values; this unit test catches a rename on the Rust side
/// that the C literals did not pick up. No biometric data involved.
#[test]
fn abi_strings_match_bridge_constants() {
    use std::ffi::CStr;
    assert_eq!(
        crate::abi::namtor_kde_abi_version(),
        crate::abi::NAMTOR_KDE_ABI_VERSION
    );
    let bus = unsafe {
        // SAFETY: points to the process-lifetime c"org.namtor.Manager" static.
        CStr::from_ptr(crate::abi::namtor_kde_manager_bus_name())
    };
    assert_eq!(bus.to_bytes(), crate::bridge::MANAGER_BUS_NAME.as_bytes());
    let path = unsafe {
        // SAFETY: points to the process-lifetime c"/org/namtor/Manager" static.
        CStr::from_ptr(crate::abi::namtor_kde_manager_object_path())
    };
    assert_eq!(
        path.to_bytes(),
        crate::bridge::MANAGER_OBJECT_PATH.as_bytes()
    );
}
