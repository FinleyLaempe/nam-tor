// SPDX-License-Identifier: GPL-3.0-only
//! A camera held by another program must reach the user as "Camera is busy",
//! not as an ioctl name.
//!
//! The §8.3 classifier (`failure_of`) reads the condition out of the reason
//! string the capture layer produced. That is a contract between two crates
//! with no compiler to enforce it, so it is pinned here: the real
//! `V4l2Error::Busy` / `CaptureError::Busy` text — not a hand-written
//! approximation of it — must classify as `FailureReason::CameraBusy`.
//!
//! This is the failure a working machine actually hits: a video call or a
//! camera app holds `/dev/videoN` and the driver answers EBUSY.

#![cfg(target_os = "linux")]

use namtor_capture::source::CaptureError;
use namtor_capture::v4l2::V4l2Error;
use namtor_daemon::verify::{failure_of, VerifyOutcome};
use namtor_proto::pam::FailureReason;

/// Every ioctl that can answer EBUSY on a busy node, as the capture layer
/// names them.
const BUSY_STAGES: &[&str] = &["open", "VIDIOC_S_FMT", "VIDIOC_REQBUFS", "VIDIOC_STREAMON"];

#[test]
fn ebusy_becomes_a_typed_busy_error_not_a_generic_device_error() {
    for stage in BUSY_STAGES {
        let e: CaptureError = V4l2Error::Busy((*stage).to_string()).into();
        assert!(
            matches!(e, CaptureError::Busy(_)),
            "{stage}: EBUSY must stay typed, got {e:?}"
        );
    }
}

#[test]
fn a_non_busy_ioctl_failure_stays_a_device_error() {
    let e: CaptureError = V4l2Error::Ioctl("VIDIOC_S_FMT: EINVAL".into()).into();
    assert!(
        matches!(e, CaptureError::Device(_)),
        "only EBUSY may claim the busy condition, got {e:?}"
    );
}

#[test]
fn the_real_busy_text_classifies_as_camera_busy() {
    for stage in BUSY_STAGES {
        let capture: CaptureError = V4l2Error::Busy((*stage).to_string()).into();
        let reason = capture.to_string();
        let outcome = VerifyOutcome::unavailable(&reason);
        assert_eq!(
            failure_of(&outcome),
            Some(FailureReason::CameraBusy),
            "the capture layer's own wording for {stage} ({reason:?}) must \
             reach the user as 'Camera is busy'"
        );
    }
}

#[test]
fn camera_busy_shows_the_user_a_message() {
    assert_eq!(
        FailureReason::CameraBusy.user_message(),
        Some("Camera is busy"),
        "a busy camera is a §8.3 condition the user is told about"
    );
}

/// The classifier keys off wording, so a capture error that merely mentions
/// another device condition must not be mistaken for a busy camera.
#[test]
fn an_unrelated_device_error_is_not_reported_as_busy() {
    let capture: CaptureError = V4l2Error::NegotiationRefused.into();
    let outcome = VerifyOutcome::unavailable(&capture.to_string());
    assert_ne!(failure_of(&outcome), Some(FailureReason::CameraBusy));
}
