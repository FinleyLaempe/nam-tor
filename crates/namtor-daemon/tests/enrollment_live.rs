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
//! additionally needs real YuNet + SFace weights and an armed IR emitter. The
//! weights are located exactly the way the daemon locates them — see
//! [`model_dir_candidates`] — so `NAMTOR_MODELS_DIR` overrides them here for
//! the same reason it does there, and an installed system and a developer
//! checkout both work with no arguments.
//!
//! Every skip below names the thing it could not find, and where it looked.
//! Review finding F13: this test used to skip with "no real ONNX weights on
//! this machine" while the weights sat in the repo-relative model directory
//! all along, because it probed one hardcoded system path. A skip whose
//! stated reason is false hides a runnable test, which is worse than a
//! failure.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use namtor_capture::frame::PixFmt;
use namtor_capture::ir_uvc::{IrUvc, ParityPolicy, V4l2IrStream};
use namtor_capture::source::{CaptureSource, SourceCaps, Spectrum};
use namtor_daemon::manager::embed::PipelineEmbedSource;
use namtor_daemon::manager::observation::{derive_guidance, observation_from};
use namtor_daemon::manager::session::EnrollmentCapture;
use namtor_layout::{InstallRoot, LayoutPaths};

/// The reference camera: Sonix 3277:0018, IR node at `/dev/video2`, GREY 8-bit
/// 640x360@30, metadata node at `/dev/video3`.
const IR_NODE: &str = "/dev/video2";
const META_NODE: &str = "/dev/video3";
const IR_W: u32 = 640;
const IR_H: u32 = 360;

/// The install manifest an installed system ships, and the one in a source
/// checkout. `LayoutPaths::resolve` falls back to its declared defaults when
/// the file is unreadable, so passing a path that does not exist is safe.
const INSTALLED_MANIFEST: &str = "/usr/share/namtor/install-manifest.toml";
const SOURCE_MANIFEST_REL: &str = "packaging/install-manifest.toml";

/// Smallest file the test will accept as a real ONNX graph. Fixtures
/// elsewhere in the tree write placeholder files under the same names; a real
/// YuNet graph is ~230 KiB and SFace is ~37 MiB.
const MIN_WEIGHT_BYTES: u64 = 64 * 1024;

/// Frames pulled before the session starts, purely to read the scene's
/// brightness and decide whether the IR emitter is on.
const EMITTER_PROBE_FRAMES: usize = 5;

/// Mean sample value below which the IR sensor is judged to be looking at an
/// unlit scene, i.e. the emitter is dark.
///
/// Measured on the reference camera with the emitter unarmed: mean 0.1 of
/// 255, min 0, max 4 over five 640x360 GREY8 frames. Any actually illuminated
/// IR scene sits far above this; the floor only has to separate "black" from
/// "not black".
const EMITTER_DARK_MEAN: f32 = 4.0;

/// Serialises access to the reference camera across this binary's tests.
///
/// `/dev/video2` is a single exclusive V4L2 node: `VIDIOC_REQBUFS` fails with
/// EBUSY while another stream holds it. Both tests here open it and cargo runs
/// them on separate threads, so without this they race. That race was latent
/// until review finding F13 was fixed — the enrollment test used to bail out
/// at the (false) model check before it ever reached the camera, so the two
/// never overlapped.
///
/// The poison is deliberately discarded: a panicking sibling must not make the
/// other test fail for a reason that has nothing to do with it.
static CAMERA: Mutex<()> = Mutex::new(());

fn lock_camera() -> MutexGuard<'static, ()> {
    CAMERA
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

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

/// The repo root, from this crate's manifest directory.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Every model directory the daemon would consider, in its order of
/// precedence, each tagged with where it came from so a skip can say so.
///
/// This mirrors `src/bin/namtor-daemon.rs`: `NAMTOR_MODELS_DIR` wins,
/// otherwise `LayoutPaths::models_dir`. The daemon only ever resolves an
/// installed root, because that is the only shape it runs in; a test also has
/// to work in a developer checkout, where the weights live under the
/// repo-relative `var/lib/namtor/models/` that `AGENTS.md` documents, so the
/// source root is consulted too.
fn model_dir_candidates() -> Vec<(&'static str, PathBuf)> {
    let mut out = Vec::new();
    if let Some(dir) = std::env::var_os("NAMTOR_MODELS_DIR") {
        out.push(("NAMTOR_MODELS_DIR", PathBuf::from(dir)));
    }
    out.push((
        "installed layout",
        LayoutPaths::resolve(
            Path::new("/"),
            Path::new(INSTALLED_MANIFEST),
            InstallRoot::Installed,
        )
        .models_dir,
    ));
    let repo = repo_root();
    let source_manifest = repo.join(SOURCE_MANIFEST_REL);
    out.push((
        "source layout",
        LayoutPaths::resolve(&repo, &source_manifest, InstallRoot::Source).models_dir,
    ));
    out
}

