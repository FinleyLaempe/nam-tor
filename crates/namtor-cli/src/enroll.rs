// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment subcommand.
//!
//! v3 path (default): drives the daemon's `org.namtor.Manager3` continuous
//! enrollment lifecycle over the system D-Bus. The daemon owns capture,
//! quality scoring, and the candidate reservoir; the CLI is a thin driver
//! that shows progress, waits for the operator to stop, reviews, and
//! commits/discards. Spec §6.6 Tier 1 + enrollment redesign.
//!
//! `--old` path: the original local capture-and-store flow (Spec §6.6 Tier 1
//! baseline). Captures 25 frames directly via V4L2, quality-filters, retains
//! 8 diverse templates, stores encrypted. Kept for v1 compatibility and for
//! environments where the daemon is not running.

use std::io::{stdin, BufReader};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use namtor_recog::pipeline::RecognitionPipeline;
use namtor_recog::quality::laplacian_stddev;
use namtor_store::template::{EnrollmentMeta, Template, TemplateRecord};
use namtor_store::{StoreError, TemplateStore};

use crate::enroll_session::{install_sigint_handler, run_v3_session, DbusEnrollmentServiceV3};

/// The system user that owns the template store in production. The
/// sysusers.d entry (`packaging/sysusers.d/namtor.conf`) installs it; the
/// daemon runs as this user and expects every store artefact — key file,
/// templates directory, per-uid lock files, sealed records — to be owned by
/// it. Enrollment run as root chowns every artefact to this user so the
/// daemon can read what enrollment wrote.
const STORE_OWNER: &str = "namtor";

/// Resolved enrollment identity: who is being enrolled and which uid the
/// store should treat as the file owner.
#[derive(Debug, Clone)]
pub struct EnrollmentIdentity {
    pub uid: u32,
    pub username: String,
    pub store_owner_uid: u32,
}

/// Identity-resolution failures. Each variant maps to a distinct user-facing
/// error so the operator can tell a missing install from a missing user.
#[derive(Debug)]
pub enum IdentityError {
    /// `--user` named a user that does not exist on this system.
    UnknownUser(String),
    /// Running as root but the `namtor` system user is not installed
    /// (sysusers.d not applied). Enrollment cannot produce daemon-readable
    /// artefacts without it.
    MissingStoreOwner,
    /// `--user` targets another account but the process is not root; only
    /// root may enroll another user.
    NotRootForOtherUser,
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdentityError::UnknownUser(n) => {
                write!(f, "user '{n}' not found")
            }
            IdentityError::MissingStoreOwner => {
                write!(
                    f,
                    "system user '{STORE_OWNER}' not found; install the namtor package (sysusers.d) before enrolling"
                )
            }
            IdentityError::NotRootForOtherUser => {
                write!(f, "enrolling another user requires root")
            }
        }
    }
}

/// Resolve the enrollment identity for `user` (None = self).
///
/// - Self-enrollment (no `--user`): the enrolled uid is the current euid and
///   the store owner is the current euid. Works unprivileged.
/// - Enrolling another user (`--user NAME`): requires root. The enrolled uid
///   is NAME's uid; the store owner is the `namtor` system user, which must
///   exist (the daemon reads the artefacts as `namtor`).
pub fn resolve_identity(user: Option<&str>) -> Result<EnrollmentIdentity, IdentityError> {
    let euid = nix::unistd::getuid().as_raw();
    let is_root = euid == 0;

    match user {
        Some(name) => {
            let target = nix::unistd::User::from_name(name)
                .map_err(|_| IdentityError::UnknownUser(name.to_string()))?
                .ok_or_else(|| IdentityError::UnknownUser(name.to_string()))?;
            if !is_root {
                return Err(IdentityError::NotRootForOtherUser);
            }
            let owner = nix::unistd::User::from_name(STORE_OWNER)
                .map_err(|_| IdentityError::MissingStoreOwner)?
                .ok_or(IdentityError::MissingStoreOwner)?;
            Ok(EnrollmentIdentity {
                uid: target.uid.as_raw(),
                username: target.name,
                store_owner_uid: owner.uid.as_raw(),
            })
        }
        None => {
            let username = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(euid))
                .ok()
                .flatten()
                .map(|u| u.name)
                .unwrap_or_else(|| format!("uid-{euid}"));
            let store_owner_uid = if is_root {
                nix::unistd::User::from_name(STORE_OWNER)
                    .map_err(|_| IdentityError::MissingStoreOwner)?
                    .ok_or(IdentityError::MissingStoreOwner)?
                    .uid
                    .as_raw()
            } else {
                euid
            };
            Ok(EnrollmentIdentity {
                uid: euid,
                username,
                store_owner_uid,
            })
        }
    }
}

