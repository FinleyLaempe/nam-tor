// SPDX-License-Identifier: GPL-3.0-only
//! Hardware-gated **diagnostic** for the analytic PAD radial-falloff gate.
//!
//! This test asserts nothing about liveness. It measures what
//! `namtor_pad::radial_falloff` is actually fed on the reference camera, and
//! prints the intermediate statistics that decide its verdict: the annular
//! bin counts and means inside the detected face box of `D = On − Off`, the
//! dynamic range, and which of `monotonic_falloff_score`'s two degenerate
//! early returns (if either) fires.
//!
//! It exists because a live authentication run measured
//! `pad veto: radial (1.000)` on 23 of 24 attempts against a real face, i.e.
//! `monotonic_falloff_score` returning **exactly** 0.0 — which is only
//! reachable through an early return, not through a tight threshold.
//!
//! Run with:
//! ```text
//! NAMTOR_HW_TEST=1 cargo test --release -p namtor-daemon --test radial_pad_live -- \
//!     --ignored --nocapture --test-threads=1
//! ```
//!
//! # Safety
//!
//! Identical to `enrollment_live`: the emitter is armed only through the
//! shared [`hw_emitter`] guard (`force_off_before_anything` before any write,
//! RAII `payload_off` on every exit path including panic), every armed window
//! is bounded by the device's own `max_on_ms` watchdog, and the emitter is
//! proven dark afterwards by a `GET_CUR` readback **and** by measuring the
//! scene's photons.

use std::path::{Path, PathBuf};
use std::time::Instant;

use namtor_camera::watchdog::SystemClock;
use namtor_capture::frame::PixFmt;
use namtor_capture::ir_uvc::{IrUvc, ParityPolicy, V4l2IrStream};
use namtor_capture::source::{CaptureSource, SourceCaps, Spectrum};
use namtor_hwdb::quirks::EmitterCfg;
use namtor_layout::{InstallRoot, LayoutPaths};
use namtor_pad::{mean_u8, run_analytic_pad, strobe_difference, PadConfig};
use namtor_recog::detect::FaceDetector;

#[allow(dead_code)]
mod hw_emitter;

const IR_NODE: &str = "/dev/video2";
const META_NODE: &str = "/dev/video3";
const IR_W: u32 = 640;
const IR_H: u32 = 360;
const INSTALLED_MANIFEST: &str = "/usr/share/namtor/install-manifest.toml";
const SOURCE_MANIFEST_REL: &str = "packaging/install-manifest.toml";
const INSTALLED_QUIRKS_DIR: &str = "/usr/share/namtor/quirks/devices";
const MIN_WEIGHT_BYTES: u64 = 64 * 1024;

/// Matching `enrollment_live`: the floor a genuinely illuminated frame clears.
const EMITTER_LIT_MEAN: f32 = 20.0;
/// Matching `enrollment_live`: the AE is off the clipping rail below this.
const AE_SATURATED_MAX: f32 = 0.02;
const AE_CLIP_LEVEL: u8 = 250;
const AE_SETTLE_MAX_FRAMES: usize = 45;
/// The scene must measure below this once the emitter is off.
const EMITTER_DARK_MEAN: f32 = 4.0;
/// Frame pairs to characterise per armed window.
const PAIRS_PER_WINDOW: usize = 6;
/// Armed windows to spend looking for pairs that carry a face.
const MAX_WINDOWS: usize = 12;
/// Face-carrying pairs that make the diagnostic conclusive.
const TARGET_PAIRS: usize = 8;
/// Leave this much of the watchdog budget unused.
const ARMED_WINDOW_MARGIN_MS: u64 = 400;

fn hw_enabled() -> bool {
    if std::env::var("NAMTOR_HW_TEST").is_err() {
        eprintln!("NAMTOR_HW_TEST not set; skipping");
        return false;
    }
    true
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn quirks_dir_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = std::env::var_os("NAMTOR_QUIRKS_DIR") {
        out.push(PathBuf::from(dir));
    }
    out.push(PathBuf::from(INSTALLED_QUIRKS_DIR));
    out.push(repo_root().join("quirks").join("devices"));
    out
}

/// The YuNet graph, located exactly the way `enrollment_live` locates it.
fn yunet_path() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("NAMTOR_MODELS_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    dirs.push(
        LayoutPaths::resolve(
            Path::new("/"),
            Path::new(INSTALLED_MANIFEST),
            InstallRoot::Installed,
        )
        .models_dir,
    );
    let repo = repo_root();
    dirs.push(
        LayoutPaths::resolve(&repo, &repo.join(SOURCE_MANIFEST_REL), InstallRoot::Source)
            .models_dir,
    );
    for dir in &dirs {
        let yunet = dir.join("yunet_2023mar.onnx");
        if std::fs::metadata(&yunet)
            .map(|m| m.len() >= MIN_WEIGHT_BYTES)
            .unwrap_or(false)
        {
            eprintln!("models: {}", yunet.display());
            return Some(yunet);
        }
    }
    eprintln!("skipping: no real yunet_2023mar.onnx in any of {dirs:?}");
    None
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

