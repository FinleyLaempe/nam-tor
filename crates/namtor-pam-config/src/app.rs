// SPDX-License-Identifier: GPL-3.0-only
//! CLI for the transactional PAM editor, shared by the standalone
//! `namtor-pam-edit` binary and the `namtor pam-edit` subcommand.
//!
//! The editor ships inside the multi-call `namtor` executable because the
//! install scripts need it at runtime. `packaging/install-manifest.toml`
//! never shipped `namtor-pam-edit`, so on a real system `pam-install.sh`,
//! `pam-uninstall.sh` and `lifecycle.sh` could not find it: lifecycle.sh
//! logs a warning and SKIPS, which meant `pre-remove disable-all` and
//! `post-remove purge` silently never ran and package removal left
//! `pam_namtor.so` entries behind in /etc/pam.d. namtor-daemon already links
//! this crate, so the subcommand adds no bytes to the shipped binary.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::{
    disable_all_services, disable_service, enable_service, migrate, purge, restore_service,
    snapshot, validate, EditOptions, NamtorControl, ValidationStatus,
};

const LIFECYCLE_ACTIONS: &[&str] = &["snapshot", "validate", "migrate", "disable-all", "purge"];

fn print_usage() {
    eprintln!("usage: namtor-pam-edit <enable|disable|restore> --service <id> [options]");
    eprintln!("       namtor-pam-edit <snapshot|validate|migrate|disable-all|purge> [options]");
    eprintln!();
    eprintln!("  --service <id>        PAM service ID (filename in pam.d)");
    eprintln!("  --pamd-root <dir>     PAM config root (default /etc/pam.d)");
    eprintln!("  --registry-root <dir> Nam-tor state root (default /var/lib/namtor)");
    eprintln!("  --module-root <dir>   root to search for pam_namtor.so (default /)");
    eprintln!("  --control <c>         sufficient|default-ignore (default sufficient)");
}

/// Entry point for `namtor pam-edit` and the standalone `namtor-pam-edit`.
///
/// `argv` is the full argument vector including the program name at index 0,
/// so the two spellings share one parser.
pub fn main(argv: Vec<String>) -> ExitCode {
    // Diagnostics name the spelling the operator actually used, so a message
    // from `namtor pam-edit` does not point at a binary that is not installed.
    let prog = argv
        .first()
        .map(|p| {
            std::path::Path::new(p)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone())
        })
        .unwrap_or_else(|| "namtor-pam-edit".to_string());
    let args = argv;
    if args.len() < 2 {
        print_usage();
        return ExitCode::from(2);
    }

    let action = &args[1];
    let is_lifecycle = LIFECYCLE_ACTIONS.contains(&action.as_str());

    let mut service: Option<String> = None;
    let mut pamd_root = PathBuf::from("/etc/pam.d");
    let mut registry_root = PathBuf::from("/var/lib/namtor");
    let mut module_root = PathBuf::from("/");
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
            "--module-root" => {
                if let Some(v) = args.get(i + 1) {
                    module_root = PathBuf::from(v);
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
        module_root,
        control,
        crash: None,
    };

    if is_lifecycle {
        return run_lifecycle(&prog, action, &opts);
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
        "restore" => restore_service(&service, &opts),
        _ => {
            eprintln!(
                "unknown action: {action} (expected enable|disable|restore|snapshot|validate|migrate|disable-all|purge)"
            );
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(_) => {
            eprintln!("{prog}: {action} {service}: OK");
            ExitCode::from(0)
        }
        Err(e) => {
            eprintln!("{prog}: {action} {service}: FAILED: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_lifecycle(prog: &str, action: &str, opts: &EditOptions) -> ExitCode {
    match action {
        "snapshot" => match snapshot(opts) {
            Ok(path) => {
                eprintln!("{prog}: snapshot: OK ({}):", path.display());
                ExitCode::from(0)
            }
            Err(e) => {
                eprintln!("{prog}: snapshot: FAILED: {e}");
                ExitCode::from(1)
            }
        },
        "validate" => match validate(opts) {
            Ok(report) => match report.status {
                ValidationStatus::Consistent => {
                    eprintln!("{prog}: validate: OK (consistent)");
                    ExitCode::from(0)
                }
                ValidationStatus::Inconsistent => {
                    eprintln!("{prog}: validate: INCONSISTENT");
                    for inc in &report.inconsistencies {
                        eprintln!("  {}: {}", inc.service_id, inc.kind);
                    }
                    ExitCode::from(1)
                }
            },
            Err(e) => {
                eprintln!("{prog}: validate: FAILED: {e}");
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
                eprintln!("{prog}: migrate: FAILED (rolled back): {e}");
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
                eprintln!("{prog}: disable-all: FAILED: {e}");
                ExitCode::from(1)
            }
        },
        "purge" => match purge(opts) {
            Ok(_) => {
                eprintln!("{prog}: purge: OK");
                ExitCode::from(0)
            }
            Err(e) => {
                eprintln!("{prog}: purge: FAILED: {e}");
                ExitCode::from(1)
            }
        },
        _ => unreachable!("lifecycle action validated upstream"),
    }
}
