// SPDX-License-Identifier: Apache-2.0
//! Hardware spike CLI. Answers the M1 open risks against a real camera.
//!
//! WARNING: `emitter` writes to vendor extension unit controls. Every write is
//! preceded by a read and followed by a restore, but a non-compliant device
//! can still misbehave. Requires explicit confirmation.

use namtor_capture::frame::{EmitterState, Frame, PixFmt};
use namtor_uvc::descriptors::{format_guid, is_l8_ir_format, parse_descriptors};
use namtor_uvc::device::V4l2Device;
use namtor_uvc::discover::{discover_emitter, DiscoverCfg, FrameSampler};
use namtor_uvc::mode::{find_mode_controls, measure_alternation};
use namtor_uvc::probe::{SafeXu, XuError};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

/// Samples frames by shelling out to `v4l2-ctl --stream-mmap`.
///
/// Deliberately not the V4L2 `read()` API: most UVC devices support only
/// mmap streaming, and `read()` would fail on them — which would block the
/// single most important measurement in the project. Subprocess overhead of
/// a few hundred milliseconds per sample is irrelevant for a spike run a few
/// dozen times. M2's capture layer does mmap streaming in-process.
struct V4l2CtlSampler {
    device: PathBuf,
    width: u32,
    height: u32,
    started: Instant,
    scratch: PathBuf,
}

