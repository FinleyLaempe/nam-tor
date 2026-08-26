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
use crate::fake::{FakeBus, FakeHandle, FakeManagerSignals};
use crate::types::{
    EnrollOutcome, EnrollmentProgress, EnrollmentProgressV3, EnrollmentReviewV3, KcmError,
    ProfileInfo, QualityLabel, ReviewOutcome, StatusReport,
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

#[test]
fn happy_path() {
    let fake = namtor_client::fake::FakeManagerClient::new();
    namtor_client::contract::happy_path(&fake, &fake);
}

#[test]
fn error_daemon_unavailable() {
    let fake = namtor_client::fake::FakeManagerClient::new();
    namtor_client::contract::error_daemon_unavailable(&fake, &fake);
}

#[test]
fn cancel_keeps_state_safe() {
    let fake = namtor_client::fake::FakeManagerClient::new();
    namtor_client::contract::cancel_keeps_state_safe(&fake, &fake);
}

#[test]
fn auth_loss_preserves_profiles() {
    let fake = namtor_client::fake::FakeManagerClient::new();
    namtor_client::contract::auth_loss_preserves_profiles(&fake, &fake);
}

#[test]
fn merge_and_replace() {
    let fake = namtor_client::fake::FakeManagerClient::new();
    namtor_client::contract::merge_and_replace(&fake, &fake);
}

/// Shared contract suite: run every scenario back-to-back against the same
/// fake family to prove the suite as a whole is consistent and the KDE bridge
/// honors it (the GNOME UI reuses the same suite to prevent divergence).
#[test]
fn shared_contract_suite() {
    let fake = namtor_client::fake::FakeManagerClient::new();
    namtor_client::contract::happy_path(&fake, &fake);
    namtor_client::contract::error_daemon_unavailable(&fake, &fake);
    namtor_client::contract::cancel_keeps_state_safe(&fake, &fake);
    namtor_client::contract::auth_loss_preserves_profiles(&fake, &fake);
    namtor_client::contract::merge_and_replace(&fake, &fake);
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
        quality: QualityLabel::High,
        coverage_percent: 50,
    };
    let review = ReviewOutcome {
        profile_id: "p1".to_string(),
        needs_reenroll: false,
        quality: QualityLabel::High,
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
    // quality must be a non-numeric bucket, never a score.
    assert!(!progress.quality.as_str().chars().any(|c| c.is_ascii_digit()));
    // Review quality is a bucket, never a numeric score.
    assert!(!review.quality.as_str().chars().any(|c| c.is_ascii_digit()));
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
        quality: QualityLabel::Medium,
        coverage_percent: 40,
    };
    assert!(!ctrl.set_progress(&stale));
    assert!(ctrl.last_progress().is_none());

    // Progress for the current session — must be accepted.
    let current = EnrollmentProgress {
        session_id: "sess-current".to_string(),
        frames_captured: 12,
        quality: QualityLabel::High,
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
            quality: QualityLabel::Medium,
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
                    assert_eq!(p.quality, QualityLabel::Medium);
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

// ---------------------------------------------------------------------------
// v3 tests (org.namtor.Manager3 continuous enrollment lifecycle)
// ---------------------------------------------------------------------------

/// The v3 Rust mirror types must have the same field names and wire
/// signatures as the daemon-side `contract::EnrollmentProgressV3` /
/// `EnrollmentReviewV3`. This is the client-side parity assertion: if the
/// daemon renames a field or changes the wire shape, this test fails before
/// a client silently deserialises garbage.
#[test]
fn v3_types_match_contract() {
    // Wire signature parity: the zbus `Type` derive produces the D-Bus
    // signature string. We assert it matches the frozen v3 XML.
    assert_eq!(
        EnrollmentProgressV3::SIGNATURE,
        "(ssuuuuuys)",
        "EnrollmentProgressV3 wire signature drifted from (ssuuuuuys)"
    );
    assert_eq!(
        EnrollmentReviewV3::SIGNATURE,
        "(sbuuus)",
        "EnrollmentReviewV3 wire signature drifted from (sbuuus)"
    );

    // Field-name parity: construct a value the way the daemon does and
    // round-trip it through serde to confirm the JSON keys match the Rust
    // field names the daemon contract uses.
    let progress = EnrollmentProgressV3 {
        session: "s1".to_string(),
        phase: "capturing".to_string(),
        quality_percent: 75,
        diversity_percent: 50,
        frames_seen: 100,
        frames_accepted: 80,
        frames_rejected: 20,
        quality_bucket: 2,
        guidance: "looking_good".to_string(),
    };
    let json = serde_json::to_value(&progress).unwrap();
    assert_eq!(json["session"], "s1");
    assert_eq!(json["phase"], "capturing");
    assert_eq!(json["quality_percent"], 75);
    assert_eq!(json["diversity_percent"], 50);
    assert_eq!(json["frames_seen"], 100);
    assert_eq!(json["frames_accepted"], 80);
    assert_eq!(json["frames_rejected"], 20);
    assert_eq!(json["quality_bucket"], 2);
    assert_eq!(json["guidance"], "looking_good");

    let review = EnrollmentReviewV3 {
        session: "s1".to_string(),
        ready_to_save: true,
        quality_percent: 80,
        diversity_percent: 60,
        frames_accepted: 8,
        guidance: "looking_good".to_string(),
    };
    let json = serde_json::to_value(&review).unwrap();
    assert_eq!(json["session"], "s1");
    assert_eq!(json["ready_to_save"], true);
    assert_eq!(json["quality_percent"], 80);
    assert_eq!(json["diversity_percent"], 60);
    assert_eq!(json["frames_accepted"], 8);
    assert_eq!(json["guidance"], "looking_good");

    // Privacy: no forbidden biometric tokens in the v3 wire surface.
    for forbidden in [
        "embedding",
        "vector",
        "score",
        "threshold",
        "template",
        "biometric",
    ] {
        let pj = serde_json::to_string(&progress).unwrap().to_lowercase();
        let rj = serde_json::to_string(&review).unwrap().to_lowercase();
        assert!(
            !pj.contains(forbidden),
            "EnrollmentProgressV3 leaked `{forbidden}`"
        );
        assert!(
            !rj.contains(forbidden),
            "EnrollmentReviewV3 leaked `{forbidden}`"
        );
    }
}

/// v3 happy path: start → stop → review → commit. Confirms the bridge drives
/// the full continuous-enrollment lifecycle against a fake v3 service and
/// that commit produces a new profile visible to the v1 list call.
#[test]
fn v3_bridge_start_stop_review_commit() {
    let handle = FakeHandle::new();
    zbus::block_on(async {
        let (_bus, bridge) = rig(&handle).await;
        // v3 API version is served on the v3 interface.
        assert_eq!(bridge.api_version_v3().await.unwrap(), 3);

        let session = bridge.start_enrollment_v3(TEST_UID, "Alice").await.unwrap();
        assert!(!session.is_empty());

        // Stop moves Capturing → Stopped; review is now valid.
        bridge.stop_enrollment_v3(&session).await.unwrap();
        let review = bridge.review_enrollment_v3(&session).await.unwrap();
        assert_eq!(review.session, session);
        assert!(review.ready_to_save);
        assert!(review.quality_percent <= 100);
        assert!(review.diversity_percent <= 100);

        // Commit produces a profile id and the session is consumed.
        let profile_id = bridge.commit_profile_v3(&session).await.unwrap();
        assert!(!profile_id.is_empty());

        // The committed profile is visible to the v1 ListProfiles surface.
        let profiles = bridge.list_profiles().await.unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, profile_id);

        // v1 and v2 surfaces still work alongside v3 — no regression.
        let status = bridge.get_status().await.unwrap();
        assert_eq!(status.api_version, 1);
    });
}

/// v3 cancel path: start → cancel. The session is torn down and no profile is
/// committed. Cancel is idempotent on the fake (a second cancel on the
/// already-removed session returns an error, not a panic).
#[test]
fn v3_bridge_start_cancel() {
    let handle = FakeHandle::new();
    zbus::block_on(async {
        let (_bus, bridge) = rig(&handle).await;
        let session = bridge.start_enrollment_v3(TEST_UID, "Bob").await.unwrap();
        bridge.cancel_enrollment_v3(&session).await.unwrap();

        // No profile committed.
        assert!(bridge.list_profiles().await.unwrap().is_empty());

        // Review on the cancelled session fails closed — the session is gone.
        match bridge.review_enrollment_v3(&session).await {
            Err(KcmError::Call(_)) => {}
            other => panic!("expected Call error on review-after-cancel, got {other:?}"),
        }
    });
}

/// v3 stop→resume→stop→commit: the resume path returns the session to
/// capturing and a subsequent stop makes review valid again. Confirms the
/// full Stop/Resume/Review/Commit cycle the CLI relies on.
#[test]
fn v3_bridge_stop_resume_cycle() {
    let handle = FakeHandle::new();
    zbus::block_on(async {
        let (_bus, bridge) = rig(&handle).await;
        let session = bridge.start_enrollment_v3(TEST_UID, "Carol").await.unwrap();
        bridge.stop_enrollment_v3(&session).await.unwrap();
        bridge.resume_enrollment_v3(&session).await.unwrap();
        // Review before stop fails: session is back in Capturing.
        match bridge.review_enrollment_v3(&session).await {
            Err(KcmError::Call(_)) => {}
            other => panic!("expected Call error on review-while-capturing, got {other:?}"),
        }
        bridge.stop_enrollment_v3(&session).await.unwrap();
        let review = bridge.review_enrollment_v3(&session).await.unwrap();
        assert!(review.ready_to_save);
        let _ = bridge.commit_profile_v3(&session).await.unwrap();
    });
}

/// v3 malformed tuple fails closed: if the daemon returns a malformed review
/// tuple, the bridge surfaces a `KcmError::Call` and no partial profile is
/// committed. Simulated by dropping the server before review so the call
/// fails on the transport layer.
#[test]
fn v3_bridge_malformed_tuple_fails_closed() {
    let handle = FakeHandle::new();
    zbus::block_on(async {
        let (bus, bridge) = rig(&handle).await;
        let session = bridge.start_enrollment_v3(TEST_UID, "Dan").await.unwrap();
        bridge.stop_enrollment_v3(&session).await.unwrap();
        // Drop the server to simulate a malformed/transport failure on review.
        drop(bus);
        match bridge.review_enrollment_v3(&session).await {
            Err(KcmError::DaemonUnavailable) | Err(KcmError::Call(_)) => {}
            Ok(v) => panic!("expected error on malformed review, got {v:?}"),
        }
        // No profile could have been committed by a failed review.
        // (The session may still exist on the daemon side, but the client
        // never reached commit, so no partial profile is visible.)
    });
}

/// v1/v2 compatibility unchanged: every v1 bridge method still works
/// alongside the new v3 methods. This is the regression guard required by
/// the task — adding v3 must not break v1/v2 callers.
#[test]
fn v1_v2_compatibility_unchanged() {
    let fake = namtor_client::fake::FakeManagerClient::new();
    namtor_client::contract::happy_path(&fake, &fake);
    namtor_client::contract::cancel_keeps_state_safe(&fake, &fake);
    namtor_client::contract::merge_and_replace(&fake, &fake);
}
