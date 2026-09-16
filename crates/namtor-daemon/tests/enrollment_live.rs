// SPDX-License-Identifier: GPL-3.0-only
//! Hardware-gated enrollment checks against the reference IR camera.
//!
//! Both tests are `#[ignore]`d and additionally no-op unless `NAMTOR_HW_TEST=1`
//! is set, so a machine without the reference camera never fails CI.
//!
//! Run with:
//! ```text
//! NAMTOR_HW_TEST=1 cargo test -p namtor-daemon --test enrollment_live -- --ignored --nocapture
//! ```
//!
//! [`live_enrollment_session_accepts_frames_from_the_reference_camera`]
//! additionally needs real YuNet + SFace weights; point `NAMTOR_MODELS_DIR` at
//! a directory holding `yunet_2023mar.onnx` and `sface_2021dec.onnx`.

use std::path::{Path, PathBuf};

use namtor_capture::frame::PixFmt;
use namtor_capture::ir_uvc::{IrUvc, ParityPolicy, V4l2IrStream};
use namtor_capture::source::{CaptureSource, SourceCaps, Spectrum};
use namtor_daemon::manager::embed::PipelineEmbedSource;
use namtor_daemon::manager::observation::{derive_guidance, observation_from};
use namtor_daemon::manager::session::EnrollmentCapture;

/// The reference camera: Sonix 3277:0018, IR node at `/dev/video2`, GREY 8-bit
/// 640x360@30, metadata node at `/dev/video3`.
const IR_NODE: &str = "/dev/video2";
const META_NODE: &str = "/dev/video3";
const IR_W: u32 = 640;
const IR_H: u32 = 360;

fn hw_enabled() -> bool {
    if std::env::var("NAMTOR_HW_TEST").is_err() {
        eprintln!("NAMTOR_HW_TEST not set; skipping");
        return false;
    }
    true
}

fn open_reference_camera() -> Box<dyn CaptureSource> {
    let streamer = V4l2IrStream::open(Path::new(IR_NODE), Some(Path::new(META_NODE)), IR_W, IR_H)
        .expect("open the reference IR node");
    let caps = SourceCaps {
        spectrum: Spectrum::Ir,
        format: PixFmt::Grey8,
        width: IR_W,
        height: IR_H,
        fps: 30,
        has_metadata: true,
    };
    let mut source = IrUvc::new(streamer, ParityPolicy, caps);
    source.start().expect("start the IR stream");
    Box::new(source)
}

/// `yunet_2023mar.onnx` + `sface_2021dec.onnx`, or `None` when the weights
/// are not on this machine.
fn model_paths() -> Option<(PathBuf, PathBuf)> {
    let root = PathBuf::from(
        std::env::var("NAMTOR_MODELS_DIR").unwrap_or_else(|_| "/var/lib/namtor/models".into()),
    );
    let yunet = root.join("yunet_2023mar.onnx");
    let sface = root.join("sface_2021dec.onnx");
    // The test fixtures elsewhere in the tree write tiny placeholder files
    // under the same names; a real YuNet graph is ~230 KiB.
    let real = |p: &Path| {
        std::fs::metadata(p)
            .map(|m| m.len() > 64 * 1024)
            .unwrap_or(false)
    };
    if real(&yunet) && real(&sface) {
        Some((yunet, sface))
    } else {
        eprintln!(
            "no real ONNX weights under {} (set NAMTOR_MODELS_DIR); skipping",
            root.display()
        );
        None
    }
}