/// Commit an enrollment record to the store with the correct owner.
///
/// Opens the store expecting `identity.store_owner_uid` as the file owner,
/// then `update`s the record atomically. The store chowns the key file,
/// templates directory, per-uid lock file, and sealed record to the owner
/// when running as root, so the daemon (running as `namtor`) can immediately
/// read what a root-initiated enrollment wrote. On any write failure the
/// store's atomic-write discipline leaves the previous record byte-identical
/// and removes the temp file.
pub fn commit_enrollment(
    templates_dir: &Path,
    key_path: &Path,
    identity: &EnrollmentIdentity,
    record: &TemplateRecord,
) -> Result<(), StoreError> {
    let mut selector = namtor_store::crypto::BackendSelector::default();
    commit_enrollment_with_selector(templates_dir, key_path, identity, record, &mut selector)
}

pub fn commit_enrollment_with_selector(
    templates_dir: &Path,
    key_path: &Path,
    identity: &EnrollmentIdentity,
    record: &TemplateRecord,
    selector: &mut namtor_store::crypto::BackendSelector,
) -> Result<(), StoreError> {
    let store = TemplateStore::open_with_owner_and_selector(
        templates_dir,
        key_path,
        Some(identity.store_owner_uid),
        selector,
    )?;
    store.update(record)
}

pub fn run_enroll(
    models_dir: &Path,
    templates_dir: &Path,
    key_path: &Path,
    device: Option<String>,
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

    let identity = match resolve_identity(user.as_deref()) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let uid = identity.uid;
    let username = identity.username.clone();

    eprintln!("enrolling uid={uid} ({username})...");

    let frame_data = match capture_enrollment_frames(device.as_deref()) {
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
        Ok(p) => {
            let mut p = p;
            p.detector = p.detector.with_thresholds(0.3, 0.4);
            p
        }
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
                // Review finding F7: this used to be a byte-for-byte copy of
                // `namtor_recog::quality::laplacian_stddev`. The copy is gone
                // and the CLI delegates, so the two cannot drift. The
                // arithmetic is unchanged, so the `< 3.0` reject below still
                // means exactly what it meant when it was calibrated.
                let blur = laplacian_stddev(&frame.data, frame.width, frame.height);
                if blur < 3.0 {
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
            extrinsics: None,
        },
        templates,
    };

    let store = match TemplateStore::open_with_owner(
        templates_dir,
        key_path,
        Some(identity.store_owner_uid),
    ) {
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

/// v3 enrollment entry point: drives the daemon's continuous enrollment
/// lifecycle over `org.namtor.Manager3` on the system D-Bus.
///
/// Flow: resolve identity → connect to daemon → start → progress → stop →
/// review → Save/Resume/Discard. SIGINT cancels cleanly.
pub fn run_enroll_v3(user: Option<String>) -> ExitCode {
    let identity = match resolve_identity(user.as_deref()) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let service = match DbusEnrollmentServiceV3::connect() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("hint: is the namtor-daemon running? Or use `namtor-cli enroll --old` for the local capture path.");
            return ExitCode::FAILURE;
        }
    };

    let cancel_flag = Arc::new(AtomicBool::new(false));
    if let Err(e) = install_sigint_handler(cancel_flag.clone()) {
        eprintln!("warning: {e}; Ctrl+C will hard-terminate");
    }

    let mut reader = BufReader::new(stdin());
    run_v3_session(
        &service,
        identity.uid,
        &identity.username,
        &mut reader,
        &cancel_flag,
    )
    .to_exit_code()
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

/// A discovered IR camera candidate with its USB VID:PID for quirks lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCandidate {
    pub dev_path: std::path::PathBuf,
    pub vid_pid: String,
    pub is_metadata: bool,
}

/// Resolve which device to use for `--old` enrollment and its VID:PID for
/// quirks lookup (USA-14). If `--device` is given, it takes priority — the
/// function returns a candidate matching that path (with the VID:PID from the
/// candidates list when available, or an empty string when the device is not
/// found among the enumerated candidates). Without `--device`, the function
/// probes by VID:PID: picks the first non-metadata candidate by lowest
/// `/dev/videoN` index.
pub fn resolve_enrollment_device(
    device: Option<&str>,
    candidates: &[DeviceCandidate],
) -> Option<DeviceCandidate> {
    if let Some(path) = device {
        let parsed = std::path::Path::new(path);
        return candidates
            .iter()
            .find(|c| c.dev_path == parsed)
            .cloned()
            .or_else(|| {
                Some(DeviceCandidate {
                    dev_path: parsed.to_path_buf(),
                    vid_pid: String::new(),
                    is_metadata: false,
                })
            });
    }
    candidates
        .iter()
        .filter(|c| !c.is_metadata)
        .min_by_key(|c| node_index(&c.dev_path))
        .cloned()
}

