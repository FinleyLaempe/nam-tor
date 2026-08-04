// SPDX-License-Identifier: Apache-2.0
//! `namtor-probe` binary: device classification and emitter probing CLI.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

use namtor_hwdb::sysfs::enumerate_usb;
use namtor_probe::describe::{load_quirks_db, run_describe};

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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let db = load_quirks_db(&cli.quirks_dir);

    match cli.command {
        Command::Describe => cmd_describe(&cli.sysfs_root, &db),
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
