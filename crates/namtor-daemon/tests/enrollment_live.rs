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
//! The emitter this test **arms itself**, through the shipped
//! `namtor_camera::emitter_ctrl` + `namtor_camera::watchdog` API, bounded by
//! the device's own `max_on_ms` and restored on every exit path including
//! panic. See [`hw_emitter`] for the safety argument. It used to skip here,
//! claiming activation needed root inside the daemon; the quirks entry's
//! measured `[access] requires_root = false` says otherwise, and it does.
//!
//! Every skip below names the thing it could not find, and where it looked.
//! Review finding F13: this test used to skip with "no real ONNX weights on
//! this machine" while the weights sat in the repo-relative model directory
//! all along, because it probed one hardcoded system path. A skip whose
//! stated reason is false hides a runnable test, which is worse than a
//! failure.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use namtor_camera::watchdog::SystemClock;
use namtor_hwdb::quirks::EmitterCfg;

use namtor_capture::frame::PixFmt;
use namtor_capture::ir_uvc::{IrUvc, ParityPolicy, V4l2IrStream};
use namtor_capture::source::{CaptureSource, SourceCaps, Spectrum};
use namtor_daemon::manager::embed::PipelineEmbedSource;
use namtor_daemon::manager::observation::{derive_guidance, observation_from};
use namtor_daemon::manager::session::EnrollmentCapture;
use namtor_daemon::verify::VerifyEngine;
use namtor_layout::{InstallRoot, LayoutPaths};
use namtor_proto::pam::Verdict;
use namtor_store::{EnrollmentMeta, TemplateRecord, TemplateStore};

mod hw_emitter;

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

/// Mean sample value a genuinely armed emitter must clear, measured the same
/// way as [`EMITTER_DARK_MEAN`].
///
/// `quirks/devices/3277-0018.toml` measured mode 2 at mean 41.0 across the
/// alternation and 81.9 on the illuminated half; [`brightest_mean`] reads the
/// illuminated half, and reads it in the first few frames, before the
/// firmware AE has decayed — quirks records the first pair at 217.2, and this
/// test observed 253.0 with a subject close to the lens. The floor therefore
/// sits far below every one of those numbers, because its only job is to
/// separate "the LED came on" from "the write was accepted but nothing
/// happened" — a distinction a GET_CUR readback alone cannot make.
const EMITTER_LIT_MEAN: f32 = 20.0;

/// Upper bound on frames pumped through the enrollment session. The real
/// bound is the emitter watchdog; this only stops a fast camera from running
/// on after the watchdog has already shut the LED off.
const MAX_PUMP_FRAMES: usize = 120;

/// How many independent teardown timings gate G1.5 is measured over. A single
/// sample is not evidence; the result is reported as every sample, plus the
/// median and the max.
const TEARDOWN_SAMPLES: usize = 5;

/// Gate G1.5's budget: the emitter must be dark within this many milliseconds
/// of teardown being issued.
const TEARDOWN_BUDGET_MS: f64 = 200.0;

/// Upper bound on frames pulled after arming while waiting for the strobe to
/// light the scene. The real bound is the device's own `max_on_ms` watchdog;
/// this only stops a silent camera from spinning.
const TEARDOWN_WARMUP_FRAMES: usize = 20;

/// How long frames keep being sampled after `force_off` is issued. Six times
/// the budget, so a teardown that overruns it is *measured* rather than
/// truncated into an unhelpful "never went dark".
const TEARDOWN_WINDOW_MS: f64 = 1200.0;

/// Consecutive sub-floor frames required before the scene counts as dark.
///
/// Mode 2 is a strict 1:1 firmware strobe — `quirks/devices/3277-0018.toml`
/// records `illuminated_mean = 81.9` against `clean_mean = 0.13` — so **half
/// of all frames read below the dark floor while the emitter is fully armed**.
/// A single sub-floor frame therefore proves nothing at all. Darkness is only
/// called when a frame and every frame after it to the end of the window are
/// sub-floor, and that run is at least this long: four alternation pairs.
const TEARDOWN_DARK_CONFIRM_FRAMES: usize = 8;

/// Installed quirks directory, mirroring `LayoutPaths::quirks_dirs`.
const INSTALLED_QUIRKS_DIR: &str = "/usr/share/namtor/quirks/devices";

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

/// Every quirks directory to search for the reference device's emitter
/// configuration, in precedence order.
///
/// `LayoutPaths::quirks_dirs` carries the source entry as the *relative*
/// `quirks/devices`, which resolves only for a process started at the repo
/// root; `cargo test` starts in the crate directory. The repo-relative path
/// is therefore built from `CARGO_MANIFEST_DIR` instead, exactly as
/// [`model_dir_candidates`] does for weights.
fn quirks_dir_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = std::env::var_os("NAMTOR_QUIRKS_DIR") {
        out.push(PathBuf::from(dir));
    }
    out.push(PathBuf::from(INSTALLED_QUIRKS_DIR));
    out.push(repo_root().join("quirks").join("devices"));
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

