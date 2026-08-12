// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-cli` binary: user-facing CLI entry point.

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use namtor_cli::diagnostics::{
    collect_model_info, collect_systemd_status, create_bundle, filter_log_lines,
    get_daemon_version, run_doctor, BundleInfo, DiagnosticConfig,
};
use namtor_cli::status::{run_status_command, DbusStatusSource};
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
    /// Enroll a face template. Defaults to the v3 daemon-driven continuous
    /// enrollment lifecycle (`org.namtor.Manager3`). Use `--old` for the v1
    /// local capture-and-store flow (no daemon required). Use `--user` to
    /// enroll another user (requires root).
    ///
    /// v3 lifecycle: start → live progress → press Enter to stop → review →
    /// Save (commit) / Resume (continue capture) / Discard (cancel). SIGINT
    /// discards cleanly.
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
        /// Use the v1 local capture-and-store flow instead of the v3 daemon
        /// lifecycle. Kept for compatibility and daemon-less environments.
        #[arg(long)]
        old: bool,
    },
    /// Verify a face against enrolled templates via the daemon socket.
    Verify {
        #[arg(long)]
        user: Option<String>,
    },
    /// List enrolled profiles. Defaults to the D-Bus daemon (`ListProfiles`);
    /// use `--local` for the direct-store recovery path.
    List {
        #[arg(long)]
        templates_dir: Option<PathBuf>,
        #[arg(long)]
        key_path: Option<PathBuf>,
        /// Use the direct-store path (recovery; default is the D-Bus daemon).
        #[arg(long)]
        local: bool,
    },
    /// Remove an enrollment. Defaults to the D-Bus daemon (`DeleteProfile`);
    /// use `--local` for the direct-store recovery path (deletes all profiles
    /// for the uid).
    Remove {
        uid: u32,
        /// Profile id to delete (daemon mode). Defaults to "default".
        #[arg(default_value = "default")]
        profile_id: String,
        #[arg(long)]
        templates_dir: Option<PathBuf>,
        #[arg(long)]
        key_path: Option<PathBuf>,
        /// Use the direct-store path (recovery; default is the D-Bus daemon).
        #[arg(long)]
        local: bool,
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
            old,
        } => {
            if old {
                let layout = installed_layout();
                namtor_cli::enroll::run_enroll(
                    &models_dir.unwrap_or(layout.models_dir),
                    &templates_dir.unwrap_or(layout.templates_dir),
                    &key_path.unwrap_or(layout.key_path),
                    device,
                    user,
                )
            } else {
                namtor_cli::enroll::run_enroll_v3(user)
            }
        }
        Command::Verify { user } => namtor_cli::verify::run_verify(user),
        Command::List {
            templates_dir,
            key_path,
            local,
        } => {
            if local {
                let layout = installed_layout();
                namtor_cli::list::run_list_local(
                    &templates_dir.unwrap_or(layout.templates_dir),
                    &key_path.unwrap_or(layout.key_path),
                )
            } else {
                match namtor_cli::list::DbusProfileSource::connect() {
                    Ok(source) => namtor_cli::list::run_list_daemon(&source),
                    Err(e) => {
                        eprintln!("list: {e}");
                        std::process::ExitCode::FAILURE
                    }
                }
            }
        }
        Command::Remove {
            uid,
            profile_id,
            templates_dir,
            key_path,
            local,
        } => {
            if local {
                let layout = installed_layout();
                namtor_cli::list::run_remove_local(
                    uid,
                    &templates_dir.unwrap_or(layout.templates_dir),
                    &key_path.unwrap_or(layout.key_path),
                )
            } else {
                match namtor_cli::list::DbusProfileSource::connect() {
                    Ok(source) => {
                        namtor_cli::list::run_remove_daemon(&source, uid, &profile_id)
                    }
                    Err(e) => {
                        eprintln!("remove: {e}");
                        std::process::ExitCode::FAILURE
                    }
                }
            }
        }
        Command::Diagnostics { out_dir } => cmd_diagnostics(out_dir),
    }
}

fn cmd_status() -> ExitCode {
    let source = DbusStatusSource::new();
    run_status_command(&source)
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