fn node_index(path: &std::path::Path) -> u32 {
    path.file_name()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("video"))
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(u32::MAX)
}

/// Derive the metadata companion node for a given `/dev/videoN` stream device.
/// The metadata node is typically `N+1` (the next video index on the same
/// USB interface). Returns `None` when the derived path does not exist on the
/// filesystem.
fn metadata_dev_for(dev: &str) -> Option<String> {
    let n = dev
        .strip_prefix("/dev/video")
        .and_then(|s| s.parse::<u32>().ok())?;
    let meta = format!("/dev/video{}", n + 1);
    if std::path::Path::new(&meta).exists() {
        Some(meta)
    } else {
        None
    }
}

/// Turns a capture failure into something the operator can act on.
///
/// A camera held by another program is the most common way enrollment fails
/// on a working machine, and the kernel's own words for it — "ioctl failed:
/// VIDIOC_S_FMT: EBUSY: Device or resource busy" — name an ioctl rather than
/// a remedy. `CaptureError::Busy` is typed, so this says what to close and
/// how to find out what is holding the node.
fn describe_capture_error(
    dev: &str,
    stage: &str,
    e: namtor_capture::source::CaptureError,
) -> String {
    match e {
        namtor_capture::source::CaptureError::Busy(_) => format!(
            "the IR camera {dev} is in use by another program.\n  \
             Close whatever is using it — a video call, a camera app, or a running\n  \
             namtor daemon (`systemctl stop namtor`) — and try again.\n  \
             `fuser -v {dev}` names the program holding it."
        ),
        other => format!("{stage} {dev}: {other}"),
    }
}