fn saturated_fraction(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    data.iter().filter(|&&b| b >= AE_CLIP_LEVEL).count() as f32 / data.len() as f32
}

/// The face box exactly as `verify.rs::detection_bbox` computes it.
fn detection_bbox(b: &namtor_recog::detect::FaceBox, w: u32, h: u32) -> (u32, u32, u32, u32) {
    let x = b.x.max(0.0).min(w.saturating_sub(1) as f32) as u32;
    let y = b.y.max(0.0).min(h.saturating_sub(1) as f32) as u32;
    let width = b.w.max(1.0).min(w.saturating_sub(x) as f32) as u32;
    let height = b.h.max(1.0).min(h.saturating_sub(y) as f32) as u32;
    (x, y, width, height)
}

/// A verbatim re-implementation of `namtor_pad::radial`'s private internals,
/// instrumented. Kept byte-for-byte equivalent so the printed numbers are the
/// ones the shipped gate actually computes; the shipped gate's own verdict is
/// printed beside them as a cross-check.
fn describe_radial(d: &[f32], w: u32, h: u32, bbox: (u32, u32, u32, u32)) -> String {
    const BINS: usize = 8;
    let (bx, by, bw, bh) = bbox;
    let x0 = bx.min(w);
    let y0 = by.min(h);
    let x1 = (x0 + bw).min(w);
    let y1 = (y0 + bh).min(h);
    if x1 <= x0 || y1 <= y0 {
        return "degenerate bbox -> full frame".into();
    }
    let cw = (x1 - x0) as usize;
    let ch = (y1 - y0) as usize;
    let mut crop = Vec::with_capacity(cw * ch);
    for y in y0..y1 {
        for x in x0..x1 {
            crop.push(d[(y * w + x) as usize]);
        }
    }

    let mut sum_w = 0.0f32;
    let mut sx = 0.0f32;
    let mut sy = 0.0f32;
    let mut positives = 0usize;
    for y in 0..ch {
        for x in 0..cw {
            let v = crop[y * cw + x];
            if v > 0.0 {
                positives += 1;
                sum_w += v;
                sx += v * x as f32;
                sy += v * y as f32;
            }
        }
    }
    let (cx, cy) = if sum_w > 0.0 {
        (sx / sum_w, sy / sum_w)
    } else {
        ((cw as f32 - 1.0) / 2.0, (ch as f32 - 1.0) / 2.0)
    };

    let max_radius = ((cw as f32).hypot(ch as f32) / 2.0).max(1.0);
    let mut sums = [0.0f32; BINS];
    let mut counts = [0usize; BINS];
    for y in 0..ch {
        for x in 0..cw {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let radius = (dx * dx + dy * dy).sqrt();
            let bin = (((radius / max_radius) * BINS as f32) as usize).min(BINS - 1);
            sums[bin] += crop[y * cw + x].max(0.0);
            counts[bin] += 1;
        }
    }

    let dmin = crop.iter().copied().fold(f32::INFINITY, f32::min);
    let dmax = crop.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let dmean = crop.iter().copied().sum::<f32>() / crop.len() as f32;
    let head = format!(
        "crop {cw}x{ch} centroid=({cx:.1},{cy:.1}) max_radius={max_radius:.1} \
         D[min={dmin:.1} max={dmax:.1} mean={dmean:.2}] positive={:.1}% counts={counts:?}",
        positives as f32 * 100.0 / crop.len() as f32,
    );

    if counts.contains(&0) {
        return format!("{head}\n    => EARLY RETURN: counts.contains(&0) -> score 0.0");
    }
    let means: [f32; BINS] = std::array::from_fn(|i| sums[i] / counts[i] as f32);
    let pretty: Vec<String> = means.iter().map(|m| format!("{m:.2}")).collect();
    let dynamic_range = means[0] - means[BINS - 1];
    let needed = means[0].abs() * 0.05;
    let body = format!(
        "{head}\n    means=[{}] dynamic_range={dynamic_range:.3} (needs > {needed:.3}, \
         i.e. > 5% of means[0]={:.2}); contrast={:.2}%",
        pretty.join(", "),
        means[0],
        if means[0].abs() > 0.0 {
            dynamic_range / means[0].abs() * 100.0
        } else {
            0.0
        },
    );
    if dynamic_range <= needed {
        return format!("{body}\n    => EARLY RETURN: dynamic_range <= 5%*means[0] -> score 0.0");
    }
    let decreasing = means
        .windows(2)
        .filter(|pair| pair[1] <= pair[0] + dynamic_range * 0.10)
        .count();
    format!(
        "{body}\n    => decreasing={decreasing}/7 score={:.3} raw={:.3}",
        decreasing as f32 / (BINS - 1) as f32,
        1.0 - decreasing as f32 / (BINS - 1) as f32,
    )
}

