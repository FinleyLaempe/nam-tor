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
use std::time::Instant;

use namtor_camera::watchdog::SystemClock;
use namtor_hwdb::quirks::EmitterCfg;

use namtor_capture::frame::PixFmt;
use namtor_capture::ir_uvc::{IrUvc, ParityPolicy, V4l2IrStream};
use namtor_capture::source::{CaptureSource, SourceCaps, Spectrum};
use namtor_daemon::manager::embed::PipelineEmbedSource;
use namtor_daemon::manager::observation::{derive_guidance, observation_from};
use namtor_daemon::manager::session::EnrollmentCapture;
use namtor_layout::{InstallRoot, LayoutPaths};

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
