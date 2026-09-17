// SPDX-License-Identifier: GPL-3.0-only
//! Standalone `namtor-pam-edit`. Kept for development and for the packaging
//! gates; shipped systems reach the same code through `namtor pam-edit`.

use std::process::ExitCode;

fn main() -> ExitCode {
    namtor_pam_config::app::main(std::env::args().collect())
}