struct Window<'a> {
    armed: hw_emitter::ArmedEmitter<'a>,
    armed_at: Instant,
    settled_mean: f32,
}

fn arm_and_settle<'a>(
    node: &Path,
    cfg: &'a EmitterCfg,
    watchdog: &mut namtor_camera::watchdog::Watchdog<'_, SystemClock>,
    source: &mut dyn CaptureSource,
) -> Result<Window<'a>, String> {
    let mut armed = hw_emitter::ArmedEmitter::arm(node, cfg)?;
    let armed_at = Instant::now();
    watchdog.arm();
    let mut drained = 0usize;
    let mut lit_mean = 0.0f32;
    while drained < AE_SETTLE_MAX_FRAMES {
        if watchdog.check_expired() {
            armed.force_off();
            watchdog.disarm();
            return Err(format!(
                "watchdog fired after {drained} frames while settling"
            ));
        }
        match source.next_frame() {
            Ok(Some(frame)) => {
                drained += 1;
                let mean = mean_u8(&frame.data);
                if mean >= EMITTER_LIT_MEAN {
                    lit_mean = lit_mean.max(mean);
                    if saturated_fraction(&frame.data) < AE_SATURATED_MAX {
                        return Ok(Window {
                            armed,
                            armed_at,
                            settled_mean: mean,
                        });
                    }
                }
            }
            Ok(None) => {
                armed.force_off();
                watchdog.disarm();
                return Err("stream ended while settling".into());
            }
            Err(e) => {
                armed.force_off();
                watchdog.disarm();
                return Err(format!("next_frame while settling: {e}"));
            }
        }
    }
    armed.force_off();
    watchdog.disarm();
    Err(format!(
        "exposure never came off the clipping rail in {drained} frames (brightest {lit_mean:.1})"
    ))
}