/// How a candidate weight file looks on disk, for the skip message.
fn describe_weight(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(m) if m.len() >= MIN_WEIGHT_BYTES => format!("{} B", m.len()),
        Ok(m) => format!("{} B (below the {MIN_WEIGHT_BYTES} B floor)", m.len()),
        Err(e) => format!("unreadable ({})", e.kind()),
    }
}

/// `yunet_2023mar.onnx` + `sface_2021dec.onnx`, or `None` when no candidate
/// directory holds both.
///
/// On `None` the message lists **every directory searched** and what was
/// found in it, so the next reader can tell a missing download from a
/// misconfigured path without re-deriving the search order.
fn model_paths() -> Option<(PathBuf, PathBuf)> {
    let mut searched = Vec::new();
    for (origin, dir) in model_dir_candidates() {
        let yunet = dir.join("yunet_2023mar.onnx");
        let sface = dir.join("sface_2021dec.onnx");
        let big = |p: &Path| {
            std::fs::metadata(p)
                .map(|m| m.len() >= MIN_WEIGHT_BYTES)
                .unwrap_or(false)
        };
        if big(&yunet) && big(&sface) {
            eprintln!(
                "models: {} (via {origin}); yunet {}, sface {}",
                dir.display(),
                describe_weight(&yunet),
                describe_weight(&sface),
            );
            return Some((yunet, sface));
        }
        searched.push(format!(
            "  {} (via {origin})\n    yunet_2023mar.onnx: {}\n    sface_2021dec.onnx: {}",
            dir.display(),
            describe_weight(&yunet),
            describe_weight(&sface),
        ));
    }
    eprintln!(
        "skipping: no directory searched holds both real ONNX weights \
         (>= {MIN_WEIGHT_BYTES} B each). Searched, in order:\n{}\n\
         Set NAMTOR_MODELS_DIR to override.",
        searched.join("\n")
    );
    None
}

/// Brightest per-frame mean sample value over up to `frames` frames, or `0.0`
/// when nothing could be pulled.
///
/// This is the emitter probe: an IR sensor with its emitter dark returns an
/// essentially black buffer regardless of what is in front of it, and the
/// brightest of several frames is a cheap, honest way to tell that apart from
/// an illuminated scene without needing the XU control this crate cannot
/// reach.
///
/// It reads **pixels**, not `Frame::emitter`. Measured on the reference
/// camera: the parity policy labels frames `EmitterState::On` while the image
/// is uniformly black (mean 0.10 of 255). The metadata flag reports which side
/// of the alternation a frame came from, not whether the emitter is actually
/// lit, so trusting it here would reintroduce exactly the class of false
/// signal F13 is about.
fn brightest_mean(source: &mut dyn CaptureSource, frames: usize) -> f32 {
    let mut best = 0.0f32;
    for _ in 0..frames {
        match source.next_frame() {
            Ok(Some(frame)) if !frame.data.is_empty() => {
                let mean = frame.data.iter().map(|&b| u64::from(b)).sum::<u64>() as f32
                    / frame.data.len() as f32;
                best = best.max(mean);
            }
            _ => break,
        }
    }
    best
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
///   buffer, which YuNet will not detect a face in.
///
/// The second one is *checked*, not assumed: [`brightest_mean`] reads the
/// scene before the session starts and the test skips with the measurement in
/// hand when the sensor is dark. Review finding F13/F14 — asserting
/// `frames_accepted > 0` against a black frame produces a failure that blames
/// the pipeline for a missing emitter.
#[test]
#[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
fn live_enrollment_session_accepts_frames_from_the_reference_camera() {
    if !hw_enabled() {
        return;
    }
    let _camera = lock_camera();
    let Some((yunet, sface)) = model_paths() else {
        return;
    };

    let mut source = open_reference_camera();
    let mean = brightest_mean(source.as_mut(), EMITTER_PROBE_FRAMES);
    if mean < EMITTER_DARK_MEAN {
        source.stop();
        eprintln!(
            "skipping: {IR_NODE} returns an unlit image — brightest frame mean {mean:.2}/255 \
             over {EMITTER_PROBE_FRAMES} frames, below the {EMITTER_DARK_MEAN:.1} floor. The IR \
             emitter is not armed, so no detector can find a face here and every frame would \
             be counted rejected_no_face. Arming it needs the UVC extension unit and the quirks \
             table in src/bin/namtor-daemon/live_capture.rs, which runs inside namtor-daemon as \
             root; this test links the library only. Run it with namtor-daemon holding the \
             emitter on (owner-gated item O1). Models were found, so this is the only \
             remaining blocker."
        );
        return;
    }
    eprintln!("emitter: scene is lit (brightest frame mean {mean:.2}/255)");

    let embed = PipelineEmbedSource::load(&yunet, &sface).expect("load recognition pipeline");
    let mut capture = EnrollmentCapture::new(
        1000,
        1000,
        ":1.hw".into(),
        "hw".into(),
        Some(source),
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
    let _camera = lock_camera();
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
