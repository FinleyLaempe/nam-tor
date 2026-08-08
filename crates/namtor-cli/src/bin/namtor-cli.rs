// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-cli` binary: user-facing CLI entry point.

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use namtor_cli::diagnostics::{
    collect_model_info, collect_systemd_status, create_bundle, filter_log_lines,
    get_daemon_version, run_doctor, BundleInfo, DiagnosticConfig,
};
use namtor_cli::status::{run_status, CameraManager, ProtectionLevel, StreamState};
use namtor_layout::{InstallRoot, LayoutPaths};

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
        #[arg(long)]
        models_dir: Option<PathBuf>,
        #[arg(long)]
        templates_dir: Option<PathBuf>,
        #[arg(long)]
        key_path: Option<PathBuf>,
        #[arg(long)]
        device: Option<String>,
        #[arg(long)]
        user: Option<String>,
    },
    /// Verify a face against enrolled templates via the daemon socket.
    Verify {
        #[arg(long)]
        user: Option<String>,
    },
    /// List enrolled users.
    List {
        #[arg(long)]
        templates_dir: Option<PathBuf>,
        #[arg(long)]
        key_path: Option<PathBuf>,
    },
    /// Remove an enrollment.
    Remove {
        uid: u32,
        #[arg(long)]
        templates_dir: Option<PathBuf>,
        #[arg(long)]
        key_path: Option<PathBuf>,
    },
    /// Generate a privacy-safe diagnostics support bundle.
    Diagnostics {
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
}

fn installed_layout() -> LayoutPaths {
    LayoutPaths::resolve(
        Path::new("/"),
        Path::new("/usr/share/namtor/install-manifest.toml"),
        InstallRoot::Installed,
    )
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
        } => {
            let layout = installed_layout();
            namtor_cli::enroll::run_enroll(
                &models_dir.unwrap_or(layout.models_dir),
                &templates_dir.unwrap_or(layout.templates_dir),
                &key_path.unwrap_or(layout.key_path),
                device,
                user,
            )
        }
        Command::Verify { user } => namtor_cli::verify::run_verify(user),
        Command::List {
            templates_dir,
            key_path,
        } => {
            let layout = installed_layout();
            namtor_cli::list::run_list(
                &templates_dir.unwrap_or(layout.templates_dir),
                &key_path.unwrap_or(layout.key_path),
            )
        }
        Command::Remove {
            uid,
            templates_dir,
            key_path,
        } => {
            let layout = installed_layout();
            namtor_cli::enroll::run_remove(
                uid,
                &templates_dir.unwrap_or(layout.templates_dir),
                &key_path.unwrap_or(layout.key_path),
            )
        }
        Command::Diagnostics { out_dir } => cmd_diagnostics(out_dir),
    }
}

fn cmd_status() -> ExitCode {
    let mgr = CameraManager::new(None, None, None, StreamState::Closed, ProtectionLevel::Full);
    print!("{}", run_status(&mgr));
    ExitCode::SUCCESS
}

fn cmd_diagnostics(out_dir: Option<PathBuf>) -> ExitCode {
    let cfg = DiagnosticConfig::production();
    let report = run_doctor(&cfg);
    let daemon_version = get_daemon_version(&cfg.daemon_bin);
    let systemd_status = collect_systemd_status("namtor-daemon.service");
    let model_info = collect_model_info(&cfg.models_dir);
    let raw_logs = std::process::Command::new("journalctl")
        .args(["-u", "namtor-daemon", "--no-pager", "-n", "200"])
        .output();
    let log_excerpts = match raw_logs {
        Ok(o) => filter_log_lines(&String::from_utf8_lossy(&o.stdout)),
        Err(_) => String::from("journalctl unavailable"),
    };
    let info = BundleInfo {
        cli_version: env!("CARGO_PKG_VERSION"),
        daemon_version,
        systemd_status,
        model_info,
        log_excerpts,
    };
    let out = out_dir.unwrap_or_else(|| PathBuf::from("."));
    match create_bundle(&report, &cfg, &info, &out) {
        Ok(path) => {
            println!("bundle written to {}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("diagnostics: {e}");
            ExitCode::FAILURE
        }
    }
}