/// Mean sample value of one frame, on the sensor's own 0..=255 scale.
///
/// Empty buffers score `0.0` rather than dividing by zero: a frame with no
/// pixels carries no evidence that anything is lit.
fn frame_mean(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    data.iter().map(|&b| u64::from(b)).sum::<u64>() as f32 / data.len() as f32
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
                best = best.max(frame_mean(&frame.data));
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
/// Two preconditions. The test arranges one and can only check the other:
///
/// * **An armed IR emitter** — *arranged*. `/dev/video2` returns an
///   essentially black buffer with the emitter dark (measured mean 0.1/255),
///   and YuNet finds nothing in black. The emitter is armed here, from this
///   process, as this user: the reference device's quirks entry records
///   `[access] requires_root = false` as a measured fact, and the seat ACL on
///   the node gives the desktop user `rw`. Everything about *how* — RAII
///   force-off, start-up reconciliation, `max_on_ms` watchdog — lives in
///   [`hw_emitter`].
/// * **A face in front of the camera** — *checked only*. Nothing this process
///   can do puts one there. Without one every frame is a counted
///   `rejected_no_face` and the final assertion fails, saying so.
///
/// Both emitter states are measured in **photons**, not just in GET_CUR
/// bytes: dark before arming, lit after, dark again after teardown. Review
/// finding F13/F14 — a readback proves the device accepted a write, not that
/// an LED came on, and an assertion that trusts the former reintroduces
/// exactly the class of false signal F13 is about.
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

    let quirks_dirs = quirks_dir_candidates();
    let Some((quirks_dir, emitter_cfg)) =
        hw_emitter::emitter_cfg(&quirks_dirs, hw_emitter::REFERENCE_VID_PID)
    else {
        eprintln!(
            "skipping: no quirks entry for {} in any of {:?}. Without it there is no emitter \
             control to write, and {IR_NODE} stays dark.",
            hw_emitter::REFERENCE_VID_PID,
            quirks_dirs,
        );
        return;
    };
    eprintln!(
        "quirks: {} (via {}); emitter method {:?} unit {:?} selector {:?} \
         payload_on {:02x?} payload_off {:02x?}",
        hw_emitter::REFERENCE_VID_PID,
        quirks_dir.display(),
        emitter_cfg.method,
        emitter_cfg.unit,
        emitter_cfg.selector,
        emitter_cfg.payload_on,
        emitter_cfg.payload_off,
    );

    let mut source = open_reference_camera();
    let dark_before = brightest_mean(source.as_mut(), EMITTER_PROBE_FRAMES);
    eprintln!(
        "emitter disarmed: brightest frame mean {dark_before:.2}/255 over \
         {EMITTER_PROBE_FRAMES} frames"
    );

    // Loading SFace reads ~37 MiB off disk and builds a graph. Do it *before*
    // arming, so that cost is not spent with the LED lit.
    let embed = PipelineEmbedSource::load(&yunet, &sface).expect("load recognition pipeline");

    let budget_ms = hw_emitter::max_on_ms(&emitter_cfg);
    let clock = SystemClock;

    // Everything inside this block runs with the emitter potentially lit. The
    // block exists so the guard, and the capture session holding the V4L2
    // node, are both dropped before the post-conditions below are measured.
    let outcome = {
        let mut armed = match hw_emitter::ArmedEmitter::arm(Path::new(IR_NODE), &emitter_cfg) {
            Ok(a) => a,
            Err(e) => {
                source.stop();
                eprintln!(
                    "skipping: could not arm the IR emitter on {IR_NODE}: {e}\n\
                     The quirks entry claims [access] requires_root = false. If the error above \
                     is EACCES or EPERM that claim is wrong for this kernel and this test needs \
                     namtor-daemon to hold the emitter on instead (owner-gated item O1). Any \
                     other errno is a device or configuration fault, not a privilege one. \
                     Models and quirks were both found, so this is the only remaining blocker."
                );
                return;
            }
        };
        let mut watchdog = hw_emitter::watchdog_for(&emitter_cfg, &clock);
        watchdog.arm();
        eprintln!(
            "emitter armed: GET_CUR read back {:02x?}; watchdog budget {budget_ms} ms",
            armed.readback(),
        );

        let lit = brightest_mean(source.as_mut(), EMITTER_PROBE_FRAMES);
        eprintln!(
            "emitter armed: brightest frame mean {lit:.2}/255 over {EMITTER_PROBE_FRAMES} frames"
        );

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
        let mut best_confidence = 0.0f32;
        let mut pumped = 0usize;
        let mut watchdog_fired = false;
        for i in 0..MAX_PUMP_FRAMES {
            if watchdog.check_expired() {
                // Spec §4.10.2: the emitter goes off the instant the budget
                // is spent, before anything else happens on this iteration.
                armed.force_off();
                watchdog.disarm();
                watchdog_fired = true;
                eprintln!(
                    "watchdog: {budget_ms} ms emitter budget spent after {i} pumped frames; \
                     emitter forced off, capture stopped"
                );
                break;
            }
            let snapshot = match capture.pump() {
                Ok(Some(s)) => s,
                Ok(None) => break,
                Err(e) => panic!("pump failed on frame {i}: {e}"),
            };
            pumped += 1;
            if let Some(m) = snapshot.metrics {
                best_confidence = best_confidence.max(m.detection_confidence);
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

        // Force off explicitly rather than waiting for `Drop`, so the LED is
        // dark while the reservoir is summarised and the node released.
        armed.force_off();
        let off_readback = armed.current();
        eprintln!(
            "emitter guard: is_armed={} after force_off (watchdog_fired={watchdog_fired})",
            armed.is_armed(),
        );
        let snapshot = capture.snapshot();
        let diversity = capture.diversity_percent();
        // Releases /dev/video2 (V4l2Stream::drop does STREAMOFF + munmap) so
        // the post-condition probe below can reopen it.
        drop(capture);

        Outcome {
            lit,
            snapshot,
            diversity,
            guidance_seen,
            best_confidence,
            pumped,
            watchdog_fired,
            off_readback,
        }
    };

    // --- Emitter post-conditions come first. A pipeline assertion that fires
    // --- below must not be able to hide the proof that the LED went out.
    match &outcome.off_readback {
        Ok(bytes) => eprintln!("emitter disarmed: GET_CUR read back {bytes:02x?}"),
        Err(e) => eprintln!("emitter disarmed: readback failed: {e}"),
    }
    let after = hw_emitter::read_control(Path::new(IR_NODE), &emitter_cfg);
    assert_eq!(
        after.as_deref().ok(),
        Some(emitter_cfg.payload_off.as_slice()),
        "the emitter control must read back payload_off after teardown; got {after:02x?}"
    );

    let mut source = open_reference_camera();
    let dark_after = brightest_mean(source.as_mut(), EMITTER_PROBE_FRAMES);
    source.stop();
    eprintln!(
        "emitter restored: brightest frame mean {dark_after:.2}/255 over \
         {EMITTER_PROBE_FRAMES} frames (was {:.2} armed, {dark_before:.2} before arming)",
        outcome.lit,
    );
    assert!(
        dark_after < EMITTER_DARK_MEAN,
        "emitter still lighting the scene after teardown: brightest frame mean \
         {dark_after:.2}/255 is at or above the {EMITTER_DARK_MEAN:.1} dark floor"
    );

    // --- Then: did arming actually do anything?
    assert!(
        outcome.lit >= EMITTER_LIT_MEAN,
        "the emitter control accepted payload_on but the scene stayed dark: brightest frame \
         mean {:.2}/255, below the {EMITTER_LIT_MEAN:.1} lit floor (quirks measured 41.0 mean / \
         81.9 illuminated for mode 2). The write succeeded and the LED did not come on.",
        outcome.lit,
    );

    // --- Then: the pipeline.
    let snapshot = &outcome.snapshot;
    eprintln!(
        "pumped={} watchdog_fired={} seen={} accepted={} rejected={} (no_face={} below_floor={} \
         torn={} embed_err={}) best_conf={:.3} diversity={}% guidance={:?}",
        outcome.pumped,
        outcome.watchdog_fired,
        snapshot.frames_captured,
        snapshot.frames_accepted,
        snapshot.frames_rejected,
        snapshot.rejected_no_face,
        snapshot.rejected_below_floor,
        snapshot.rejected_torn,
        snapshot.rejected_embed_error,
        outcome.best_confidence,
        outcome.diversity,
        outcome.guidance_seen,
    );
    assert!(snapshot.frames_captured > 0, "no frames pulled");
    assert_eq!(
        snapshot.frames_captured,
        snapshot.frames_accepted + snapshot.frames_rejected,
        "every seen frame must be accounted for"
    );
    assert!(
        snapshot.frames_accepted > 0,
        "no frame was accepted out of {} seen ({} rejected for no face, best detection \
         confidence {:.3}). The emitter WAS armed and lit (brightest frame mean {:.2}/255), so \
         this is not the emitter: is a face in front of {IR_NODE}?",
        snapshot.frames_captured,
        snapshot.rejected_no_face,
        outcome.best_confidence,
        outcome.lit,
    );
}

/// What the armed section produced, carried out of the block so the emitter
/// guard and the V4L2 node are released before any of it is asserted on.
struct Outcome {
    lit: f32,
    snapshot: namtor_daemon::manager::session::ProgressSnapshot,
    diversity: u32,
    guidance_seen: std::collections::HashSet<String>,
    best_confidence: f32,
    pumped: usize,
    watchdog_fired: bool,
    off_readback: Result<Vec<u8>, String>,
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

/// One timed emitter teardown.
struct TeardownSample {
    /// Brightest frame mean seen while armed, i.e. the proof that this sample
    /// timed the shutdown of an emitter that was genuinely lit.
    lit_mean: f32,
    /// Elapsed milliseconds from `force_off` being issued to the first frame
    /// of the sustained-dark run, or `None` when the scene never went
    /// sustainably dark inside the sampling window.
    dark_at_ms: Option<f64>,
    /// Elapsed milliseconds of the last frame at or above the dark floor.
    /// `None` when no frame after `force_off` was ever above it.
    last_lit_ms: Option<f64>,
    /// Milliseconds between the `payload_on` write returning and the first
    /// frame arriving afterwards. The counterpart of [`Self::first_frame_ms`]
    /// for the other direction of the same mode switch.
    arm_blind_ms: f64,
    /// Elapsed milliseconds at which the `payload_off` write returned. The
    /// emitter cannot be off before this.
    issue_ms: f64,
    /// Elapsed milliseconds at which the *first* frame after the force-off was
    /// dequeued. The sensor says nothing at all before this instant, so no
    /// measurement can resolve below it.
    first_frame_ms: f64,
    /// Frames dequeued during the sampling window.
    frames: usize,
    /// Length of the sustained-dark run at the end of the window.
    dark_tail: usize,
    /// Mean interval between dequeued frames over the window. This is the
    /// measurement's granularity, *measured* rather than assumed from the
    /// nominal 30 fps.
    frame_interval_ms: f64,
}

/// Arms nothing and opens nothing: times one teardown of an already-armed
/// emitter against an already-streaming source.
///
/// The caller owns the [`hw_emitter::ArmedEmitter`] guard, so every error path
/// here — including the ones that return early — still leaves the guard to
/// force `payload_off` on drop. `force_off` is additionally called explicitly
/// on each pre-measurement error path, so the LED goes out at the moment of
/// the fault rather than at the end of the caller's scope.
fn measure_teardown(
    source: &mut dyn CaptureSource,
    cfg: &EmitterCfg,
    clock: &SystemClock,
    armed: &mut hw_emitter::ArmedEmitter<'_>,
) -> Result<TeardownSample, String> {
    // The armed window is bounded by the device's own `[emitter.lifecycle]
    // max_on_ms`, exactly as the enrollment test bounds its pump loop.
    let mut watchdog = hw_emitter::watchdog_for(cfg, clock);
    watchdog.arm();

    // The caller armed the emitter immediately before this call, so this
    // clocks the blind window the *other* direction of the mode switch opens.
    let armed_at = Instant::now();
    let mut arm_blind_ms = f64::NAN;
    let mut lit = 0.0f32;
    let mut warmup = 0usize;
    while lit < EMITTER_LIT_MEAN && warmup < TEARDOWN_WARMUP_FRAMES {
        if watchdog.check_expired() {
            armed.force_off();
            watchdog.disarm();
            return Err(format!(
                "the {} ms emitter watchdog fired after {warmup} frames while waiting for the \
                 strobe to light the scene; brightest frame mean {lit:.2}/255",
                hw_emitter::max_on_ms(cfg),
            ));
        }
        match source.next_frame() {
            Ok(Some(frame)) => {
                if warmup == 0 {
                    arm_blind_ms = armed_at.elapsed().as_secs_f64() * 1000.0;
                }
                lit = lit.max(frame_mean(&frame.data));
                warmup += 1;
            }
            Ok(None) => {
                armed.force_off();
                watchdog.disarm();
                return Err("the stream ended while waiting for the strobe".into());
            }
            Err(e) => {
                armed.force_off();
                watchdog.disarm();
                return Err(format!("next_frame while waiting for the strobe: {e}"));
            }
        }
    }
    if lit < EMITTER_LIT_MEAN {
        armed.force_off();
        watchdog.disarm();
        return Err(format!(
            "the emitter control accepted payload_on but the scene stayed dark over {warmup} \
             frames: brightest mean {lit:.2}/255, below the {EMITTER_LIT_MEAN:.1} lit floor. \
             There is no teardown to time if nothing was ever lit."
        ));
    }

    // ---- The measured interval starts on the next line. ----
    // `t0` is taken immediately before the force-off is issued, so the ioctl's
    // own cost is inside the measurement rather than excluded from it.
    let t0 = Instant::now();
    armed.force_off();
    let issue_ms = t0.elapsed().as_secs_f64() * 1000.0;
    watchdog.disarm();

    // Sample frames continuously and timestamp each one at the moment
    // `next_frame` hands it over. `next_frame` blocks in VIDIOC_DQBUF, and the
    // per-frame work here is a 230 KiB sum, so the consumer keeps pace with
    // the stream and the MMAP queue stays shallow.
    let mut trace: Vec<(f64, f32)> = Vec::new();
    loop {
        let frame = match source.next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => return Err("the stream ended while timing the teardown".into()),
            Err(e) => return Err(format!("next_frame while timing the teardown: {e}")),
        };
        let at_ms = t0.elapsed().as_secs_f64() * 1000.0;
        trace.push((at_ms, frame_mean(&frame.data)));
        if at_ms >= TEARDOWN_WINDOW_MS {
            break;
        }
    }

    // Darkness is the *suffix* of the window, not the first sub-floor frame:
    // under the 1:1 strobe every other frame is sub-floor while the emitter is
    // fully on. See `TEARDOWN_DARK_CONFIRM_FRAMES`.
    let last_lit = trace.iter().rposition(|&(_, m)| m >= EMITTER_DARK_MEAN);
    let dark_idx = last_lit.map_or(0, |i| i + 1);
    let dark_tail = trace.len().saturating_sub(dark_idx);
    let dark_at_ms = if dark_idx < trace.len() && dark_tail >= TEARDOWN_DARK_CONFIRM_FRAMES {
        Some(trace[dark_idx].0)
    } else {
        None
    };
    let frame_interval_ms = if trace.len() > 1 {
        (trace[trace.len() - 1].0 - trace[0].0) / (trace.len() - 1) as f64
    } else {
        f64::NAN
    };

    Ok(TeardownSample {
        lit_mean: lit,
        arm_blind_ms,
        issue_ms,
        first_frame_ms: trace[0].0,
        dark_at_ms,
        last_lit_ms: last_lit.map(|i| trace[i].0),
        frames: trace.len(),
        dark_tail,
        frame_interval_ms,
    })
}

/// Gate **G1.5** — "emitter off ≤ 200 ms after teardown" — measured with a
/// real clock against the reference camera's own sensor.
///
/// # What is measured
///
/// For each of [`TEARDOWN_SAMPLES`] samples: arm the emitter, confirm in
/// **photons** that the scene is genuinely lit, take `t0`, issue
/// [`hw_emitter::ArmedEmitter::force_off`], then keep dequeuing frames and
/// timestamping each one at the instant it arrives. The reported interval is
/// `t0` → the first frame of the run of sub-floor frames that lasts to the end
/// of the window.
///
/// Darkness is the same objective criterion the rest of this file uses — frame
/// mean below the [`EMITTER_DARK_MEAN`] floor of 4.0/255 — but it is applied to
/// a *run* of frames, never to one frame. Mode 2 is a strict 1:1 firmware
/// strobe, so while the emitter is fully armed every other frame already reads
/// ~0.13/255. A "first frame under the floor" rule would time the next clean
/// half of the alternation and report a teardown that had not happened.
///
/// # Precision — read this before quoting the number
///
/// The stream is GREY8 640x360 at a nominal 30 fps, so frames arrive roughly
/// every 33 ms and **that is the sampling granularity**. The measured value is
/// quantised to frame arrivals and is not meaningful below one frame period.
/// The test prints the granularity it actually observed alongside every
/// sample; do not quote a digit the method does not support.
///
/// Two further caveats, both in the conservative direction:
///
/// * The timestamp is taken when `next_frame` returns, i.e. at dequeue, which
///   is later than the exposure it describes. The number is therefore an
///   **upper bound** on the interval to the LED going dark.
/// * Because the strobe alternates, a sustained-dark run starting at frame `k`
///   bounds the true off instant to no later than frame `k+1`'s exposure:
///   consecutive frames cover both phases, so a still-strobing emitter cannot
///   produce two dark frames in a row. The residual uncertainty is therefore
///   about ±1 frame period, ~33 ms.
///
/// # Measured result on the reference camera (2026-09-17)
///
/// The gate is **not met as measured**, and the reason is an observation
/// limit rather than a slow LED. Five samples:
///
/// ```text
/// payload_off write returned   40.0 .. 42.4 ms
/// no frames at all for a further        ~391 ms
/// first frame back, already dark       431.0 .. 435.1 ms   <- reported value
/// ```
///
/// The MSXU mode switch stalls the V4L2 stream, and it does so symmetrically:
/// arming (mode 1 -> 2) blinds it for 390.5 .. 393.2 ms by the same
/// measurement. So the ~431 ms is roughly `write + blackout + one frame`, and
/// the instant the LED actually went out is only bracketed to
/// `[42, 435] ms`. The camera is the only sensor here, and it is asleep for
/// most of that interval; no amount of care with the clock closes the gap.
/// Resolving G1.5 properly needs an observer that does not stall with the
/// mode switch — a second camera watching the LED, or a photodiode.
///
/// The test asserts the budget anyway and therefore fails on this hardware.
/// That is deliberate: `docs/evidence/status-audit.md` records G1.5 as
/// WITHHELD with these numbers, and a test quietly relaxed to pass would
/// erase the one honest thing this measurement produced.
///
/// # What this does and does not establish
///
/// It exercises the **library** emitter path — `namtor_camera::emitter_ctrl`
/// and `namtor_camera::watchdog` — driven from a test process on the reference
/// camera. It does **not** exercise the installed daemon's teardown path in
/// `src/bin/namtor-daemon/live_capture.rs`, which is untouched by this test.
///
/// # Safety
///
/// Identical to the enrollment test above: `force_off_before_anything` before
/// any write (inside [`hw_emitter::ArmedEmitter::arm`]), the armed window
/// bounded by the device's `[emitter.lifecycle] max_on_ms` watchdog, and
/// `payload_off` restored on every exit path including panic, by the guard's
/// `Drop`. The emitter post-conditions are asserted **before** any timing
/// assertion, so a blown budget can never short-circuit the proof that the LED
/// went out.
#[test]
#[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
fn emitter_goes_dark_within_the_teardown_budget() {
    if !hw_enabled() {
        return;
    }
    let _camera = lock_camera();

    let quirks_dirs = quirks_dir_candidates();
    let Some((quirks_dir, emitter_cfg)) =
        hw_emitter::emitter_cfg(&quirks_dirs, hw_emitter::REFERENCE_VID_PID)
    else {
        eprintln!(
            "skipping: no quirks entry for {} in any of {:?}. Without it there is no emitter \
             control to write, and there is no teardown to time.",
            hw_emitter::REFERENCE_VID_PID,
            quirks_dirs,
        );
        return;
    };
    let budget_ms = hw_emitter::max_on_ms(&emitter_cfg);
    eprintln!(
        "quirks: {} (via {}); payload_on {:02x?} payload_off {:02x?}; max_on_ms {budget_ms}",
        hw_emitter::REFERENCE_VID_PID,
        quirks_dir.display(),
        emitter_cfg.payload_on,
        emitter_cfg.payload_off,
    );

    let mut source = open_reference_camera();
    let dark_before = brightest_mean(source.as_mut(), EMITTER_PROBE_FRAMES);
    eprintln!(
        "emitter disarmed: brightest frame mean {dark_before:.2}/255 over \
         {EMITTER_PROBE_FRAMES} frames"
    );

    let clock = SystemClock;
    let mut samples: Vec<TeardownSample> = Vec::new();
    let mut failure: Option<String> = None;

    for i in 0..TEARDOWN_SAMPLES {
        let mut armed = match hw_emitter::ArmedEmitter::arm(Path::new(IR_NODE), &emitter_cfg) {
            Ok(a) => a,
            Err(e) => {
                if samples.is_empty() {
                    source.stop();
                    eprintln!(
                        "skipping: could not arm the IR emitter on {IR_NODE}: {e}\n\
                         The quirks entry claims [access] requires_root = false. If the error \
                         above is EACCES or EPERM that claim is wrong for this kernel and G1.5 \
                         needs a privileged run instead. Any other errno is a device or \
                         configuration fault, not a privilege one."
                    );
                    return;
                }
                failure = Some(format!(
                    "sample {} of {TEARDOWN_SAMPLES}: could not re-arm the emitter after \
                     {} successful samples: {e}",
                    i + 1,
                    samples.len(),
                ));
                break;
            }
        };
        match measure_teardown(source.as_mut(), &emitter_cfg, &clock, &mut armed) {
            Ok(s) => {
                eprintln!(
                    "teardown sample {}/{TEARDOWN_SAMPLES}: lit {:.2}/255; force_off returned \
                     at {:.1} ms; first frame back at {:.1} ms (arming blinded the stream for \
                     {:.1} ms) -> dark at {} \
                     (last lit frame {}; {} frames in a {TEARDOWN_WINDOW_MS:.0} ms window, \
                     {}-frame dark run, {:.1} ms/frame observed)",
                    i + 1,
                    s.lit_mean,
                    s.issue_ms,
                    s.first_frame_ms,
                    s.arm_blind_ms,
                    match s.dark_at_ms {
                        Some(ms) => format!("{ms:.1} ms"),
                        None => "NEVER (within the window)".to_string(),
                    },
                    match s.last_lit_ms {
                        Some(ms) => format!("{ms:.1} ms"),
                        None => "none after t0".to_string(),
                    },
                    s.frames,
                    s.dark_tail,
                    s.frame_interval_ms,
                );
                samples.push(s);
            }
            Err(e) => {
                failure = Some(format!("sample {} of {TEARDOWN_SAMPLES}: {e}", i + 1));
                break;
            }
        }
        // `armed` drops here: `payload_off` written, then read back and
        // re-forced if the device somehow disagrees.
    }

    // --- Emitter post-conditions first. A timing assertion below must never
    // --- be able to hide the proof that the LED went out.
    let after = hw_emitter::read_control(Path::new(IR_NODE), &emitter_cfg);
    assert_eq!(
        after.as_deref().ok(),
        Some(emitter_cfg.payload_off.as_slice()),
        "the emitter control must read back payload_off after teardown; got {after:02x?}"
    );
    let dark_after = brightest_mean(source.as_mut(), EMITTER_PROBE_FRAMES);
    source.stop();
    eprintln!(
        "emitter restored: GET_CUR {after:02x?}; brightest frame mean {dark_after:.2}/255 over \
         {EMITTER_PROBE_FRAMES} frames (was {dark_before:.2} before any arming)"
    );
    assert!(
        dark_after < EMITTER_DARK_MEAN,
        "emitter still lighting the scene after teardown: brightest frame mean \
         {dark_after:.2}/255 is at or above the {EMITTER_DARK_MEAN:.1} dark floor"
    );

    // --- Then: did every sample produce a usable measurement?
    assert!(
        failure.is_none(),
        "G1.5 could not be measured: {}\n{} usable sample(s) collected before the fault.",
        failure.unwrap_or_default(),
        samples.len(),
    );
    assert_eq!(
        samples.len(),
        TEARDOWN_SAMPLES,
        "G1.5 needs {TEARDOWN_SAMPLES} samples; got {}",
        samples.len()
    );

    let never: Vec<String> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.dark_at_ms.is_none())
        .map(|(i, s)| {
            format!(
                "sample {}: {} frames, longest trailing dark run {} (needs {TEARDOWN_DARK_CONFIRM_FRAMES})",
                i + 1,
                s.frames,
                s.dark_tail
            )
        })
        .collect();
    assert!(
        never.is_empty(),
        "GATE G1.5 FAILED: the scene never went sustainably dark within {TEARDOWN_WINDOW_MS:.0} \
         ms of teardown in: {}",
        never.join("; "),
    );

    // --- Then: the timing itself.
    let mut timings: Vec<f64> = samples.iter().filter_map(|s| s.dark_at_ms).collect();
    timings.sort_by(f64::total_cmp);
    let median = timings[timings.len() / 2];
    let max = timings[timings.len() - 1];
    let min = timings[0];
    let granularity =
        samples.iter().map(|s| s.frame_interval_ms).sum::<f64>() / samples.len() as f64;
    // The write cannot have taken effect before it returned, and the sensor
    // says nothing between then and the first frame back. These two numbers
    // bracket the instant the LED actually went out.
    let write_done = samples.iter().map(|s| s.issue_ms).fold(0.0f64, f64::max);
    let blind = samples
        .iter()
        .map(|s| s.first_frame_ms - s.issue_ms)
        .fold(0.0f64, f64::max);
    let arm_blind = samples
        .iter()
        .map(|s| s.arm_blind_ms)
        .fold(0.0f64, f64::max);
    let pretty = |v: &[f64]| {
        v.iter()
            .map(|t| format!("{t:.1}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    eprintln!(
        "G1.5 measured over {TEARDOWN_SAMPLES} samples: [{}] ms; min {min:.1} median \
         {median:.1} max {max:.1} ms against a {TEARDOWN_BUDGET_MS:.0} ms budget.\n\
         Sampling granularity {granularity:.1} ms/frame (GREY8 {IR_W}x{IR_H} at a nominal 30 \
         fps): values are quantised to frame arrivals, are an upper bound because they are \
         timestamped at dequeue rather than at exposure, and carry about +/-1 frame period of \
         uncertainty. Do not quote them more precisely than that.\n\
         MEASUREMENT LIMIT: the payload_off write returned by {write_done:.1} ms, then the \
         device delivered NO frames for a further {blind:.1} ms. The same mode switch blinds \
         the stream for up to {arm_blind:.1} ms on the way in, so the blackout is a property \
         of the MSXU mode change, not of the LED. The instant the emitter actually went dark \
         is therefore only bracketed to [{write_done:.1}, {max:.1}] ms; this method cannot \
         resolve it further, because the only sensor is the camera that goes blind.\n\
         Scope: the namtor_camera emitter_ctrl/watchdog LIBRARY path driven from a test \
         process. NOT the installed daemon's teardown path.",
        pretty(&timings),
    );
    assert!(
        max <= TEARDOWN_BUDGET_MS,
        "GATE G1.5 NOT MET AS MEASURED: the emitter is only *proved* dark at {max:.1} ms \
         (median {median:.1} ms, samples [{}] ms) against a {TEARDOWN_BUDGET_MS:.0} ms budget, \
         over {TEARDOWN_SAMPLES} samples at {granularity:.1} ms/frame granularity.\n\
         This is NOT a claim that the LED stayed lit for {max:.1} ms. The payload_off write \
         returned by {write_done:.1} ms and the sensor then went blind for {blind:.1} ms, so \
         the true off instant lies somewhere in [{write_done:.1}, {max:.1}] ms and is \
         unobservable from here. G1.5 asks for a luminance check, and the luminance evidence \
         does not arrive until {max:.1} ms. Resolving this needs an observer that does not \
         stall with the mode switch (a second camera, or a photodiode), not a looser test.",
        pretty(&timings),
    );
}

// ===========================================================================
// Gates G2.1 / G2.2 — authentication latency on the reference camera.
// ===========================================================================

/// Gate **G2.1**: median authentication latency budget, milliseconds.
const AUTH_MEDIAN_BUDGET_MS: f64 = 500.0;

/// Gate **G2.2**: p95 authentication latency budget, milliseconds.
const AUTH_P95_BUDGET_MS: f64 = 1000.0;

/// How many verify attempts the gates are measured over.
///
/// Twenty is the floor at which a p95 is something other than a restatement
/// of the maximum: at nearest rank the p95 of 20 samples is the 19th, so one
/// outlier still moves it. The test asserts it collected at least
/// [`AUTH_SAMPLES_MIN`] and prints the count beside every number, because a
/// percentile quoted without its N is not evidence.
const AUTH_SAMPLES_TARGET: usize = 24;

/// Fewer samples than this and the run reports what it got and refuses to
/// call the gates either way.
const AUTH_SAMPLES_MIN: usize = 20;

/// The daemon's own per-attempt budget, mirroring the private `VERIFY_TIMEOUT`
/// in `crates/namtor-daemon/src/dispatch.rs`. Every attempt here is given this
/// deadline unless the emitter watchdog forces a shorter one, and each sample
/// records which of the two it actually got.
const VERIFY_TIMEOUT_MS: u64 = 5000;

/// Headroom subtracted from the emitter's `max_on_ms` when deciding whether
/// another attempt fits inside the current armed window.
///
/// [`VerifyEngine::verify`] only tests its deadline at the top of each frame
/// iteration and then blocks in `VIDIOC_DQBUF` plus two ONNX graphs, so it can
/// return later than the deadline it was handed — by up to one frame period
/// plus one inference. This margin covers that overshoot so the LED is dark
/// again before the watchdog budget is spent.
const ARMED_WINDOW_MARGIN_MS: u64 = 400;

/// The shortest deadline worth starting an attempt with. Below this the window
/// is closed and the emitter re-armed instead, so no sample is truncated more
/// severely than it has to be.
const MIN_VERIFY_BUDGET_MS: u64 = 500;

/// Upper bound on frames drained after arming while waiting for the firmware
/// AE to stop clipping. The real bound is the watchdog; this only stops a
/// silent camera from spinning.
const AE_SETTLE_MAX_FRAMES: usize = 45;

/// Fraction of samples at or above 250/255 below which the firmware AE is
/// judged to have stopped clipping.
///
/// **This is the finding that made the measurement possible.** Arming the
/// emitter does not hand you a usable image: the first illuminated frames come
/// back at mean 254/255 with **98% of all samples clipped at 250+**, and YuNet
/// finds nothing in a white rectangle. Measured on the reference camera, the
/// AE then decays over ~13 frames to a mean in the 60–80 band, and detections
/// appear the moment the clipped fraction drops under a couple of percent.
/// `quirks/devices/3277-0018.toml` records the decay as `217.6 -> 45.3` and
/// advises *"Do NOT wait for settling: the first pair has the best SNR
/// (217.2)"*. On this device, at this working distance, the first pair is not
/// 217 but **saturated**, and it carries no recoverable face at all.
const AE_SATURATED_MAX: f32 = 0.02;

/// Sample value at or above which a pixel counts as clipped.
const AE_CLIP_LEVEL: u8 = 250;

/// The uid the throwaway enrollment is filed under. Nothing is written outside
/// a `tempfile::tempdir()`; `/var/lib/namtor` is never touched.
const AUTH_UID: u32 = 1000;

/// Templates the enrollment phase aims to collect. Eight is `SELECT_N` in
/// `manager::session` and the cap `namtor-cli enroll` truncates to, i.e. the
/// most a real enrollment ever keeps.
const ENROLL_TARGET_TEMPLATES: usize = 8;

/// Fewest templates worth authenticating against, matching the floor
/// `namtor-cli enroll` enforces before it will write a record.
const ENROLL_MIN_TEMPLATES: usize = 3;

/// Blur floor, matching `namtor-cli enroll`'s `laplacian_stddev < 3.0` reject.
const ENROLL_BLUR_FLOOR: f32 = 3.0;

/// Detector score/NMS thresholds `namtor-cli enroll` configures before it
/// enrolls, and the quality floor it then applies to the detections that come
/// back. Enrollment deliberately runs a *more permissive* detector than
/// verification — `RecognitionPipeline::new` defaults to `0.6`, which is what
/// [`VerifyEngine`] uses — and filters on the score afterwards. Mirrored here
/// exactly rather than approximated, because the difference decides which
/// frames become templates.
const ENROLL_DETECT_SCORE_THRESHOLD: f32 = 0.3;
const ENROLL_DETECT_NMS_THRESHOLD: f32 = 0.4;
const ENROLL_QUALITY_FLOOR: f32 = 0.5;

/// Armed windows the enrollment phase may spend.
const MAX_ENROLL_WINDOWS: usize = 10;

/// Armed windows the verify phase may spend collecting
/// [`AUTH_SAMPLES_TARGET`] samples.
const MAX_VERIFY_WINDOWS: usize = 40;

/// Fraction of samples at or above [`AE_CLIP_LEVEL`]. The firmware AE's own
/// error signal, read straight off the pixels.
fn saturated_fraction(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    data.iter().filter(|&&b| b >= AE_CLIP_LEVEL).count() as f32 / data.len() as f32
}

/// An armed emitter whose scene has been confirmed lit *and* out of AE
/// clipping, with the cost of getting there recorded.
struct ArmedWindow<'a> {
    armed: hw_emitter::ArmedEmitter<'a>,
    /// When the `payload_on` write returned, i.e. the start of the watchdog's
    /// `max_on_ms` budget.
    armed_at: Instant,
    /// Brightest illuminated frame mean seen while settling.
    lit_mean: f32,
    /// Mean of the first frame judged usable.
    settled_mean: f32,
    /// Milliseconds from the `payload_on` write to that frame. This is the
    /// mode-switch blackout plus the AE decay, and it is excluded from every
    /// latency sample — see the test's doc comment.
    settle_ms: f64,
    /// Frames dequeued and thrown away getting there.
    drained: usize,
}

/// Arms the emitter and drains frames until the scene is lit **and** the
/// firmware AE has stopped clipping.
///
/// The drain is deliberately outside every timed region, for two measured
/// reasons. The MSXU mode switch blinds the V4L2 stream for ~390 ms
/// (`emitter_goes_dark_within_the_teardown_budget` measures it in both
/// directions), and the AE then needs a further ~13 frames to come down off
/// the clipping rail. Neither is authentication: the daemon warms the stream
/// in `AuthPhase::Prepare` before `AuthPhase::Verify` runs. The cost is
/// **measured and reported** rather than silently dropped, because on this
/// device it is not small — see [`ArmedWindow::settle_ms`].
///
/// On every error path the emitter is forced off before the error returns, and
/// the guard that would have done so anyway is dropped.
fn arm_and_wait_for_usable_exposure<'a>(
    node: &Path,
    cfg: &'a EmitterCfg,
    watchdog: &mut namtor_camera::watchdog::Watchdog<'_, SystemClock>,
    source: &mut dyn CaptureSource,
) -> Result<ArmedWindow<'a>, String> {
    let mut armed = hw_emitter::ArmedEmitter::arm(node, cfg)?;
    let armed_at = Instant::now();
    watchdog.arm();

    let mut lit_mean = 0.0f32;
    let mut drained = 0usize;
    let mut settled: Option<f32> = None;
    while drained < AE_SETTLE_MAX_FRAMES {
        if watchdog.check_expired() {
            armed.force_off();
            watchdog.disarm();
            return Err(format!(
                "the {} ms emitter watchdog fired after {drained} frames while waiting for the \
                 exposure to come off the clipping rail; brightest illuminated frame mean \
                 {lit_mean:.2}/255",
                hw_emitter::max_on_ms(cfg),
            ));
        }
        match source.next_frame() {
            Ok(Some(frame)) => {
                drained += 1;
                let mean = frame_mean(&frame.data);
                // Half of every armed window is the clean side of the 1:1
                // firmware strobe and reads ~0.1/255. Only the illuminated
                // side carries an exposure to judge.
                if mean >= EMITTER_LIT_MEAN {
                    lit_mean = lit_mean.max(mean);
                    if saturated_fraction(&frame.data) < AE_SATURATED_MAX {
                        settled = Some(mean);
                        break;
                    }
                }
            }
            Ok(None) => {
                armed.force_off();
                watchdog.disarm();
                return Err("the stream ended while waiting for the exposure to settle".into());
            }
            Err(e) => {
                armed.force_off();
                watchdog.disarm();
                return Err(format!("next_frame while waiting for the exposure: {e}"));
            }
        }
    }
    let Some(settled_mean) = settled else {
        armed.force_off();
        watchdog.disarm();
        return Err(if lit_mean < EMITTER_LIT_MEAN {
            format!(
                "the emitter control accepted payload_on but the scene stayed dark over \
                 {drained} frames: brightest mean {lit_mean:.2}/255, below the \
                 {EMITTER_LIT_MEAN:.1} lit floor"
            )
        } else {
            format!(
                "the scene lit (brightest mean {lit_mean:.2}/255) but the exposure never came \
                 off the clipping rail within {drained} frames: still more than \
                 {:.0}% of samples at {AE_CLIP_LEVEL}+",
                AE_SATURATED_MAX * 100.0,
            )
        });
    };

    Ok(ArmedWindow {
        armed,
        armed_at,
        lit_mean,
        settled_mean,
        settle_ms: armed_at.elapsed().as_secs_f64() * 1000.0,
        drained,
    })
}

