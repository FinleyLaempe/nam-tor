// SPDX-License-Identifier: GPL-3.0-only
//! `namtor cli`: user-facing CLI entry point.
//!
//! Reached as `namtor cli <command>` (or through the `namtor-cli` compat
//! symlink, which dispatches here on argv[0]). clap owns everything after
//! the `cli` subcommand, so `#[command(version)]` and clap's own `--help`
//! keep working unchanged.

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::diagnostics::{
    collect_model_info, collect_systemd_status, create_bundle, filter_log_lines,
    get_daemon_version, run_doctor, BundleInfo, DiagnosticConfig,
};
use crate::status::{run_status_command, DbusStatusSource};
use namtor_layout::{InstallRoot, LayoutPaths};

#[derive(Debug, Parser)]
#[command(name = "namtor cli", about = "Nam-tor user CLI", version)]
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
        /// Directory holding the ONNX models. `--old` only: on the v3 path
        /// the daemon owns the models.
        #[arg(long, requires = "old")]
        models_dir: Option<PathBuf>,
        /// Directory the encrypted templates are written to. `--old` only.
        #[arg(long, requires = "old")]
        templates_dir: Option<PathBuf>,
        /// Path to the template encryption key. `--old` only.
        #[arg(long, requires = "old")]
        key_path: Option<PathBuf>,
        /// IR camera node to capture from. `--old` only: on the v3 path the
        /// daemon selects the camera from its own inventory, so accepting
        /// this flag there would silently capture from a different device.
        #[arg(long, requires = "old")]
        device: Option<String>,
        /// Enroll another user instead of yourself (requires root).
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

/// Entry point for `namtor cli`. `argv` is the full argument vector clap
/// should parse, including argv[0] (the program name shown in usage).
pub fn main(argv: Vec<String>) -> ExitCode {
    let cli = Cli::parse_from(argv);
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
                crate::enroll::run_enroll(
                    &models_dir.unwrap_or(layout.models_dir),
                    &templates_dir.unwrap_or(layout.templates_dir),
                    &key_path.unwrap_or(layout.key_path),
                    device,
                    user,
                )
            } else {
                crate::enroll::run_enroll_v3(user)
            }
        }
        Command::Verify { user } => crate::verify::run_verify(user),
        Command::List {
            templates_dir,
            key_path,
            local,
        } => {
            if local {
                let layout = installed_layout();
                crate::list::run_list_local(
                    &templates_dir.unwrap_or(layout.templates_dir),
                    &key_path.unwrap_or(layout.key_path),
                )
            } else {
                match crate::list::DbusProfileSource::connect() {
                    Ok(source) => crate::list::run_list_daemon(&source),
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
                crate::list::run_remove_local(
                    uid,
                    &templates_dir.unwrap_or(layout.templates_dir),
                    &key_path.unwrap_or(layout.key_path),
                )
            } else {
                match crate::list::DbusProfileSource::connect() {
                    Ok(source) => crate::list::run_remove_daemon(&source, uid, &profile_id),
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

#[cfg(test)]
mod arg_gating_tests {
    use super::Cli;
    use clap::Parser;

    /// `--models-dir`, `--templates-dir`, `--key-path` and `--device` only
    /// reach the v1 local flow; the v3 lifecycle takes its models, store and
    /// camera from the daemon. Accepting them on the default path meant a
    /// user who passed `--device /dev/video2` was silently captured from
    /// whichever camera the daemon picked. Requiring `--old` makes clap say
    /// so instead.
    #[test]
    fn local_flow_options_are_refused_without_old() {
        for flag in [
            ["--models-dir", "/tmp/m"],
            ["--templates-dir", "/tmp/t"],
            ["--key-path", "/tmp/k"],
            ["--device", "/dev/video2"],
        ] {
            let argv = vec!["namtor cli", "enroll", flag[0], flag[1]];
            let err = Cli::try_parse_from(argv).expect_err(&format!(
                "{} must not be silently ignored on the v3 path",
                flag[0]
            ));
            let msg = err.to_string();
            assert!(msg.contains("--old"), "{}: {msg}", flag[0]);
        }
    }

    /// With `--old` they are exactly the options the local flow needs.
    #[test]
    fn local_flow_options_are_accepted_with_old() {
        Cli::try_parse_from([
            "namtor cli",
            "enroll",
            "--old",
            "--models-dir",
            "/tmp/m",
            "--templates-dir",
            "/tmp/t",
            "--key-path",
            "/tmp/k",
            "--device",
            "/dev/video2",
        ])
        .expect("the local flow still takes its own paths");
    }

    /// `--user` works on both paths and must not have been swept up.
    #[test]
    fn user_still_works_on_the_default_path() {
        Cli::try_parse_from(["namtor cli", "enroll", "--user", "alice"])
            .expect("--user is valid on the v3 path");
    }
}