impl FrameSampler for V4l2CtlSampler {
    fn sample(&mut self, n: usize) -> Result<Vec<Frame>, XuError> {
        let fmt = format!(
            "width={},height={},pixelformat=GREY",
            self.width, self.height
        );
        let out = Command::new("v4l2-ctl")
            .arg("-d")
            .arg(&self.device)
            .arg(format!("--set-fmt-video={fmt}"))
            .arg("--stream-mmap")
            .arg(format!("--stream-count={n}"))
            .arg(format!("--stream-to={}", self.scratch.display()))
            .output()
            .map_err(|e| XuError::Ioctl(format!("spawn v4l2-ctl: {e}")))?;

        if !out.status.success() {
            return Err(XuError::Ioctl(format!(
                "v4l2-ctl failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }

        let raw = std::fs::read(&self.scratch)
            .map_err(|e| XuError::Ioctl(format!("read capture: {e}")))?;
        let len = (self.width * self.height) as usize;

        if raw.len() < len {
            return Err(XuError::Ioctl(format!(
                "captured {} bytes, need at least {len} for one frame",
                raw.len()
            )));
        }

        Ok(raw
            .chunks_exact(len)
            .take(n)
            .enumerate()
            .map(|(i, chunk)| Frame {
                data: chunk.to_vec(),
                format: PixFmt::Grey8,
                width: self.width,
                height: self.height,
                ts_mono: self.started.elapsed() + Duration::from_millis(33 * i as u64),
                sof: None,
                emitter: EmitterState::Unknown,
                quality: None,
            })
            .collect())
    }
}

fn cmd_descriptors(path: PathBuf) -> ExitCode {
    let blob = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: read {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let set = parse_descriptors(&blob);

    println!("Extension units: {}", set.extension_units.len());
    for xu in &set.extension_units {
        println!(
            "  if{} unit {:>3}  {}  bmControls={:02x?}",
            xu.interface,
            xu.unit_id,
            format_guid(&xu.guid),
            xu.bm_controls
        );
    }

    println!("Formats: {}", set.formats.len());
    for f in &set.formats {
        println!(
            "  if{} index {}  {}{}",
            f.interface,
            f.format_index,
            format_guid(&f.guid),
            if is_l8_ir_format(&f.guid) {
                "   <- IR (L8)"
            } else {
                ""
            }
        );
    }
    ExitCode::SUCCESS
}

fn cmd_emitter(
    device: PathBuf,
    descriptors: PathBuf,
    ir_vc_interface: u8,
    assume_yes: bool,
) -> ExitCode {
    if !assume_yes {
        println!(
            "This probes vendor extension unit controls by writing to them.\n\
             Every write is read first and restored after, but a non-compliant\n\
             device may still hang and require a USB reset or reboot.\n\
             Continue? [y/N] "
        );
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err()
            || !answer.trim().eq_ignore_ascii_case("y")
        {
            println!("aborted");
            return ExitCode::SUCCESS;
        }
    }

    let blob = match std::fs::read(&descriptors) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: read {}: {e}", descriptors.display());
            return ExitCode::FAILURE;
        }
    };
    let set = parse_descriptors(&blob);

    // CRITICAL: a multi-sensor camera declares one set of extension units per
    // VideoControl interface. Probing the colour sensor's units would poke the
    // wrong device. Keep only the IR sensor's VideoControl interface.
    const MSXU_PREFIX: &str = "0f3f95dc-2632-4c4e";
    let mut units: Vec<_> = set
        .extension_units
        .iter()
        .filter(|xu| xu.interface == ir_vc_interface)
        .cloned()
        .collect();

    // MSXU first: standardised semantics, so tier 1 probes it silently
    // (spec §4.10.1). Vendor units are the consented tier-2 fallback.
    units.sort_by_key(|xu| !format_guid(&xu.guid).starts_with(MSXU_PREFIX));

    println!(
        "probing {} extension unit(s) on VideoControl interface {}:",
        units.len(),
        ir_vc_interface
    );
    for xu in &units {
        println!(
            "  unit {:<3} {}  advertised selectors {:?}",
            xu.unit_id,
            format_guid(&xu.guid),
            xu.advertised_selectors()
        );
    }

    let transport = match V4l2Device::open(&device) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut xu = SafeXu::new(transport);

    if Command::new("v4l2-ctl").arg("--version").output().is_err() {
        eprintln!("error: v4l2-ctl not found (pkg: v4l-utils); frame sampling needs it");
        return ExitCode::FAILURE;
    }

    let mut sampler = V4l2CtlSampler {
        device: device.clone(),
        width: 640,
        height: 360,
        started: Instant::now(),
        scratch: std::env::temp_dir().join("namtor-spike-frames.raw"),
    };

    let started = Instant::now();
    let found = discover_emitter(&mut xu, &mut sampler, &units, &DiscoverCfg::default());
    let elapsed = started.elapsed();

    println!("\n=== R3: emitter controllability ===");
    if found.is_empty() {
        println!("NO EMITTER CONTROL FOUND after {elapsed:?}");
        println!("This is spike finding R3-FAIL. Liveness design must be revisited.");
        return ExitCode::FAILURE;
    }
    for c in &found {
        println!(
            "unit {} selector {:#04x}  delta={:.1}  centre_weighted={}  \
             on={:02x?} off={:02x?}",
            c.unit,
            c.selector,
            c.evidence.delta,
            c.evidence.centre_weighted,
            c.payload_on,
            c.payload_off
        );
    }
    println!("R3-PASS in {elapsed:?}");
    ExitCode::SUCCESS
}

fn cmd_modes(
    device: PathBuf,
    descriptors: PathBuf,
    ir_vc_interface: u8,
    assume_yes: bool,
) -> ExitCode {
    if !assume_yes {
        println!(
            "This sets the camera's MSXU mode control to each value it accepts,\n\
             capturing a burst after each to detect firmware-driven strobing.\n\
             Every value is restored after testing. A non-compliant device may\n\
             still need a USB replug.\n\
             Continue? [y/N] "
        );
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err()
            || !answer.trim().eq_ignore_ascii_case("y")
        {
            println!("aborted");
            return ExitCode::SUCCESS;
        }
    }

    let blob = match std::fs::read(&descriptors) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: read {}: {e}", descriptors.display());
            return ExitCode::FAILURE;
        }
    };

    let units: Vec<_> = parse_descriptors(&blob)
        .extension_units
        .into_iter()
        .filter(|xu| xu.interface == ir_vc_interface)
        .collect();

    let transport = match V4l2Device::open(&device) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut xu = SafeXu::new(transport);
    let mut sampler = V4l2CtlSampler {
        device: device.clone(),
        width: 640,
        height: 360,
        started: Instant::now(),
        scratch: std::env::temp_dir().join("namtor-spike-modes.raw"),
    };

    let controls = find_mode_controls(&mut xu, &units);
    println!("mode-like controls found: {}", controls.len());

    let mut winners = Vec::new();

    for ctrl in &controls {
        println!(
            "\nunit {} selector {} len {}  CUR {:02x?}  MAX {:02x?}",
            ctrl.unit, ctrl.selector, ctrl.len, ctrl.current, ctrl.max
        );

        // Baseline at the current value, so a device that already alternates
        // is not credited to a mode change.
        match sampler.sample(16) {
            Ok(f) => {
                let ev = measure_alternation(&f);
                println!(
                    "  baseline           alternation={:.1} alternates={}",
                    ev.alternation, ev.alternates
                );
            }
            Err(e) => println!("  baseline capture failed: {e}"),
        }

        for payload in ctrl.mode_candidates() {
            let Ok(restorer) = xu.try_write(ctrl.unit, ctrl.selector, &payload) else {
                println!("  {payload:02x?}  rejected by device");
                continue;
            };

            let frames = sampler.sample(16).unwrap_or_default();
            drop(restorer); // restores the previous value

            if frames.is_empty() {
                println!("  {payload:02x?}  no frames captured");
                continue;
            }

            let ev = measure_alternation(&frames);
            let luma = frames
                .iter()
                .map(|f| namtor_uvc::emitter::luma_stats(f).mean)
                .sum::<f64>()
                / frames.len() as f64;

            println!(
                "  {payload:02x?}  mean={luma:.1} alternation={:.1} on={:.1} off={:.1} \
                 alternates={}",
                ev.alternation, ev.on_mean, ev.off_mean, ev.alternates
            );

            if ev.alternates {
                winners.push((ctrl.clone(), payload.clone(), ev));
            }
        }
    }

    println!("\n=== TIER 0: device-driven illumination ===");
    if winners.is_empty() {
        println!("No mode produced firmware alternation.");
        println!("Fall back to Tier 1 (`namtor-spike emitter`): userspace strobe.");
        return ExitCode::SUCCESS;
    }
    for (ctrl, payload, ev) in &winners {
        println!(
            "TIER-0-PASS  unit {} selector {} payload {:02x?}  \
             alternation={:.1} (on {:.1} / off {:.1})",
            ctrl.unit, ctrl.selector, payload, ev.alternation, ev.on_mean, ev.off_mean
        );
    }
    println!("\nThe camera strobes the emitter itself, as it does for Windows Hello.");
    println!("Torn-frame handling (spec §4.4) is unnecessary and risk R2 is void.");
    ExitCode::SUCCESS
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  \
         namtor-spike descriptors --path <file>\n  \
         namtor-spike emitter --device <node> --descriptors <file> \
         [--ir-vc-interface N] [--yes]\n  \
         namtor-spike modes --device <node> --descriptors <file> \
         [--ir-vc-interface N] [--yes]\n\n\
         --ir-vc-interface defaults to 2 (the reference camera's IR\n\
         VideoControl interface). Find it with `namtor-spike descriptors`:\n\
         it is the interface whose extension units accompany the L8 format."
    );
    std::process::exit(2);
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }

