// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-daemon` binary entry point.
//!
//! Supports systemd socket activation: if `LISTEN_FDS=1` is set, adopts the
//! pre-bound listener fd from systemd (`namtor-daemon.socket`). Otherwise
//! binds `/run/namtor/auth.sock` directly (dev/test path).

use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("namtor-daemon: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::path::PathBuf::from(
        std::env::var("NAMTOR_SOCKET").unwrap_or_else(|_| "/run/namtor/auth.sock".into()),
    );

    eprintln!(
        "namtor-daemon: skeleton — full lifecycle wiring in Task 26; socket at {}",
        path.display()
    );
    Ok(())
}