/// The whole point of task 3.6: a real camera plus a real pipeline must
/// actually fill a reservoir and produce varying guidance. Before it,
/// `start_enrollment` passed `None, None` and the first `pump()` failed with
/// "no capture source for session".
///
/// Two preconditions this test cannot arrange for itself:
///
/// * **A face in front of the camera.** Without one every frame is a counted
///   `rejected_no_face` and `frames_accepted` stays 0.
/// * **An armed IR emitter.** Emitter activation lives in the daemon binary
///   (`src/bin/namtor-daemon/live_capture.rs`), behind the UVC XU and the
///   quirks table, not in this crate's library surface. Pulling frames
///   straight off `/dev/video2` with the emitter dark yields a near-black
///   buffer (mean intensity ~1 of 255), which YuNet will not detect a face
///   in. Run this with the daemon holding the emitter on, or expect the
///   `frames_accepted > 0` assertion to be what fails.
#[test]
#[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
fn live_enrollment_session_accepts_frames_from_the_reference_camera() {
    if !hw_enabled() {
        return;
    }
    let Some((yunet, sface)) = model_paths() else {
        return;
    };
    let embed = PipelineEmbedSource::load(&yunet, &sface).expect("load recognition pipeline");
    let mut capture = EnrollmentCapture::new(
        1000,
        1000,
        ":1.hw".into(),
        "hw".into(),
        Some(open_reference_camera()),
        Some(Box::new(embed)),
        64,
    );

    let mut guidance_seen = std::collections::HashSet::new();
    for i in 0..120 {
        let snapshot = match capture.pump() {
            Ok(Some(s)) => s,
            Ok(None) => break,
            Err(e) => panic!("pump failed on frame {i}: {e}"),
        };
        if let Some(m) = snapshot.metrics {
            let obs = observation_from(&m, capture.diversity_percent());
            let g = derive_guidance(&obs);
            guidance_seen.insert(format!("{g:?}"));
            if i % 20 == 0 {
                eprintln!(
                    "frame {i:3}: conf={:.2} blur={:.2} illum={:.2} sat={:.2} \
                     yaw={:+.1} roll={:+.1} face={:.3} spread={:.2} -> {g:?}",
                    m.detection_confidence,
                    m.blur,
                    m.illumination,
                    m.saturated_fraction,
                    m.yaw_deg,
                    m.roll_deg,
                    m.face_fraction,
                    m.landmark_spread,
                );
            }
        }
    }

    let snapshot = capture.snapshot();
    eprintln!(
        "seen={} accepted={} rejected={} (no_face={} below_floor={} torn={} embed_err={}) \
         diversity={}% guidance={guidance_seen:?}",
        snapshot.frames_captured,
        snapshot.frames_accepted,
        snapshot.frames_rejected,
        snapshot.rejected_no_face,
        snapshot.rejected_below_floor,
        snapshot.rejected_torn,
        snapshot.rejected_embed_error,
        capture.diversity_percent(),
    );
    assert!(snapshot.frames_captured > 0, "no frames pulled");
    assert_eq!(
        snapshot.frames_captured,
        snapshot.frames_accepted + snapshot.frames_rejected,
        "every seen frame must be accounted for"
    );
    assert!(
        snapshot.frames_accepted > 0,
        "no frame was accepted; is a face in front of {IR_NODE}?"
    );
}

/// Camera-only: no model weights needed. Pulls real IR frames and measures
/// them against a full-frame detection, which is enough to show the
/// pixel-derived metrics behave on real sensor data rather than on synthetic
/// fills.
#[test]
#[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
fn live_ir_frames_produce_plausible_pixel_metrics() {
    if !hw_enabled() {
        return;
    }
    use namtor_recog::detect::{Detection, FaceBox, Landmarks};

    let mut source = open_reference_camera();
    let whole_frame = Detection {
        box_: FaceBox {
            x: 0.0,
            y: 0.0,
            w: IR_W as f32,
            h: IR_H as f32,
            score: 1.0,
        },
        landmarks: Landmarks {
            right_eye: (IR_W as f32 * 0.4, IR_H as f32 * 0.4),
            left_eye: (IR_W as f32 * 0.6, IR_H as f32 * 0.4),
            nose: (IR_W as f32 * 0.5, IR_H as f32 * 0.55),
            right_mouth: (IR_W as f32 * 0.45, IR_H as f32 * 0.7),
            left_mouth: (IR_W as f32 * 0.55, IR_H as f32 * 0.7),
        },
    };

    let mut measured = 0;
    for i in 0..30 {
        let Some(frame) = source.next_frame().expect("next_frame") else {
            break;
        };
        assert_eq!(frame.format, PixFmt::Grey8);
        assert_eq!(frame.data.len(), (IR_W * IR_H) as usize);
        let m = namtor_recog::quality::measure(&frame.data, IR_W, IR_H, &whole_frame);
        assert!((0.0..=1.0).contains(&m.blur), "blur out of range: {m:?}");
        assert!(
            (0.0..=1.0).contains(&m.illumination),
            "illumination out of range: {m:?}"
        );
        assert!(
            (0.0..=1.0).contains(&m.saturated_fraction),
            "saturated_fraction out of range: {m:?}"
        );
        if i % 10 == 0 {
            let mean =
                frame.data.iter().map(|&b| b as u64).sum::<u64>() as f32 / frame.data.len() as f32;
            eprintln!(
                "frame {i:2} emitter={:?} mean={mean:6.2} blur={:.3} illum={:.3} sat={:.3}",
                frame.emitter, m.blur, m.illumination, m.saturated_fraction
            );
        }
        measured += 1;
    }
    source.stop();
    assert!(measured > 0, "no frames pulled from {IR_NODE}");
}