/// Characterise the radial gate's real input. Asserts only the emitter
/// post-conditions; the PAD numbers are reported, never asserted.
#[test]
#[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
fn radial_gate_input_on_the_reference_camera() {
    if !hw_enabled() {
        return;
    }
    let Some(yunet) = yunet_path() else {
        return;
    };
    let quirks_dirs = quirks_dir_candidates();
    let Some((quirks_dir, cfg)) =
        hw_emitter::emitter_cfg(&quirks_dirs, hw_emitter::REFERENCE_VID_PID)
    else {
        eprintln!(
            "skipping: no quirks entry for {} in {quirks_dirs:?}",
            hw_emitter::REFERENCE_VID_PID
        );
        return;
    };
    eprintln!("quirks: {}", quirks_dir.display());

    // Enrollment runs the permissive detector (0.3/0.4) and filters on the
    // score afterwards; do the same so a diagnostic is not gated on the
    // stricter verification floor.
    let mut detector = FaceDetector::new(&yunet, (640, 640))
        .expect("load YuNet")
        .with_thresholds(0.3, 0.4);
    let mut source = open_reference_camera();
    let clock = SystemClock;
    let budget_ms = u64::from(hw_emitter::max_on_ms(&cfg));
    let pad_cfg = PadConfig::default();

    let mut described = 0usize;
    let mut no_face = 0usize;
    let mut windows = 0usize;

    while described < TARGET_PAIRS && windows < MAX_WINDOWS {
        windows += 1;
        let mut watchdog = hw_emitter::watchdog_for(&cfg, &clock);
        let mut window =
            match arm_and_settle(Path::new(IR_NODE), &cfg, &mut watchdog, source.as_mut()) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("window {windows}: {e}");
                    // A window that never came off the clipping rail is not a
                    // reason to stop; the AE state carries over, so the next
                    // window usually settles. The loop is bounded by
                    // MAX_WINDOWS either way.
                    continue;
                }
            };
        eprintln!(
            "window {windows}: settled at mean {:.2}/255",
            window.settled_mean
        );

        let mut pairs = 0usize;
        let mut prev: Option<namtor_capture::frame::Frame> = None;
        while pairs < PAIRS_PER_WINDOW && described < TARGET_PAIRS {
            if watchdog.check_expired()
                || window.armed_at.elapsed().as_millis() as u64
                    >= budget_ms.saturating_sub(ARMED_WINDOW_MARGIN_MS)
            {
                break;
            }
            let frame = match source.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(_) => continue,
            };
            let Some(first) = prev.take() else {
                prev = Some(frame);
                continue;
            };
            pairs += 1;
            // Exactly what verify.rs does: order the pair by measured
            // luminance, not by the parity tag (finding F21).
            let (on, off) = if mean_u8(&first.data) >= mean_u8(&frame.data) {
                (&first, &frame)
            } else {
                (&frame, &first)
            };
            let w = on.width;
            let h = on.height;
            let on_mean = mean_u8(&on.data);
            let off_mean = mean_u8(&off.data);
            let Ok(d) = strobe_difference(&on.data, &off.data, w, h) else {
                continue;
            };
            let detections = match detector.detect(&on.data, w, h) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("  detect failed: {e}");
                    continue;
                }
            };
            let Some(best) = detections
                .iter()
                .max_by(|a, b| a.box_.score.total_cmp(&b.box_.score))
            else {
                no_face += 1;
                if no_face <= 1 {
                    // No face: still characterise D, on the full frame and on
                    // a centred box the size of a face at working distance.
                    let cx = w / 2;
                    let cy = h / 2;
                    let bb = (cx - 80, cy - 80, 160, 160);
                    eprintln!(
                        "  no-face pair: on_mean={on_mean:.2} off_mean={off_mean:.2} \
                         sat(on)={:.3} sat(off)={:.3}",
                        saturated_fraction(&on.data),
                        saturated_fraction(&off.data),
                    );
                    eprintln!(
                        "    full frame: {}",
                        describe_radial(&d, w, h, (0, 0, w, h))
                    );
                    eprintln!("    centred 160x160: {}", describe_radial(&d, w, h, bb));
                    // How the statistic behaves as the crop tightens onto a
                    // roughly-equidistant region — which is exactly what a
                    // YuNet face box is.
                    for side in [320u32, 240, 180, 140, 110, 90, 70, 50] {
                        for (lx, ly, tag) in [
                            (cx - side / 2, cy - side / 2, "centre"),
                            (cx - side / 2 - 140, cy - side / 2, "left"),
                            (cx - side / 2 + 140, cy - side / 2, "right"),
                        ] {
                            eprintln!(
                                "      {side}x{side} @{tag}: {}",
                                describe_radial(&d, w, h, (lx, ly, side, side))
                            );
                        }
                    }
                }
                continue;
            };
            let bbox = detection_bbox(&best.box_, w, h);
            let verdict = match run_analytic_pad(&d, w, h, on_mean, Some(bbox), &pad_cfg) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("  pad failed: {e}");
                    continue;
                }
            };
            described += 1;
            eprintln!(
                "  pair {described}: on_mean={on_mean:.2} off_mean={off_mean:.2} \
                 parity(on)={:?} parity(off)={:?} bbox={bbox:?} det_score={:.3}",
                on.emitter, off.emitter, best.box_.score,
            );
            eprintln!("    {}", describe_radial(&d, w, h, bbox));
            eprintln!(
                "    shipped gates: energy(pass={} raw={:.4}) gradient(pass={} raw={:.4}) \
                 specular(pass={} raw={:.4}) radial(pass={} score={:.4} raw={:.4}) live={}",
                verdict.energy.pass,
                verdict.energy.raw,
                verdict.gradient.pass,
                verdict.gradient.raw,
                verdict.specular.pass,
                verdict.specular.raw,
                verdict.radial.pass,
                verdict.radial.score,
                verdict.radial.raw,
                verdict.live,
            );
            prev = None;
        }

        window.armed.force_off();
        watchdog.disarm();
        drop(window.armed);
    }

    eprintln!("characterised {described} face-carrying pair(s); {no_face} pair(s) had no face");

    // ---- Emitter post-conditions, asserted before anything else is said.
    let after = hw_emitter::read_control(Path::new(IR_NODE), &cfg);
    assert_eq!(
        after.as_deref().ok(),
        Some(cfg.payload_off.as_slice()),
        "the emitter control must read back payload_off after teardown; got {after:02x?}"
    );
    let mut dark = 0.0f32;
    for _ in 0..8 {
        if let Ok(Some(f)) = source.next_frame() {
            dark = dark.max(mean_u8(&f.data));
        }
    }
    eprintln!("emitter disarmed: GET_CUR {after:02x?}; brightest post-teardown mean {dark:.2}/255");
    assert!(
        dark < EMITTER_DARK_MEAN,
        "the scene must be dark after teardown; brightest mean {dark:.2}/255"
    );
}
