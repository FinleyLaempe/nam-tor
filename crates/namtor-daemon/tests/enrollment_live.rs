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

use namtor_camera::watchdog::SystemClock;

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
