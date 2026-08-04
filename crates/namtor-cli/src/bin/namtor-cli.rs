// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-cli` binary: user-facing CLI entry point.

use clap::{Parser, Subcommand};
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Status => cmd_status(),
    }
}

fn cmd_status() -> ExitCode {
    // The daemon is not wired yet (M3). Until then, `status` reports a
    // placeholder "no device" state so the binary is exercisable end-to-end.
    let mgr = CameraManager::new(None, None, None, StreamState::Closed, ProtectionLevel::Full);
    print!("{}", run_status(&mgr));
    ExitCode::SUCCESS
}
