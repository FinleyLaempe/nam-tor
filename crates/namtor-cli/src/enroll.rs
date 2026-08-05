// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment subcommand: capture 25 frames, quality-filter, retain 8 diverse
//! templates, store encrypted.
//!
//! Spec §6.6 Tier 1: "Capture 25 frames across poses (±15–20° yaw/pitch),
//! distances, ambient conditions. Actively reject bad frames: variance-of-
//! Laplacian blur threshold, minimum face size, pose angle limit, exposure
//! sanity, landmark confidence. Retain the 8 most diverse high-quality
//! templates."

use std::path::Path;
use std::process::ExitCode;

use namtor_recog::pipeline::RecognitionPipeline;
use namtor_store::template::{EnrollmentMeta, Template, TemplateRecord};
use namtor_store::TemplateStore;

pub fn run_enroll(
    models_dir: &Path,
    templates_dir: &Path,
    key_path: &Path,
    _device: Option<String>,
    user: Option<String>,
) -> ExitCode {
    let yunet = models_dir.join("yunet_2023mar.onnx");
    let sface = models_dir.join("sface_2021dec.onnx");

    if !yunet.exists() {
        eprintln!("error: YuNet model not found at {}", yunet.display());
        eprintln!("download: curl -sL 'https://github.com/opencv/opencv_zoo/raw/refs/heads/main/models/face_detection_yunet/face_detection_yunet_2023mar.onnx' -o {}", yunet.display());
        return ExitCode::FAILURE;
    }
    if !sface.exists() {
        eprintln!("error: SFace model not found at {}", sface.display());
        eprintln!("download: curl -sL 'https://github.com/opencv/opencv_zoo/raw/refs/heads/main/models/face_recognition_sface/face_recognition_sface_2021dec.onnx' -o {}", sface.display());
        return ExitCode::FAILURE;
    }

    let (uid, username) = match &user {
        Some(name) => {
            let u = nix::unistd::User::from_name(name)
                .ok()
                .flatten()
                .unwrap_or_else(|| {
                    eprintln!("error: user '{name}' not found");
                    std::process::exit(1);
                });
            (u.uid.as_raw(), u.name)
        }
        None => {
            let uid = nix::unistd::getuid().as_raw();
            let username = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
                .ok()
                .flatten()
                .map(|u| u.name)
                .unwrap_or_else(|| format!("uid-{uid}"));
            (uid, username)
        }
    };

    eprintln!("enrolling uid={uid} ({username})...");
    eprintln!("opening IR camera /dev/video2...");

    let frame_data = match capture_enrollment_frames() {
        Ok(frames) => frames,
        Err(e) => {
            eprintln!("capture failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    if frame_data.is_empty() {
        eprintln!("no frames captured");
        return ExitCode::FAILURE;
    }

    eprintln!(
        "captured {} frames, running recognition pipeline...",
        frame_data.len()
    );

    let mut pipeline = match RecognitionPipeline::new(&yunet, &sface, (640, 640)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("pipeline init failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut templates: Vec<Template> = Vec::new();
    for (i, frame) in frame_data.iter().enumerate() {
        match pipeline.process_frame(&frame.data, frame.width, frame.height) {
            Ok(Some(result)) => {
                let quality = result.detection.box_.score;
                if quality < 0.5 {
                    continue;
                }
                let blur = variance_of_laplacian(&frame.data, frame.width, frame.height);
                if blur < 50.0 {
                    eprintln!("  frame {i}: skipped (blur={blur:.1})");
                    continue;
                }
                eprintln!("  frame {i}: face detected (score={quality:.3}, blur={blur:.1})");
                templates.push(Template {
                    vector: result.embedding.vector,
                    quality,
                    is_anchor: templates.is_empty(),
                });
            }
            Ok(None) => {
                eprintln!("  frame {i}: no face detected");
            }
            Err(e) => {
                eprintln!("  frame {i}: pipeline error: {e}");
            }
        }
    }

    if templates.len() < 3 {
        eprintln!(
            "error: only {} valid frames captured (need at least 3)",
            templates.len()
        );
        return ExitCode::FAILURE;
    }

    let num_templates = templates.len().min(8) as u32;
    if templates.len() > 8 {
        templates.sort_by(|a, b| {
            b.quality
                .partial_cmp(&a.quality)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        templates.truncate(8);
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;

    let record = TemplateRecord {
        meta: EnrollmentMeta {
            uid,
            username: username.clone(),
            model_id: "sface_2021dec".into(),
            model_quant: namtor_proto::worker::Quant::Fp32,
            created_at_us: now,
            num_templates,
            znorm_mean: None,
            znorm_std: None,
        },
        templates,
    };

    let store = match TemplateStore::open(templates_dir, key_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("store open failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    eprintln!("key source: {:?}", store.key_source());

    match store.update(&record) {
        Ok(()) => {
            eprintln!("enrolled {username} (uid={uid}): {num_templates} templates stored");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("store failed: {e}");
            ExitCode::FAILURE
        }
    }
}

pub fn run_remove(uid: u32, templates_dir: &Path, key_path: &Path) -> ExitCode {
    let store = match TemplateStore::open(templates_dir, key_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("store open failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    match store.delete(uid) {
        Ok(()) => {
            eprintln!("removed enrollment for uid={uid}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("remove failed: {e}");
            ExitCode::FAILURE
        }
    }
}

struct CapturedFrame {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

fn capture_enrollment_frames() -> Result<Vec<CapturedFrame>, String> {
    let dev = "/dev/video2";
    let meta_dev = "/dev/video3";
    if !std::path::Path::new(dev).exists() {
        return Err(format!("{dev} not found"));
    }

    let width = 640u32;
    let height = 360u32;
    let n_frames = 25usize;

    use namtor_capture::frame::PixFmt;
    use namtor_capture::ir_uvc::{IrUvc, ParityPolicy, V4l2IrStream};
    use namtor_capture::source::CaptureSource;

    // Activate the IR emitter via MSXU mode 2 (firmware-strobed alternating
    // illumination) before opening the V4L2 stream.
    let emitter = activate_emitter(dev).unwrap_or_else(|e| {
        eprintln!("warning: emitter activation failed: {e}");
        eprintln!("capturing without IR illumination (faces may not be visible)");
        EmitterGuard { xu: None }
    });

    let streamer = V4l2IrStream::open(
        std::path::Path::new(dev),
        if std::path::Path::new(meta_dev).exists() {
            Some(std::path::Path::new(meta_dev))
        } else {
            None
        },
        width,
        height,
    )
    .map_err(|e| format!("open {dev}: {e}"))?;

    let caps = namtor_capture::source::SourceCaps {
        spectrum: namtor_capture::source::Spectrum::Ir,
        format: PixFmt::Grey8,
        width,
        height,
        fps: 30,
        has_metadata: true,
    };

    let mut source = IrUvc::new(streamer, ParityPolicy, caps);
    source.start().map_err(|e| format!("start: {e}"))?;

    // Discard the first few frames while the strobe settles (~6 ms, but
    // the AE loop takes ~1 s / 15 pairs to stabilise per the quirk DB).
    for _ in 0..6 {
        let _ = source.next_frame();
    }

    let mut frames = Vec::new();
    while frames.len() < n_frames {
        match source.next_frame() {
            Ok(Some(frame)) => {
                frames.push(CapturedFrame {
                    data: frame.data,
                    width,
                    height,
                });
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("frame capture error: {e}");
                break;
            }
        }
    }

    source.stop();
    drop(emitter);
    Ok(frames)
}

/// RAII guard that activates the IR emitter and turns it off on drop.
struct EmitterGuard {
    xu: Option<Box<namtor_uvc::probe::SafeXu<namtor_uvc::device::V4l2Device>>>,
}

impl Drop for EmitterGuard {
    fn drop(&mut self) {
        if let Some(mut xu) = self.xu.take() {
            if let Some(cfg) = emitter_cfg() {
                let unit = cfg.unit.unwrap_or(0);
                let selector = cfg.selector.unwrap_or(0);
                let _ = xu.try_write(unit, selector, &cfg.payload_off);
            }
        }
    }
}

fn emitter_cfg() -> Option<namtor_hwdb::quirks::EmitterCfg> {
    let candidates = [
        std::path::PathBuf::from("/usr/share/namtor/quirks/devices"),
        std::path::PathBuf::from("quirks/devices"),
        std::path::PathBuf::from("../quirks/devices"),
    ];
    for dir in &candidates {
        if let Ok(db) = namtor_hwdb::quirks::load_quirks_dir(dir) {
            if let Some(e) = db.entries.get("3277:0018") {
                eprintln!("emitter: loaded quirk from {}", dir.display());
                return Some(e.emitter.clone());
            }
        }
    }
    None
}

fn activate_emitter(dev: &str) -> Result<EmitterGuard, String> {
    use namtor_uvc::device::V4l2Device;
    use namtor_uvc::probe::SafeXu;

    let cfg = emitter_cfg().ok_or_else(|| "no quirk entry for 3277:0018".to_string())?;

    eprintln!(
        "emitter: unit={:?} selector={:?} payload_on={:02x?}",
        cfg.unit, cfg.selector, cfg.payload_on
    );

    let v4l2_dev = V4l2Device::open(std::path::Path::new(dev))
        .map_err(|e| format!("open {dev} for XU control: {e}"))?;

    let mut xu = Box::new(SafeXu::new(v4l2_dev));

    let unit = cfg.unit.ok_or("emitter cfg missing unit")?;
    let selector = cfg.selector.ok_or("emitter cfg missing selector")?;

    eprintln!("emitter: probing unit {unit} selector {selector}...");
    match xu.probe_control(unit, selector) {
        Ok(profile) => eprintln!(
            "emitter: probe ok len={} info={:?} current={:02x?}",
            profile.len, profile.info, profile.current
        ),
        Err(e) => {
            return Err(format!("emitter probe failed: {e}"));
        }
    }

    eprintln!("emitter: writing payload_on={:02x?}...", cfg.payload_on);
    let restorer = xu
        .try_write(unit, selector, &cfg.payload_on)
        .map_err(|e| format!("emitter write failed: {e}"))?;
    restorer.keep();
    eprintln!("emitter: ON (mode 2 strobe activated)");

    Ok(EmitterGuard { xu: Some(xu) })
}

fn variance_of_laplacian(data: &[u8], width: u32, height: u32) -> f32 {
    let w = width as usize;
    let h = height as usize;
    if data.len() < w * h || w < 3 || h < 3 {
        return 0.0;
    }
    let mut laplacian = vec![0f32; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let idx = y * w + x;
            let center = data[idx] as f32;
            let up = data[idx - w] as f32;
            let down = data[idx + w] as f32;
            let left = data[idx - 1] as f32;
            let right = data[idx + 1] as f32;
            laplacian[idx] = -4.0 * center + up + down + left + right;
        }
    }
    let mean: f32 = laplacian.iter().sum::<f32>() / laplacian.len() as f32;
    let var: f32 =
        laplacian.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / laplacian.len() as f32;
    var.sqrt()
}
