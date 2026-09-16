// SPDX-License-Identifier: GPL-3.0-only
//! Shared `--version` / `--help` handling for every Nam-tor binary.
//!
//! systemd starts the daemon with no arguments. Packaging smoke tests call
//! `--version` and `--help` and require exit 0 without side effects, so this
//! MUST run before any tracing, model loading or socket binding.

/// Handle `--version`/`-V` and `--help`/`-h`, rejecting anything else.
///
/// Returns `Some(code)` when the caller must exit immediately with `code`,
/// `None` when the caller should proceed with normal startup. For binaries
/// that take no other arguments (`namtor-daemon`, `namtor-enroll-wizard`);
/// binaries with their own parser use [`handle_known`] instead.
pub fn handle(bin: &str, version: &str, usage: &str, args: &[String]) -> Option<u8> {
    if let Some(code) = handle_known(bin, version, usage, args) {
        return Some(code);
    }
    match args.first().map(String::as_str) {
        None => None,
        Some(other) => {
            eprintln!("{bin}: unknown argument {other}");
            eprintln!("{usage}");
            Some(2)
        }
    }
}

/// Handle `--version`/`-V` and `--help`/`-h`, leaving every other argument
/// to the caller.
///
/// `namtor-worker` and `namtor-camera-emitter-off` parse their own flags, so
/// they must not hand them to [`handle`], which exits 2 on the first argument
/// it does not recognise.
pub fn handle_known(bin: &str, version: &str, usage: &str, args: &[String]) -> Option<u8> {
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("{bin} {version}");
            Some(0)
        }
        Some("--help" | "-h") => {
            println!("{usage}");
            Some(0)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_args_continues() {
        assert_eq!(handle("namtor-daemon", "0.1.0", "usage", &a(&[])), None);
    }

    #[test]
    fn version_exits_zero() {
        assert_eq!(
            handle("namtor-daemon", "0.1.0", "usage", &a(&["--version"])),
            Some(0)
        );
        assert_eq!(
            handle("namtor-daemon", "0.1.0", "usage", &a(&["-V"])),
            Some(0)
        );
    }

    #[test]
    fn help_exits_zero() {
        assert_eq!(
            handle("namtor-daemon", "0.1.0", "usage", &a(&["--help"])),
            Some(0)
        );
        assert_eq!(
            handle("namtor-daemon", "0.1.0", "usage", &a(&["-h"])),
            Some(0)
        );
    }

    #[test]
    fn handle_known_ignores_unknown_args() {
        assert_eq!(
            handle_known("namtor-worker", "0.1.0", "usage", &a(&["--read-fd", "3"])),
            None
        );
        assert_eq!(
            handle_known("namtor-worker", "0.1.0", "usage", &a(&["--version"])),
            Some(0)
        );
        assert_eq!(
            handle_known("namtor-worker", "0.1.0", "usage", &a(&[])),
            None
        );
    }

    #[test]
    fn unknown_arg_exits_two() {
        assert_eq!(
            handle("namtor-daemon", "0.1.0", "usage", &a(&["--wat"])),
            Some(2)
        );
    }
}
