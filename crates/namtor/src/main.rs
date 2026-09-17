// SPDX-License-Identifier: GPL-3.0-only
//! `namtor`: the single shipped Nam-tor executable.
//!
//! One file, four entry points — a multi-call binary in the busybox sense.
//! `ort`/ONNX Runtime and the recognition stack dominate the link, and the
//! daemon, the worker and the CLI all need them, so shipping one executable
//! instead of four costs roughly one copy of those bytes instead of three.
//!
//! What this merge does **not** do:
//!
//! - It does not merge `pam_namtor.so`. That is a cdylib `sudo` and `gdm`
//!   dlopen into their own address space, with a hard size budget and a
//!   structural CI gate (G5.1) proving it links no ML or image code.
//! - It does not merge `namtor-enroll-wizard`. Linking GTK4/libadwaita in
//!   here would destroy the headless-safe property: a server installs
//!   `namtor` alone for daemon + PAM + CLI, pulling no GTK and no Qt.
//! - It does not merge the worker into the daemon's **process**. `namtor
//!   daemon` re-execs `/proc/self/exe` as `namtor worker` (see
//!   `namtor_daemon::supervisor`): same file, separate process, separate
//!   address space, seccomp + Landlock applied by the child. That isolation
//!   is what keeps an inference-side compromise away from templates, keys
//!   and the PAM socket — the Howdy authentication-bypass CVE class.
//!
//! Dispatch is on the first argument, or on argv[0] when the executable was
//! invoked through a compat symlink (`namtor-cli`).

use std::process::ExitCode;

const USAGE: &str = "\
Usage: namtor <COMMAND> [ARGS...]

Nam-tor face authentication. One executable; the command selects the role.

Commands:
  daemon                 Authentication daemon (systemd ExecStart)
  worker                 Sandboxed inference worker (re-exec'd by the daemon)
  cli <SUBCOMMAND>       User-facing CLI: status, enroll, verify, list,
                         remove, diagnostics
  emitter-off <DEVICE>   Best-effort IR emitter force-off (ExecStopPost)
  pam-edit <ACTION>      Transactional /etc/pam.d editor used by the install
                         and lifecycle scripts

Options:
  -h, --help       Print this help and exit
  -V, --version    Print the version and exit

Run `namtor <COMMAND> --help` for that command's own options.";

/// The four roles this executable can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Daemon,
    Worker,
    Cli,
    EmitterOff,
    /// The transactional PAM editor. It lives here because the shipped
    /// install scripts need it at runtime and the manifest never shipped a
    /// separate `namtor-pam-edit`; `namtor-daemon` already links the crate,
    /// so this costs no additional bytes.
    PamEdit,
}

/// Map a subcommand word to a role.
fn role_from_subcommand(word: &str) -> Option<Role> {
    match word {
        "daemon" => Some(Role::Daemon),
        "worker" => Some(Role::Worker),
        "cli" => Some(Role::Cli),
        "emitter-off" => Some(Role::EmitterOff),
        "pam-edit" => Some(Role::PamEdit),
        _ => None,
    }
}

/// Map an argv[0] basename to a role, so a compat symlink keeps the old
/// spelling working. `/usr/bin/namtor-cli -> namtor` is the one symlink the
/// install manifest still ships; the other three names are recognised so an
/// operator's own symlink or an old script path behaves sanely rather than
/// printing a usage error.
fn role_from_argv0(argv0: &str) -> Option<Role> {
    let base = std::path::Path::new(argv0)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())?;
    match base.as_str() {
        "namtor-daemon" => Some(Role::Daemon),
        "namtor-worker" => Some(Role::Worker),
        "namtor-cli" => Some(Role::Cli),
        "namtor-camera-emitter-off" => Some(Role::EmitterOff),
        "namtor-pam-edit" => Some(Role::PamEdit),
        _ => None,
    }
}

