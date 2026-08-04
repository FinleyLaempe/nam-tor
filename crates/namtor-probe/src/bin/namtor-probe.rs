// SPDX-License-Identifier: Apache-2.0
//! `namtor-probe` binary: device classification and emitter probing CLI.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

use namtor_hwdb::sysfs::enumerate_usb;
use namtor_probe::describe::{load_quirks_db, run_describe};
use namtor_probe::probe::{run_probe_live, ConsentGate, ProbeResult};

/// Hardware probe and contribution CLI.
#[derive(Debug, Parser)]
#[command(
    name = "namtor-probe",
    about = "Classify UVC IR cameras and probe emitter controls",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Sysfs root to walk instead of `/sys`.
    #[arg(long, global = true, default_value = "/sys")]
    sysfs_root: PathBuf,

    /// Quirks directory to load instead of `./quirks/devices`.
    #[arg(long, global = true, default_value = "quirks/devices")]
    quirks_dir: PathBuf,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print a human-readable classification report for every USB device.
    Describe,
    /// Probe a device's extension units for emitter control (HW-gated).
    Probe {
        /// IR V4L2 node to open, e.g. `/dev/video2`.
        #[arg(long)]
        device: PathBuf,
        /// USB VID (hex), e.g. `3277`.
        #[arg(long)]
        vid: u16,
        /// USB PID (hex), e.g. `0018`.
        #[arg(long)]
        pid: u16,
        /// IR VideoControl interface (where the extension units live).
        #[arg(long, default_value_t = 2)]
        ir_vc_interface: u8,
        /// Auto-approve the vendor-XU probe tier without prompting.
        #[arg(long)]
        yes: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let db = load_quirks_db(&cli.quirks_dir);

    match cli.command {
        Command::Describe => cmd_describe(&cli.sysfs_root, &db),
        Command::Probe {
            device,
            vid,
            pid,
            ir_vc_interface,
            yes,
        } => cmd_probe(
            &cli.sysfs_root,
            &db,
            &device,
            vid,
            pid,
            ir_vc_interface,
            yes,
        ),
    }
}

fn cmd_describe(sysfs_root: &std::path::Path, db: &namtor_hwdb::quirks::QuirksDb) -> ExitCode {
    let devices = match enumerate_usb(sysfs_root) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: enumerating USB under {}: {e}", sysfs_root.display());
            return ExitCode::FAILURE;
        }
    };

    if devices.is_empty() {
        eprintln!("no USB devices found under {}", sysfs_root.display());
        return ExitCode::SUCCESS;
    }

    let mut printed = 0;
    for usb in &devices {
        match run_describe(usb, db) {
            Ok(report) => {
                if printed > 0 {
                    println!();
                }
                println!("{report}");
                printed += 1;
            }
            Err(e) => {
                eprintln!(
                    "warn: {:04x}:{:04x}: classification failed: {e}",
                    usb.vid, usb.pid
                );
            }
        }
    }

    if printed == 0 {
        eprintln!("no classifiable UVC IR devices found");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Consent gate that auto-approves the vendor probe tier when `--yes` is set,
/// and otherwise prompts on stdin.
struct CliConsent {
    yes: bool,
}

impl ConsentGate for CliConsent {
    fn request_vendor_probe(&self, vid: u16, pid: u16) -> bool {
        if self.yes {
            return true;
        }
        println!(
            "Probing vendor extension units on {vid:04x}:{pid:04x} writes blindly.\n\
             A non-compliant device may hang and need a USB replug.\n\
             Continue? [y/N] "
        );
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() {
            return false;
        }
        answer.trim().eq_ignore_ascii_case("y")
    }
}

fn cmd_probe(
    sysfs_root: &std::path::Path,
    db: &namtor_hwdb::quirks::QuirksDb,
    device: &std::path::Path,
    vid: u16,
    pid: u16,
    ir_vc_interface: u8,
    yes: bool,
) -> ExitCode {
    let devices = match enumerate_usb(sysfs_root) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: enumerating USB under {}: {e}", sysfs_root.display());
            return ExitCode::FAILURE;
        }
    };

    let Some(usb) = devices.iter().find(|u| u.vid == vid && u.pid == pid) else {
        eprintln!("error: no USB device {vid:04x}:{pid:04x} found under {sysfs_root:?}");
        return ExitCode::FAILURE;
    };

    let c = match namtor_hwdb::pipeline::classify(usb, db) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: classification failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let ir_xus: Vec<_> = c
        .xu_units
        .iter()
        .filter(|xu| xu.interface == ir_vc_interface)
        .cloned()
        .collect();

    if ir_xus.is_empty() {
        eprintln!("error: no extension units on IR VideoControl interface {ir_vc_interface}");
        return ExitCode::FAILURE;
    }

    let consent = CliConsent { yes };
    match run_probe_live(device, &ir_xus, vid, pid, &consent) {
        Ok(ProbeResult {
            vid_pid,
            method,
            unit,
            selector,
            payload_on,
            payload_off,
            mode,
            evidence,
        }) => {
            println!("probe result for {vid_pid}:");
            println!("  method    = {method:?}");
            println!("  unit      = {unit}");
            println!("  selector  = {selector}");
            println!("  payload_on  = {payload_on:02x?}");
            println!("  payload_off = {payload_off:02x?}");
            if let Some(m) = mode {
                println!("  mode      = {m}");
            }
            println!("  evidence  = {evidence}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: probe failed: {e}");
            ExitCode::FAILURE
        }
    }
}
