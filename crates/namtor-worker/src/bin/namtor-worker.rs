// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-worker` binary: the sandboxed inference process.
//!
//! Task 3 wires argument parsing; Task 9 fills in the [`namtor_worker::run_main`]
//! loop. The daemon (Task 21 supervisor) spawns this binary with inherited fds.

use std::process::ExitCode;

fn main() -> ExitCode {
    // Minimal arg parse: --read-fd N --write-fd M. The full loop lands in
    // Task 9; for now we only validate the flags so the binary is callable by
    // the sandbox self-tests (Tasks 6-8).
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut read_fd: Option<i32> = None;
    let mut write_fd: Option<i32> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--read-fd" if i + 1 < args.len() => {
                read_fd = args[i + 1].parse().ok();
                i += 2;
            }
            "--write-fd" if i + 1 < args.len() => {
                write_fd = args[i + 1].parse().ok();
                i += 2;
            }
            other => {
                eprintln!("namtor-worker: unknown arg {other}");
                return ExitCode::from(2);
            }
        }
    }
    match (read_fd, write_fd) {
        (Some(r), Some(w)) => {
            eprintln!("namtor-worker: read_fd={r} write_fd={w} (loop not yet wired)");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: namtor-worker --read-fd N --write-fd M");
            ExitCode::from(2)
        }
    }
}
