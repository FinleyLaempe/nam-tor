// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-cli` binary: user-facing CLI entry point.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

use namtor_cli::status::{run_status, CameraManager, ProtectionLevel, StreamState};

#[derive(Debug, Parser)]
#[command(name = "namtor-cli", about = "Nam-tor user CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report the selected device, stream state, and protection level.
    Status,
    /// Enroll a face template. Use --user to enroll another user (requires root).
    Enroll {
        #[arg(long, default_value = "/var/lib/namtor/models")]
        models_dir: PathBuf,
        #[arg(long, default_value = "/var/lib/namtor/templates")]
        templates_dir: PathBuf,
        #[arg(long, default_value = "/var/lib/namtor/templates.key")]
        key_path: PathBuf,
        #[arg(long)]
        device: Option<String>,
        #[arg(long)]
        user: Option<String>,
    },
    /// List enrolled users.
    List {
        #[arg(long, default_value = "/var/lib/namtor/templates")]
        templates_dir: PathBuf,
        #[arg(long, default_value = "/var/lib/namtor/templates.key")]
        key_path: PathBuf,
    },
    /// Remove an enrollment.
    Remove {
        uid: u32,
        #[arg(long, default_value = "/var/lib/namtor/templates")]
        templates_dir: PathBuf,
        #[arg(long, default_value = "/var/lib/namtor/templates.key")]
        key_path: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Status => cmd_status(),
        Command::Enroll {
            models_dir,
            templates_dir,
            key_path,
            device,
            user,
        } => namtor_cli::enroll::run_enroll(&models_dir, &templates_dir, &key_path, device, user),
        Command::List {
            templates_dir,
            key_path,
        } => namtor_cli::list::run_list(&templates_dir, &key_path),
        Command::Remove {
            uid,
            templates_dir,
            key_path,
        } => namtor_cli::enroll::run_remove(uid, &templates_dir, &key_path),
    }
}

fn cmd_status() -> ExitCode {
    let mgr = CameraManager::new(None, None, None, StreamState::Closed, ProtectionLevel::Full);
    print!("{}", run_status(&mgr));
    ExitCode::SUCCESS
}
