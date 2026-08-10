// SPDX-License-Identifier: GPL-3.0-only
//! CLI for the transactional PAM editor.
//!
//! Usage:
//!   namtor-pam-edit enable  --service <id> [--pamd-root <dir>] [--registry-root <dir>] [--control sufficient|default-ignore]
//!   namtor-pam-edit disable --service <id> [--pamd-root <dir>] [--registry-root <dir>]
//!   namtor-pam-edit snapshot    [--pamd-root <dir>] [--registry-root <dir>]
//!   namtor-pam-edit validate    [--pamd-root <dir>] [--registry-root <dir>]
//!   namtor-pam-edit migrate     [--pamd-root <dir>] [--registry-root <dir>] [--control sufficient|default-ignore]
//!   namtor-pam-edit disable-all [--pamd-root <dir>] [--registry-root <dir>]
//!   namtor-pam-edit purge       [--pamd-root <dir>] [--registry-root <dir>]

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use namtor_pam_config::{
    disable_all_services, disable_service, enable_service, migrate, purge, snapshot, validate,
    EditOptions, NamtorControl, ValidationStatus,
};

const LIFECYCLE_ACTIONS: &[&str] = &["snapshot", "validate", "migrate", "disable-all", "purge"];

fn print_usage() {
    eprintln!("usage: namtor-pam-edit <enable|disable> --service <id> [options]");
    eprintln!("       namtor-pam-edit <snapshot|validate|migrate|disable-all|purge> [options]");
    eprintln!();
    eprintln!("  --service <id>        PAM service ID (filename in pam.d)");
    eprintln!("  --pamd-root <dir>     PAM config root (default /etc/pam.d)");
    eprintln!("  --registry-root <dir> Nam-tor state root (default /var/lib/namtor)");
    eprintln!("  --control <c>         sufficient|default-ignore (default sufficient)");
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        return ExitCode::from(2);
    }

    let action = &args[1];
    let is_lifecycle = LIFECYCLE_ACTIONS.contains(&action.as_str());

    let mut service: Option<String> = None;
    let mut pamd_root = PathBuf::from("/etc/pam.d");
    let mut registry_root = PathBuf::from("/var/lib/namtor");
    let mut control = NamtorControl::Sufficient;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--service" => {
                service = args.get(i + 1).cloned();
                i += 2;
            }
            "--pamd-root" => {
                if let Some(v) = args.get(i + 1) {
                    pamd_root = PathBuf::from(v);
                }
                i += 2;
            }
            "--registry-root" => {
                if let Some(v) = args.get(i + 1) {
                    registry_root = PathBuf::from(v);
                }
                i += 2;
            }
            "--control" => {
                if let Some(v) = args.get(i + 1) {
                    control = match v.as_str() {
                        "sufficient" => NamtorControl::Sufficient,
                        "default-ignore" => NamtorControl::DefaultIgnore,
                        _ => {
                            eprintln!("unknown control: {v}");
                            return ExitCode::from(2);
                        }
                    };
                }
                i += 2;
            }
            "-h" | "--help" => {
                print_usage();
                return ExitCode::from(0);
            }
            _ => {
                eprintln!("unknown option: {}", args[i]);
                return ExitCode::from(2);
            }
        }
    }

    let opts = EditOptions {
        pamd_root,
        registry_root,
        control,
        crash: None,
    };

    if is_lifecycle {
        return run_lifecycle(action, &opts);
    }

    let service = match service {
        Some(s) => s,
        None => {
            eprintln!("--service is required for {action}");
            return ExitCode::from(2);
        }
    };

    let result = match action.as_str() {
        "enable" => enable_service(&service, &opts),
        "disable" => disable_service(&service, &opts),
        _ => {
            eprintln!(
                "unknown action: {action} (expected enable|disable|snapshot|validate|migrate|disable-all|purge)"
            );
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(_) => {
            eprintln!("namtor-pam-edit: {action} {service}: OK");
            ExitCode::from(0)
        }
        Err(e) => {
            eprintln!("namtor-pam-edit: {action} {service}: FAILED: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_lifecycle(action: &str, opts: &EditOptions) -> ExitCode {
    match action {
        "snapshot" => match snapshot(opts) {
            Ok(path) => {
                eprintln!("namtor-pam-edit: snapshot: OK ({}):", path.display());
                ExitCode::from(0)
            }
            Err(e) => {
                eprintln!("namtor-pam-edit: snapshot: FAILED: {e}");
                ExitCode::from(1)
            }
        },
        "validate" => match validate(opts) {
            Ok(report) => match report.status {
                ValidationStatus::Consistent => {
                    eprintln!("namtor-pam-edit: validate: OK (consistent)");
                    ExitCode::from(0)
                }
                ValidationStatus::Inconsistent => {
                    eprintln!("namtor-pam-edit: validate: INCONSISTENT");
                    for inc in &report.inconsistencies {
                        eprintln!("  {}: {}", inc.service_id, inc.kind);
                    }
                    ExitCode::from(1)
                }
            },
            Err(e) => {
                eprintln!("namtor-pam-edit: validate: FAILED: {e}");
                ExitCode::from(1)
            }
        },
        "migrate" => match migrate(opts) {
            Ok(report) => {
                eprintln!(
                    "namtor-pam-edit: migrate: OK ({} services, restored={})",
                    report.migrated_services.len(),
                    report.restored
                );
                ExitCode::from(0)
            }
            Err(e) => {
                eprintln!("namtor-pam-edit: migrate: FAILED (rolled back): {e}");
                ExitCode::from(1)
            }
        },
        "disable-all" => match disable_all_services(opts) {
            Ok(disabled) => {
                eprintln!(
                    "namtor-pam-edit: disable-all: OK ({} services)",
                    disabled.len()
                );
                ExitCode::from(0)
            }
            Err(e) => {
                eprintln!("namtor-pam-edit: disable-all: FAILED: {e}");
                ExitCode::from(1)
            }
        },
        "purge" => match purge(opts) {
            Ok(_) => {
                eprintln!("namtor-pam-edit: purge: OK");
                ExitCode::from(0)
            }
            Err(e) => {
                eprintln!("namtor-pam-edit: purge: FAILED: {e}");
                ExitCode::from(1)
            }
        },
        _ => unreachable!("lifecycle action validated upstream"),
    }
}