/// One timed authentication attempt.
struct LatencySample {
    /// Wall-clock milliseconds from immediately before
    /// [`VerifyEngine::verify`] is entered to immediately after it returns.
    ms: f64,
    verdict: Verdict,
    score: f32,
    reason: Option<String>,
    /// The deadline this attempt was given, in milliseconds.
    budget_ms: u64,
    /// `true` when the emitter watchdog forced a deadline shorter than the
    /// daemon's own [`VERIFY_TIMEOUT_MS`]. A sample that ran to a truncated
    /// deadline is a **lower bound** on what the daemon would have spent.
    truncated: bool,
}

impl LatencySample {
    /// `true` when the attempt spent essentially its whole deadline, i.e. the
    /// reported latency is the budget rather than the pipeline's own cost.
    fn hit_deadline(&self) -> bool {
        self.ms >= self.budget_ms as f64 * 0.95
    }
}

/// Median of an already-sorted slice; the mean of the two middle values when
/// the count is even, so the number does not depend on which side of the pair
/// the implementation happens to pick.
fn median_of(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// Nearest-rank percentile of an already-sorted slice: the smallest sample at
/// or below which at least `pct`% of them fall. No interpolation — with
/// 20-odd samples an interpolated percentile invents precision the data does
/// not have.
fn percentile_of(sorted: &[f64], pct: f64) -> f64 {
    let n = sorted.len();
    let rank = ((pct / 100.0) * n as f64).ceil().max(1.0) as usize;
    sorted[rank.min(n) - 1]
}

fn pretty_ms(v: &[f64]) -> String {
    v.iter()
        .map(|t| format!("{t:.1}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What the enrollment phase produced.
struct LiveEnrollment {
    templates: Vec<namtor_store::Template>,
    quality_scalar: f32,
    windows: usize,
    /// Longest armed window, from the `payload_on` write to the `payload_off`
    /// write. Must stay under the device's `max_on_ms`.
    max_armed_ms: f64,
    /// Brightest illuminated frame mean seen across every armed window: the
    /// proof these templates came off a genuinely lit sensor.
    lit_mean: f32,
    best_confidence: f32,
    frames_examined: usize,
    no_face: usize,
    below_quality: usize,
    too_blurred: usize,
}

/// Enrolls against the live camera, mirroring what `namtor-cli enroll` does:
/// run [`RecognitionPipeline`] over illuminated frames, keep the detections
/// that clear the blur floor as [`Template`]s, then rank by detection score
/// and keep the best [`ENROLL_TARGET_TEMPLATES`].
///
/// # Why not `EnrollmentCapture`
///
/// The daemon's own [`EnrollmentCapture`] — which
/// `live_enrollment_session_accepts_frames_from_the_reference_camera` above
/// drives — takes ownership of its capture source and exposes no handle to it.
/// On this device that is fatal: every armed window starts with the AE on the
/// clipping rail, `pump()` costs ~100 ms of ONNX per frame while the sensor
/// produces one every 33 ms, so the consumer never catches up with the queue
/// and every frame it ever sees is a saturated white rectangle. Measured: **0
/// templates from 141 frames over 8 armed windows, best detection confidence
/// 0.000**, with the emitter verified lit at 254/255 the whole time. Draining
/// the queue cheaply first is the only thing that fixes it, and that needs the
/// source. So this uses the CLI's enrollment path, which is equally shipped
/// and equally real, and notes the difference rather than hiding it.
///
/// Two things about that measurement have changed since it was taken, and
/// neither of them explains it. A consumer this far behind the sensor makes
/// the driver drop frames, and the strobe phase was counted one per dequeue
/// rather than from the driver's frame sequence, so a gap of odd length
/// inverted emitter-on/emitter-off for the rest of the stream; that is fixed
/// (`PhaseTracker` in `namtor-capture::ir_uvc`), but enrollment never read
/// the label, so it was not the cause. `EnrollmentCapture` now also logs the
/// dropped-frame count at stop, which is the number this note had to infer.
/// **The measurement above has not been retaken**, and the AE-clipping
/// explanation for it still stands unrefuted — it is the thing to look at on
/// the next run with the reference camera.
///
/// Every armed window is bounded by the device's own `max_on_ms` watchdog and
/// closed explicitly; `payload_off` is restored on every path including panic.
fn enroll_live(
    yunet: &Path,
    sface: &Path,
    cfg: &EmitterCfg,
    clock: &SystemClock,
) -> Result<LiveEnrollment, String> {
    let mut pipeline = namtor_recog::RecognitionPipeline::new(yunet, sface, (640, 640))
        .map_err(|e| format!("load the enrollment recognition pipeline: {e}"))?;
    pipeline.detector = pipeline
        .detector
        .with_thresholds(ENROLL_DETECT_SCORE_THRESHOLD, ENROLL_DETECT_NMS_THRESHOLD);
    let mut source = open_reference_camera();

    let budget_ms = u64::from(hw_emitter::max_on_ms(cfg));
    let mut templates: Vec<namtor_store::Template> = Vec::new();
    let mut windows = 0usize;
    let mut max_armed_ms = 0.0f64;
    let mut lit_mean = 0.0f32;
    let mut best_confidence = 0.0f32;
    let mut frames_examined = 0usize;
    let mut no_face = 0usize;
    let mut below_quality = 0usize;
    let mut too_blurred = 0usize;
    let mut last_err: Option<String> = None;

    while templates.len() < ENROLL_TARGET_TEMPLATES && windows < MAX_ENROLL_WINDOWS {
        windows += 1;
        let mut watchdog = hw_emitter::watchdog_for(cfg, clock);
        let mut window = match arm_and_wait_for_usable_exposure(
            Path::new(IR_NODE),
            cfg,
            &mut watchdog,
            source.as_mut(),
        ) {
            Ok(w) => w,
            Err(e) => {
                last_err = Some(format!("enroll window {windows}: {e}"));
                break;
            }
        };
        lit_mean = lit_mean.max(window.lit_mean);

        let mut kept = 0usize;
        loop {
            if watchdog.check_expired()
                || window.armed_at.elapsed().as_millis() as u64
                    >= budget_ms.saturating_sub(ARMED_WINDOW_MARGIN_MS)
                || templates.len() >= ENROLL_TARGET_TEMPLATES
            {
                break;
            }
            let frame = match source.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(_) => continue,
            };
            // Skip the clean half of the 1:1 strobe: it is black by design.
            if frame_mean(&frame.data) < EMITTER_LIT_MEAN {
                continue;
            }
            frames_examined += 1;
            match pipeline.process_frame(&frame.data, frame.width, frame.height) {
                Ok(Some(result)) => {
                    let quality = result.detection.box_.score;
                    best_confidence = best_confidence.max(quality);
                    if quality < ENROLL_QUALITY_FLOOR {
                        below_quality += 1;
                        continue;
                    }
                    let blur = namtor_recog::quality::laplacian_stddev(
                        &frame.data,
                        frame.width,
                        frame.height,
                    );
                    if blur < ENROLL_BLUR_FLOOR {
                        too_blurred += 1;
                        continue;
                    }
                    templates.push(namtor_store::Template {
                        vector: result.embedding.vector,
                        quality,
                        is_anchor: false,
                    });
                    kept += 1;
                }
                Ok(None) => no_face += 1,
                Err(e) => {
                    last_err = Some(format!("enroll window {windows}: pipeline: {e}"));
                    break;
                }
            }
        }

        window.armed.force_off();
        watchdog.disarm();
        let armed_ms = window.armed_at.elapsed().as_secs_f64() * 1000.0;
        max_armed_ms = max_armed_ms.max(armed_ms);
        drop(window.armed);
        eprintln!(
            "enroll window {windows}: exposure usable after {} drained frames / {:.0} ms \
             (settled mean {:.1}/255, brightest {:.1}/255); examined {frames_examined} \
             illuminated frames, kept {kept} this window, {} total of \
             {ENROLL_TARGET_TEMPLATES}; armed {armed_ms:.0} ms of a {budget_ms} ms budget",
            window.drained,
            window.settle_ms,
            window.settled_mean,
            window.lit_mean,
            templates.len(),
        );
        if last_err.is_some() {
            break;
        }
    }
    source.stop();
    // Releases /dev/video2 (STREAMOFF + munmap) so the verify phase can
    // reopen it.
    drop(source);

    if templates.len() < ENROLL_MIN_TEMPLATES {
        return Err(format!(
            "the live enrollment produced {} template(s), below the {ENROLL_MIN_TEMPLATES} \
             `namtor-cli enroll` insists on{}. Examined {frames_examined} illuminated frames \
             over {windows} armed window(s): {no_face} with no face even at the \
             {ENROLL_DETECT_SCORE_THRESHOLD} detector threshold, {below_quality} detected but \
             below the {ENROLL_QUALITY_FLOOR} quality floor, {too_blurred} below the \
             {ENROLL_BLUR_FLOOR:.1} blur floor; best detection confidence {best_confidence:.3}; \
             brightest illuminated frame mean {lit_mean:.2}/255. The emitter WAS lit, so this \
             is not the emitter: is a face in front of {IR_NODE}? There is nothing to \
             authenticate against and therefore no authentication latency to measure.",
            templates.len(),
            last_err
                .as_deref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default(),
        ));
    }

    // Rank by detection score and keep the best, exactly as `namtor-cli
    // enroll` does before it writes the record.
    templates.sort_by(|a, b| {
        b.quality
            .partial_cmp(&a.quality)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    templates.truncate(ENROLL_TARGET_TEMPLATES);
    if let Some(first) = templates.first_mut() {
        first.is_anchor = true;
    }
    let quality_scalar = templates.iter().map(|t| t.quality).sum::<f32>() / templates.len() as f32;

    Ok(LiveEnrollment {
        templates,
        quality_scalar,
        windows,
        max_armed_ms,
        lit_mean,
        best_confidence,
        frames_examined,
        no_face,
        below_quality,
        too_blurred,
    })
}

/// Explains a [`TemplateStore`] open failure in terms of what actually blocks
/// it, rather than re-printing a bare `PermissionDenied`.
///
/// # The structural blocker
///
/// [`VerifyEngine::new`] calls `TemplateStore::open(store_dir, key_path)`,
/// which uses `BackendSelector::default()`. The `tpm2` feature is off by
/// default, so the selector falls through to the file backend, and the file
/// backend's root secret path is the **hardcoded** `DEFAULT_ROOT_SECRET_PATH`
/// — `/var/lib/namtor/root_secret` — in `namtor-store/src/crypto.rs`.
/// `RootSecret::load_or_create` then calls `create_dir_all("/var/lib/namtor")`,
/// which an unprivileged process cannot do.
///
/// The store *directory* and the sealing *key* are both parameters of
/// `VerifyEngine::new`; the root secret underneath them is not, and no
/// `LayoutPaths` setting or environment variable moves it. Every migration
/// path forces it too: a `file-v2` blob unseals through `RootSecret`, and a
/// legacy `file` blob is unconditionally re-sealed as `file-v2` on load.
///
/// The consequence is that **an unprivileged process cannot construct a
/// `VerifyEngine` at all**, whatever directory it is pointed at. Every other
/// consumer in the tree sidesteps this with
/// `TemplateStore::open_with_selector(.., BackendSelector::file()
/// .with_root_secret_path(..))` — see `manager/store.rs` and `dbus.rs` — but
/// `VerifyEngine::new` exposes no selector, so a test cannot.
fn store_open_blocker(e: &namtor_store::StoreError) -> String {
    format!(
        "G2.1/G2.2 NOT MEASURED — the verify engine cannot be constructed by an unprivileged \
         process.\n\
         TemplateStore::open failed: {e:?}\n\
         ROOT CAUSE: VerifyEngine::new calls TemplateStore::open, which uses \
         BackendSelector::default(). The tpm2 feature is off by default, so that resolves to \
         the file backend, whose root secret path is the hardcoded \
         /var/lib/namtor/root_secret (DEFAULT_ROOT_SECRET_PATH in namtor-store/src/crypto.rs). \
         RootSecret::load_or_create then calls create_dir_all(\"/var/lib/namtor\"), which this \
         process cannot do. The store directory and the sealing key are both parameters of \
         VerifyEngine::new; the root secret is not, and no LayoutPaths setting or environment \
         variable moves it.\n\
         THIS IS NOT a camera, emitter, model or enrollment fault: all four worked. The \
         enrollment logged above produced real templates off the live sensor moments before \
         this failed.\n\
         UNBLOCKING IT needs one of: (a) a selector-injecting constructor on VerifyEngine, \
         mirroring TemplateStore::open_with_selector, which manager/store.rs and dbus.rs \
         already use for exactly this reason; or (b) running this measurement as the uid that \
         owns /var/lib/namtor. Neither is something a test under tests/ can do for itself."
    )
}

/// Proves on the wire that the emitter control reads back `payload_off`.
/// Opens the node purely to read; arms nothing.
fn assert_emitter_restored(cfg: &EmitterCfg) {
    let after = hw_emitter::read_control(Path::new(IR_NODE), cfg);
    assert_eq!(
        after.as_deref().ok(),
        Some(cfg.payload_off.as_slice()),
        "the emitter control must read back payload_off after teardown; got {after:02x?}"
    );
    eprintln!("emitter disarmed: GET_CUR read back {after:02x?}");
}

/// Gates **G2.1** (median ≤ 500 ms) and **G2.2** (p95 ≤ 1000 ms) — measured
/// with a real clock against the reference camera, a real enrollment off that
/// same camera, and the shipped [`VerifyEngine`].
///
/// # What is timed
///
/// Exactly one thing: the call to [`VerifyEngine::verify`], clocked from
/// immediately before entry to immediately after return. That call is the
/// daemon's entire authentication decision — `dispatch.rs` wraps it in nothing
/// but a backoff lookup and an `AuthReply` construction. Inside the timed
/// region, per spec §6.1:
///
/// * sealed-record load + decrypt from the template store (re-read on every
///   attempt, exactly as the daemon does),
/// * `VIDIOC_DQBUF` of frames until an On/Off pair is in hand,
/// * the strobe differential `D = On − Off`,
/// * analytic PAD over `D` (energy, gradient, specular, radial),
/// * YuNet detection on the illuminated frame,
/// * SFace alignment + 128-d embedding,
/// * cosine match against every enrolled template, z-norm, threshold.
///
/// # What is NOT timed, and why
///
/// * **Model load.** [`VerifyEngine::new`] reads ~37 MiB of SFace plus YuNet
///   and builds both graphs once, before the loop, because the running daemon
///   loads them once at start-up and not per request. Charging a 37 MiB read
///   to every sample would measure the wrong thing.
/// * **Template-store open** and the sealing-key unseal — once at start-up in
///   the daemon too. The per-attempt `store.load` IS timed.
/// * **Camera open, `VIDIOC_STREAMON`, and stream warm-up.** The daemon warms
///   the stream in `AuthPhase::Prepare`; `AuthPhase::Verify` finds it running.
/// * **Emitter arming, the ~390 ms mode-switch blackout, and the firmware AE
///   settling off the clipping rail.** Every armed window drains frames until
///   the exposure is usable *before* the clock starts. This cost is not small
///   and is not hidden: the test prints it per window and reports its median
///   beside the latency numbers. See [`AE_SATURATED_MAX`] — it is the reason
///   this measurement was reachable at all.
/// * **Enrollment.** Done once, before any timing.
///
/// # The emitter watchdog truncates some deadlines — read this
///
/// The daemon gives each attempt 5 s (`VERIFY_TIMEOUT` in `dispatch.rs`). The
/// reference device's `[emitter.lifecycle] max_on_ms` is 2000 ms, and safety
/// comes first: no attempt is started unless its deadline fits in what is left
/// of the armed window. Attempts that fit the full 5 s get it; the rest are
/// handed the shorter deadline and are **flagged**. A flagged sample that ran
/// to its deadline is a *lower bound* on what the daemon would have spent, so
/// it can only make the measured latency look better than it is, never worse.
/// The printout names every such sample.
///
/// # Scope — what this does and does not establish
///
/// This is the **library** [`VerifyEngine`] path driven from a test process.
/// It is **not** the installed daemon serving a PAM request over its socket:
/// no `SO_PEERCRED` ACL, no framing, no nonce, no backoff store, no lifecycle
/// state machine, no worker supervisor, no D-Bus. Those add round trips this
/// test cannot see. The numbers bound the recognition pipeline's own cost on
/// real hardware; they are not an end-to-end login time.
///
/// # Safety
///
/// Identical to the two tests above. `force_off_before_anything` before any
/// write (inside [`hw_emitter::ArmedEmitter::arm`]); every armed window
/// bounded by the device's own `max_on_ms` watchdog and closed explicitly
/// rather than by expiry; `payload_off` restored on every exit path including
/// panic, by the guard's `Drop`. The emitter post-conditions — `GET_CUR` reads
/// back `payload_off` AND the scene measures below the dark floor in photons —
/// are asserted **before** any latency assertion, so a blown budget can never
/// short-circuit the proof that the LED went out.
#[test]
#[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
fn authentication_latency_meets_the_median_and_p95_budgets() {
    if !hw_enabled() {
        return;
    }
    let _camera = lock_camera();
    let Some((yunet, sface)) = model_paths() else {
        return;
    };

    let quirks_dirs = quirks_dir_candidates();
    let Some((quirks_dir, emitter_cfg)) =
        hw_emitter::emitter_cfg(&quirks_dirs, hw_emitter::REFERENCE_VID_PID)
    else {
        eprintln!(
            "skipping: no quirks entry for {} in any of {:?}. Without it there is no emitter \
             control to write, {IR_NODE} stays dark, and nothing can be authenticated.",
            hw_emitter::REFERENCE_VID_PID,
            quirks_dirs,
        );
        return;
    };
    let budget_ms = u64::from(hw_emitter::max_on_ms(&emitter_cfg));
    eprintln!(
        "quirks: {} (via {}); payload_on {:02x?} payload_off {:02x?}; max_on_ms {budget_ms}",
        hw_emitter::REFERENCE_VID_PID,
        quirks_dir.display(),
        emitter_cfg.payload_on,
        emitter_cfg.payload_off,
    );

    let clock = SystemClock;

    // ---- Phase 1: enroll, so there is something to authenticate against.
    let enrolled = match enroll_live(&yunet, &sface, &emitter_cfg, &clock) {
        Ok(e) => e,
        Err(e) => {
            // Every path out of `enroll_live` has already forced the emitter
            // off. Prove it before saying anything else.
            assert_emitter_restored(&emitter_cfg);
            panic!("G2.1/G2.2 NOT MEASURED — live enrollment failed: {e}");
        }
    };
    eprintln!(
        "enrolled {} template(s) (mean detection score {:.3}, best {:.3}) over {} armed \
         window(s), longest {:.0} ms of a {budget_ms} ms budget; brightest illuminated frame \
         mean {:.2}/255. Examined {} illuminated frames: {} with no face, {} below the quality \
         floor, {} too blurred.",
        enrolled.templates.len(),
        enrolled.quality_scalar,
        enrolled.best_confidence,
        enrolled.windows,
        enrolled.max_armed_ms,
        enrolled.lit_mean,
        enrolled.frames_examined,
        enrolled.no_face,
        enrolled.below_quality,
        enrolled.too_blurred,
    );

    // ---- Phase 2: commit the templates to a throwaway store. Never
    // ---- /var/lib/namtor: this is a measurement, not an enrollment.
    let store_dir = tempfile::tempdir().expect("tempdir for the throwaway template store");
    let key_path = store_dir.path().join("key");
    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    let num_templates = enrolled.templates.len() as u32;
    let record = TemplateRecord {
        meta: EnrollmentMeta {
            uid: AUTH_UID,
            username: "hw-latency".into(),
            model_id: "sface_2021dec".into(),
            model_quant: namtor_proto::worker::Quant::Fp32,
            created_at_us: now_us,
            num_templates,
            // No lawful impostor cohort here, so no z-norm parameters: the
            // engine falls through to `DEFAULT_MATCH_THRESHOLD`, exactly as a
            // first enrollment does in production.
            znorm_mean: None,
            znorm_std: None,
            extrinsics: None,
        },
        templates: enrolled.templates,
    };
    {
        let mut sel = namtor_store::crypto::BackendSelector::file()
            .with_root_secret_path(store_dir.path().join("root_secret"));
        let store = match TemplateStore::open_with_selector(store_dir.path(), &key_path, &mut sel) {
            Ok(s) => s,
            Err(e) => {
                assert_emitter_restored(&emitter_cfg);
                panic!("{}", store_open_blocker(&e));
            }
        };
        eprintln!(
            "throwaway store at {:?}: {:?}",
            store_dir.path(),
            store.key_source()
        );
        store
            .store(&record)
            .expect("commit the live templates to the throwaway store");
    }

    // ---- Phase 3: build the engine. Both ONNX graphs and the store are
    // ---- opened HERE, once, outside every timed region, because the running
    // ---- daemon opens them once at start-up.
    let engine_built_at = Instant::now();
    // Use the selector-injecting constructor added for F24: the default one
    // resolves the root secret to /var/lib/namtor/root_secret, which an
    // unprivileged process cannot create. Keep the secret inside the tempdir.
    let mut sel = namtor_store::crypto::BackendSelector::file()
        .with_root_secret_path(store_dir.path().join("root_secret"));
    let mut engine = match VerifyEngine::new_with_selector(
        &yunet,
        &sface,
        store_dir.path(),
        &key_path,
        &mut sel,
    ) {
        Ok(e) => e,
        Err(namtor_daemon::verify::VerifyError::Store(e)) => {
            assert_emitter_restored(&emitter_cfg);
            panic!("{}", store_open_blocker(&e));
        }
        Err(e) => {
            assert_emitter_restored(&emitter_cfg);
            panic!("build the verify engine over the live enrollment: {e}");
        }
    };
    eprintln!(
        "verify engine built in {:.0} ms (YuNet + SFace graphs + store open + key unseal). None \
         of this is inside any latency sample, because the daemon pays it once at start-up.",
        engine_built_at.elapsed().as_secs_f64() * 1000.0,
    );

    // ---- Phase 4: the measurement.
    let mut source = open_reference_camera();
    let mut samples: Vec<LatencySample> = Vec::new();
    let mut settle_costs: Vec<f64> = Vec::new();
    let mut windows = 0usize;
    let mut lit_mean = 0.0f32;
    let mut max_armed_ms = 0.0f64;
    let mut window_failure: Option<String> = None;

    while samples.len() < AUTH_SAMPLES_TARGET && windows < MAX_VERIFY_WINDOWS {
        windows += 1;
        let mut watchdog = hw_emitter::watchdog_for(&emitter_cfg, &clock);
        let mut window = match arm_and_wait_for_usable_exposure(
            Path::new(IR_NODE),
            &emitter_cfg,
            &mut watchdog,
            source.as_mut(),
        ) {
            Ok(w) => w,
            Err(e) => {
                window_failure = Some(format!(
                    "verify window {windows}: {e} ({} sample(s) already collected)",
                    samples.len()
                ));
                break;
            }
        };
        lit_mean = lit_mean.max(window.lit_mean);
        settle_costs.push(window.settle_ms);

        let mut in_window = 0usize;
        while samples.len() < AUTH_SAMPLES_TARGET {
            if watchdog.check_expired() {
                break;
            }
            let spent = window.armed_at.elapsed().as_millis() as u64;
            let remaining = budget_ms
                .saturating_sub(ARMED_WINDOW_MARGIN_MS)
                .saturating_sub(spent);
            if remaining < MIN_VERIFY_BUDGET_MS {
                break;
            }
            let allowed = remaining.min(VERIFY_TIMEOUT_MS);
            let truncated = allowed < VERIFY_TIMEOUT_MS;

            // ------- The measured interval is exactly the next three lines.
            let t0 = Instant::now();
            let outcome = engine.verify(
                source.as_mut(),
                AUTH_UID,
                t0 + Duration::from_millis(allowed),
            );
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            // ------------------------------------------------------------

            in_window += 1;
            eprintln!(
                "sample {:2}: {ms:8.1} ms  {:?} score={:.3}{} (deadline {allowed} ms{})",
                samples.len() + 1,
                outcome.verdict,
                outcome.score,
                outcome
                    .reason
                    .as_deref()
                    .map(|r| format!(" reason=\"{r}\""))
                    .unwrap_or_default(),
                if truncated {
                    ", TRUNCATED by the emitter watchdog"
                } else {
                    ", the daemon's own"
                },
            );
            samples.push(LatencySample {
                ms,
                verdict: outcome.verdict,
                score: outcome.score,
                reason: outcome.reason,
                budget_ms: allowed,
                truncated,
            });
        }

        window.armed.force_off();
        watchdog.disarm();
        let armed_ms = window.armed_at.elapsed().as_secs_f64() * 1000.0;
        max_armed_ms = max_armed_ms.max(armed_ms);
        drop(window.armed);
        eprintln!(
            "verify window {windows}: exposure usable after {} drained frames / {:.0} ms \
             (settled mean {:.1}/255); {in_window} sample(s) in a {armed_ms:.0} ms armed window \
             of a {budget_ms} ms budget",
            window.drained, window.settle_ms, window.settled_mean,
        );
    }

    // --- Emitter post-conditions FIRST. A latency assertion below must never
    // --- be able to hide the proof that the LED went out.
    assert_emitter_restored(&emitter_cfg);
    let dark_after = brightest_mean(source.as_mut(), EMITTER_PROBE_FRAMES);
    source.stop();
    eprintln!(
        "emitter restored: brightest frame mean {dark_after:.2}/255 over {EMITTER_PROBE_FRAMES} \
         frames (was {lit_mean:.2} while armed). Longest armed window {max_armed_ms:.0} ms of a \
         {budget_ms} ms budget over {windows} verify window(s), and {:.0} ms over {} enrollment \
         window(s).",
        enrolled.max_armed_ms, enrolled.windows,
    );
    assert!(
        dark_after < EMITTER_DARK_MEAN,
        "emitter still lighting the scene after teardown: brightest frame mean \
         {dark_after:.2}/255 is at or above the {EMITTER_DARK_MEAN:.1} dark floor"
    );
    assert!(
        max_armed_ms <= budget_ms as f64,
        "a verify armed window ran {max_armed_ms:.0} ms, past the device's own {budget_ms} ms \
         max_on_ms budget"
    );
    assert!(
        enrolled.max_armed_ms <= budget_ms as f64,
        "an enrollment armed window ran {:.0} ms, past the device's own {budget_ms} ms \
         max_on_ms budget",
        enrolled.max_armed_ms,
    );

    // --- Then: is there enough data to speak at all?
    assert!(
        samples.len() >= AUTH_SAMPLES_MIN,
        "G2.1/G2.2 need at least {AUTH_SAMPLES_MIN} samples for a p95 to mean anything; got {} \
         over {windows} armed window(s){}",
        samples.len(),
        window_failure
            .as_deref()
            .map(|e| format!(". Last fault: {e}"))
            .unwrap_or_default(),
    );

    // --- Then: the distribution.
    let mut timings: Vec<f64> = samples.iter().map(|s| s.ms).collect();
    timings.sort_by(f64::total_cmp);
    let median = median_of(&timings);
    let p95 = percentile_of(&timings, 95.0);
    let min = timings[0];
    let max = timings[timings.len() - 1];

    let mut settle_sorted = settle_costs.clone();
    settle_sorted.sort_by(f64::total_cmp);
    let settle_median = if settle_sorted.is_empty() {
        f64::NAN
    } else {
        median_of(&settle_sorted)
    };

    let mut verdicts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for s in &samples {
        let key = match (&s.verdict, s.reason.as_deref()) {
            (Verdict::Match, _) => "Match".to_string(),
            (v, Some(r)) => format!("{v:?} ({r})"),
            (v, None) => format!("{v:?}"),
        };
        *verdicts.entry(key).or_default() += 1;
    }
    let matches = samples
        .iter()
        .filter(|s| s.verdict == Verdict::Match)
        .count();
    let truncated: Vec<usize> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.truncated && s.hit_deadline())
        .map(|(i, _)| i + 1)
        .collect();
    let best_score = samples.iter().map(|s| s.score).fold(0.0f32, f32::max);

    eprintln!(
        "\nG2.1/G2.2 measured over N={} verify attempts on the reference camera:\n\
         samples in collection order (ms): [{}]\n\
         sorted (ms): [{}]\n\
         min {min:.1}   MEDIAN {median:.1}   p95 {p95:.1}   max {max:.1}\n\
         budgets: G2.1 median <= {AUTH_MEDIAN_BUDGET_MS:.0} ms, G2.2 p95 <= \
         {AUTH_P95_BUDGET_MS:.0} ms\n\
         verdicts: {verdicts:?}  ({matches}/{} Match; best score {best_score:.3} against the \
         {:.2} default threshold)\n\
         deadline-truncated samples that ran to their shortened deadline: {}\n\
         EXCLUDED PREREQUISITE: each armed window needed a median {settle_median:.0} ms \
         (samples [{}]) from the payload_on write to the first frame with the exposure off the \
         clipping rail — the MSXU mode-switch blackout plus the firmware AE decay. No latency \
         sample contains it. The daemon pays it in AuthPhase::Prepare, not per attempt, but it \
         is real and it is larger than the G2.1 budget.\n\
         TIMED: VerifyEngine::verify only — store load+decrypt, frame dequeue to an On/Off \
         pair, strobe differential, analytic PAD, YuNet detect, SFace align+embed, cosine \
         match, z-norm, threshold.\n\
         NOT TIMED: ONNX model load (once, before the loop, as in the daemon), store open + \
         key unseal, camera open/STREAMON, emitter arming and exposure settling, enrollment.\n\
         SCOPE: the library VerifyEngine path driven from a test process. NOT the installed \
         daemon serving a PAM request over its socket (no SO_PEERCRED, framing, nonce, \
         backoff, lifecycle or D-Bus).",
        samples.len(),
        pretty_ms(&samples.iter().map(|s| s.ms).collect::<Vec<_>>()),
        pretty_ms(&timings),
        samples.len(),
        namtor_daemon::verify::DEFAULT_MATCH_THRESHOLD,
        if truncated.is_empty() {
            "none".to_string()
        } else {
            format!("{truncated:?} — each is a LOWER BOUND on the daemon's 5 s-deadline cost")
        },
        pretty_ms(&settle_costs),
    );

    // --- Then: the gates.
    assert!(
        median <= AUTH_MEDIAN_BUDGET_MS,
        "GATE G2.1 NOT MET AS MEASURED: median authentication latency {median:.1} ms over N={} \
         attempts, against a {AUTH_MEDIAN_BUDGET_MS:.0} ms budget. Sorted samples (ms): [{}]. \
         Verdicts: {verdicts:?}.",
        samples.len(),
        pretty_ms(&timings),
    );
    assert!(
        p95 <= AUTH_P95_BUDGET_MS,
        "GATE G2.2 NOT MET AS MEASURED: p95 authentication latency {p95:.1} ms (nearest rank) \
         over N={} attempts, against a {AUTH_P95_BUDGET_MS:.0} ms budget. Sorted samples (ms): \
         [{}]. Verdicts: {verdicts:?}.",
        samples.len(),
        pretty_ms(&timings),
    );
}