/// Run `role` with `args` (the arguments after the subcommand word).
fn run(role: Role, args: &[String]) -> ExitCode {
    match role {
        Role::Daemon => namtor_daemon::app::main(args),
        Role::Worker => namtor_worker::app::main(args),
        Role::EmitterOff => namtor_camera::emitter_off_cli::main(args),
        Role::PamEdit => {
            // Its parser expects argv[0] at index 0 and the action at index 1.
            let mut argv = Vec::with_capacity(args.len() + 1);
            argv.push("namtor pam-edit".to_string());
            argv.extend_from_slice(args);
            namtor_pam_config::app::main(argv)
        }
        Role::Cli => {
            // clap wants argv[0]; give it the spelling that should appear in
            // its usage output rather than the real program path.
            let mut argv = Vec::with_capacity(args.len() + 1);
            argv.push("namtor cli".to_string());
            argv.extend_from_slice(args);
            namtor_cli::app::main(argv)
        }
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let args: Vec<String> = argv.iter().skip(1).cloned().collect();

    // A compat symlink takes precedence: `namtor-cli status` must behave
    // exactly as it did before the merge, with `status` as the first argument
    // rather than a subcommand selector.
    if let Some(role) = argv.first().and_then(|a| role_from_argv0(a)) {
        return run(role, &args);
    }

    match args.first().map(String::as_str) {
        // `--version`/`--help` before anything else: no tracing, no model
        // load, no socket bind. The release smoke test depends on it.
        Some("--version" | "-V") => {
            println!("namtor {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("namtor: missing command");
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        Some(word) => match role_from_subcommand(word) {
            Some(role) => run(role, &args[1..]),
            None => {
                eprintln!("namtor: unknown command {word}");
                eprintln!("{USAGE}");
                ExitCode::from(2)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The subcommand word for a role. Written as an exhaustive `match` on
    /// purpose: adding a `Role` variant without a subcommand word stops
    /// compiling here, which a list of `assert_eq!`s could never catch.
    fn word_for(role: Role) -> &'static str {
        match role {
            Role::Daemon => "daemon",
            Role::Worker => "worker",
            Role::Cli => "cli",
            Role::EmitterOff => "emitter-off",
            Role::PamEdit => "pam-edit",
        }
    }

    const ALL_ROLES: [Role; 5] = [
        Role::Daemon,
        Role::Worker,
        Role::Cli,
        Role::EmitterOff,
        Role::PamEdit,
    ];

    #[test]
    fn every_shipped_role_has_a_subcommand() {
        for role in ALL_ROLES {
            let word = word_for(role);
            assert_eq!(
                role_from_subcommand(word),
                Some(role),
                "subcommand {word} does not select {role:?}"
            );
        }
    }

    #[test]
    fn every_shipped_role_is_listed_in_usage() {
        for role in ALL_ROLES {
            let word = word_for(role);
            assert!(USAGE.contains(word), "usage omits {word}");
        }
    }

    #[test]
    fn unknown_subcommand_has_no_role() {
        assert_eq!(role_from_subcommand("enroll"), None);
        assert_eq!(role_from_subcommand(""), None);
    }

    #[test]
    fn compat_symlink_names_map_to_roles() {
        assert_eq!(role_from_argv0("/usr/bin/namtor-cli"), Some(Role::Cli));
        assert_eq!(
            role_from_argv0("/usr/lib/namtor/namtor-daemon"),
            Some(Role::Daemon)
        );
        assert_eq!(role_from_argv0("namtor-worker"), Some(Role::Worker));
        assert_eq!(
            role_from_argv0("namtor-camera-emitter-off"),
            Some(Role::EmitterOff)
        );
        assert_eq!(
            role_from_argv0("/usr/lib/namtor/namtor-pam-edit"),
            Some(Role::PamEdit)
        );
    }

    #[test]
    fn plain_argv0_selects_no_role() {
        assert_eq!(role_from_argv0("/usr/bin/namtor"), None);
        assert_eq!(role_from_argv0("namtor"), None);
    }

    #[test]
    fn usage_lists_every_subcommand() {
        for word in ["daemon", "worker", "cli", "emitter-off", "pam-edit"] {
            assert!(USAGE.contains(word), "usage omits {word}");
        }
    }
}