fn capture_enrollment_frames(device: Option<&str>) -> Result<Vec<CapturedFrame>, String> {
    let dev = match device {
        Some(d) => d.to_string(),
        None => {
            let candidates = enumerate_device_candidates();
            let selected = resolve_enrollment_device(None, &candidates)
                .ok_or_else(|| "no IR camera device found".to_string())?;
            eprintln!(
                "selected IR camera {} (vid:pid {})",
                selected.dev_path.display(),
                if selected.vid_pid.is_empty() {
                    "unknown"
                } else {
                    &selected.vid_pid
                }
            );
            selected.dev_path.to_string_lossy().into_owned()
        }
    };
    if !std::path::Path::new(&dev).exists() {
        return Err(format!("{dev} not found"));
    }

    let meta_dev = metadata_dev_for(&dev);
    let vid_pid = device_vid_pid(&dev);

    let width = 640u32;
    let height = 360u32;
    let n_frames = 25usize;

    use namtor_capture::frame::PixFmt;
    use namtor_capture::ir_uvc::{IrUvc, ParityPolicy, V4l2IrStream};
    use namtor_capture::source::CaptureSource;

    let emitter = activate_emitter(&dev, &vid_pid).unwrap_or_else(|e| {
        eprintln!("warning: emitter activation failed: {e}");
        eprintln!("capturing without IR illumination (faces may not be visible)");
        EmitterGuard {
            xu: None,
            vid_pid: vid_pid.clone(),
        }
    });
    let streamer = V4l2IrStream::open(
        std::path::Path::new(&dev),
        meta_dev.as_ref().map(std::path::Path::new),
        width,
        height,
    )
    .map_err(|e| describe_capture_error(&dev, "open", e))?;

    let caps = namtor_capture::source::SourceCaps {
        spectrum: namtor_capture::source::Spectrum::Ir,
        format: PixFmt::Grey8,
        width,
        height,
        fps: 30,
        has_metadata: true,
    };

    let mut source = IrUvc::new(streamer, ParityPolicy, caps);
    source
        .start()
        .map_err(|e| describe_capture_error(&dev, "start", e))?;

    // Discard the first few frames while the strobe settles (~6 ms, but
    // the AE loop takes ~1 s / 15 pairs to stabilise per the quirk DB).
    for _ in 0..6 {
        let _ = source.next_frame();
    }

    let mut frames = Vec::new();
    while frames.len() < n_frames {
        match source.next_frame() {
            Ok(Some(frame)) => {
                let mean =
                    frame.data.iter().map(|&b| b as f32).sum::<f32>() / frame.data.len() as f32;
                if mean > 20.0 {
                    frames.push(CapturedFrame {
                        data: frame.data,
                        width,
                        height,
                    });
                }
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
    vid_pid: String,
}

impl Drop for EmitterGuard {
    fn drop(&mut self) {
        if let Some(mut xu) = self.xu.take() {
            if let Some(cfg) = emitter_cfg_for(&self.vid_pid) {
                let unit = cfg.unit.unwrap_or(0);
                let selector = cfg.selector.unwrap_or(0);
                let _ = xu.try_write(unit, selector, &cfg.payload_off);
            }
        }
    }
}

/// Look up the emitter configuration for `vid_pid` in the quirks database
/// (USA-14: no longer hardcodes "3277:0018"). Returns `None` when no quirk
/// entry exists for the given VID:PID.
fn emitter_cfg_for(vid_pid: &str) -> Option<namtor_hwdb::quirks::EmitterCfg> {
    if vid_pid.is_empty() {
        return None;
    }
    let layout = namtor_layout::LayoutPaths::resolve(
        std::path::Path::new("/"),
        std::path::Path::new("/usr/share/namtor/install-manifest.toml"),
        namtor_layout::InstallRoot::Installed,
    );
    let mut candidates = layout.quirks_dirs.clone();
    candidates.push(std::path::PathBuf::from("../quirks/devices"));
    for dir in &candidates {
        if let Ok(db) = namtor_hwdb::quirks::load_quirks_dir(dir) {
            if let Some(e) = db.entries.get(vid_pid) {
                return Some(e.emitter.clone());
            }
        }
    }
    None
}

/// Enumerate USB video devices and build a list of [`DeviceCandidate`]s with
/// their VID:PID. Used when `--device` is not given (USA-14: probe by VID:PID).
fn enumerate_device_candidates() -> Vec<DeviceCandidate> {
    let sysfs = std::path::Path::new("/sys");
    let usb_devices = match namtor_hwdb::sysfs::enumerate_usb(sysfs) {
        Ok(devs) => devs,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for usb in &usb_devices {
        let vid_pid = format!("{:04x}:{:04x}", usb.vid, usb.pid);
        if let Ok(nodes) = namtor_hwdb::nodes::correlate_nodes(usb) {
            for node in nodes {
                out.push(DeviceCandidate {
                    dev_path: node.dev_path,
                    vid_pid: vid_pid.clone(),
                    is_metadata: node.is_metadata,
                });
            }
        }
    }
    out.sort_by_key(|c| node_index(&c.dev_path));
    out
}

/// Look up the VID:PID for a `/dev/videoN` device from sysfs. Returns an
/// empty string when the device cannot be correlated to a USB device.
fn device_vid_pid(dev: &str) -> String {
    let candidates = enumerate_device_candidates();
    candidates
        .iter()
        .find(|c| c.dev_path == std::path::Path::new(dev))
        .map(|c| c.vid_pid.clone())
        .unwrap_or_default()
}

fn activate_emitter(dev: &str, vid_pid: &str) -> Result<EmitterGuard, String> {
    use namtor_uvc::device::V4l2Device;
    use namtor_uvc::probe::SafeXu;

    let cfg =
        emitter_cfg_for(vid_pid).ok_or_else(|| format!("no quirk entry for vid:pid {vid_pid}"))?;

    let v4l2_dev = V4l2Device::open(std::path::Path::new(dev))
        .map_err(|e| format!("open {dev} for XU control: {e}"))?;

    let mut xu = Box::new(SafeXu::new(v4l2_dev));

    let unit = cfg.unit.ok_or("emitter cfg missing unit")?;
    let selector = cfg.selector.ok_or("emitter cfg missing selector")?;

    xu.probe_control(unit, selector)
        .map_err(|e| format!("emitter probe failed: {e}"))?;

    let restorer = xu
        .try_write(unit, selector, &cfg.payload_on)
        .map_err(|e| format!("emitter write failed: {e}"))?;
    restorer.keep();

    Ok(EmitterGuard {
        xu: Some(xu),
        vid_pid: vid_pid.to_string(),
    })
}

#[cfg(test)]
mod tests {
    /// The busy case is the one an operator can actually fix, so the message
    /// has to name the device, say what to close, and hand over a command
    /// that identifies the holder. A bare errno does none of that.
    #[test]
    fn a_busy_camera_tells_the_operator_what_to_do() {
        let msg = super::describe_capture_error(
            "/dev/video2",
            "open",
            namtor_capture::source::CaptureError::Busy("VIDIOC_S_FMT".into()),
        );
        assert!(msg.contains("/dev/video2"), "{msg}");
        assert!(msg.contains("in use by another program"), "{msg}");
        assert!(msg.contains("fuser"), "{msg}");
        assert!(
            !msg.contains("VIDIOC_S_FMT"),
            "an ioctl name is not a remedy: {msg}"
        );
    }

    /// Everything else keeps the stage and the underlying words — there is
    /// no remedy to offer, so inventing one would be worse than the detail.
    #[test]
    fn other_capture_failures_keep_the_stage_and_the_detail() {
        let msg = super::describe_capture_error(
            "/dev/video2",
            "start",
            namtor_capture::source::CaptureError::Device("VIDIOC_STREAMON: ENODEV".into()),
        );
        assert!(msg.starts_with("start /dev/video2:"), "{msg}");
        assert!(msg.contains("ENODEV"), "{msg}");
    }

    use super::*;
    use std::path::PathBuf;

    fn candidate(dev: &str, vid_pid: &str, is_metadata: bool) -> DeviceCandidate {
        DeviceCandidate {
            dev_path: PathBuf::from(dev),
            vid_pid: vid_pid.into(),
            is_metadata,
        }
    }

    /// USA-14: `--device /dev/video4` selects `/dev/video4` even when other
    /// candidates have lower indices.
    #[test]
    fn resolve_device_honors_explicit_device_arg() {
        let candidates = vec![
            candidate("/dev/video0", "1111:2222", false),
            candidate("/dev/video2", "3277:0018", false),
            candidate("/dev/video3", "3277:0018", true),
            candidate("/dev/video4", "9999:aaaa", false),
        ];
        let selected = resolve_enrollment_device(Some("/dev/video4"), &candidates)
            .expect("explicit device must select");
        assert_eq!(selected.dev_path, PathBuf::from("/dev/video4"));
        assert_eq!(selected.vid_pid, "9999:aaaa");
    }

    /// USA-14: without `--device`, the function probes by VID:PID and picks
    /// the first non-metadata candidate by lowest `/dev/videoN` index (not
    /// the hardcoded `/dev/video2`).
    #[test]
    fn resolve_device_probes_by_vidpid_without_explicit_device() {
        let candidates = vec![
            candidate("/dev/video0", "1111:2222", false),
            candidate("/dev/video1", "1111:2222", true),
            candidate("/dev/video4", "3277:0018", false),
        ];
        let selected = resolve_enrollment_device(None, &candidates)
            .expect("probe must find a non-metadata candidate");
        assert_eq!(selected.dev_path, PathBuf::from("/dev/video0"));
        assert_eq!(selected.vid_pid, "1111:2222");
    }

    /// USA-14: metadata nodes are skipped during probing.
    #[test]
    fn resolve_device_skips_metadata_nodes_when_probing() {
        let candidates = vec![
            candidate("/dev/video2", "3277:0018", true),
            candidate("/dev/video4", "3277:0018", false),
        ];
        let selected = resolve_enrollment_device(None, &candidates)
            .expect("must skip metadata and select the non-metadata node");
        assert_eq!(selected.dev_path, PathBuf::from("/dev/video4"));
        assert!(!selected.is_metadata);
    }

    /// USA-14: when `--device` names a path not in the candidate list, the
    /// function still returns it (with an empty VID:PID) so the operator's
    /// explicit choice is respected.
    #[test]
    fn resolve_device_explicit_path_not_in_candidates_returns_empty_vidpid() {
        let candidates = vec![candidate("/dev/video0", "1111:2222", false)];
        let selected = resolve_enrollment_device(Some("/dev/video9"), &candidates)
            .expect("explicit path must always return");
        assert_eq!(selected.dev_path, PathBuf::from("/dev/video9"));
        assert_eq!(selected.vid_pid, "");
    }

    /// USA-14: when no candidates exist and no `--device` is given, returns
    /// `None`.
    #[test]
    fn resolve_device_none_when_no_candidates_and_no_explicit_device() {
        assert!(resolve_enrollment_device(None, &[]).is_none());
    }

    /// USA-14: `metadata_dev_for` derives `N+1` from `/dev/videoN`.
    #[test]
    fn metadata_dev_derives_next_index() {
        // /dev/video2 → /dev/video3 (only if the path exists on the host).
        // On a typical dev machine /dev/video2 may not exist; test the
        // pure parsing by checking that a non-existent path returns None.
        assert!(metadata_dev_for("/dev/video99999").is_none());
    }

    /// USA-14: `emitter_cfg_for` returns `None` for an empty VID:PID (no
    /// quirk lookup attempted).
    #[test]
    fn emitter_cfg_for_empty_vidpid_returns_none() {
        assert!(emitter_cfg_for("").is_none());
    }
}
