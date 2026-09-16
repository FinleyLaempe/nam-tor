// SPDX-License-Identifier: GPL-3.0-only
//! Shared `--version` / `--help` handling for every Nam-tor binary.
//!
//! systemd starts the daemon with no arguments. Packaging smoke tests call
//! `--version` and `--help` and require exit 0 without side effects, so this
//! MUST run before any tracing, model loading or socket binding.

/// Handle `--version`/`-V` and `--help`/`-h`.
///
/// Returns `Some(code)` when the caller must exit immediately with `code`,
/// `None` when the caller should proceed with normal startup.
pub fn handle(bin: &str, version: &str, usage: &str, args: &[String]) -> Option<u8> {
    match args.first().map(String::as_str) {
        None => None,
        Some("--version" | "-V") => {
            println!("{bin} {version}");
            Some(0)
        }
        Some("--help" | "-h") => {
            println!("{usage}");
            Some(0)
        }
        Some(other) => {
            eprintln!("{bin}: unknown argument {other}");
            eprintln!("{usage}");
            Some(2)
        }
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
    fn unknown_arg_exits_two() {
        assert_eq!(
            handle("namtor-daemon", "0.1.0", "usage", &a(&["--wat"])),
            Some(2)
        );
    }
}