    let mut path: Option<PathBuf> = None;
    let mut device: Option<PathBuf> = None;
    let mut descriptors: Option<PathBuf> = None;
    let mut ir_vc_interface: u8 = 2;
    let mut assume_yes = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--path" => {
                i += 1;
                path = Some(PathBuf::from(args.get(i).unwrap_or_else(|| usage())));
            }
            "--device" => {
                i += 1;
                device = Some(PathBuf::from(args.get(i).unwrap_or_else(|| usage())));
            }
            "--descriptors" => {
                i += 1;
                descriptors = Some(PathBuf::from(args.get(i).unwrap_or_else(|| usage())));
            }
            "--ir-vc-interface" => {
                i += 1;
                ir_vc_interface = args
                    .get(i)
                    .unwrap_or_else(|| usage())
                    .parse()
                    .unwrap_or_else(|_| usage());
            }
            "--yes" => assume_yes = true,
            _ => usage(),
        }
        i += 1;
    }

    match args[0].as_str() {
        "descriptors" => cmd_descriptors(path.unwrap_or_else(|| usage())),
        "emitter" => cmd_emitter(
            device.unwrap_or_else(|| usage()),
            descriptors.unwrap_or_else(|| usage()),
            ir_vc_interface,
            assume_yes,
        ),
        "modes" => cmd_modes(
            device.unwrap_or_else(|| usage()),
            descriptors.unwrap_or_else(|| usage()),
            ir_vc_interface,
            assume_yes,
        ),
        _ => usage(),
    }
}
