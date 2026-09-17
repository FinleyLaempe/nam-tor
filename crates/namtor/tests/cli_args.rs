// SPDX-License-Identifier: GPL-3.0-only
//! Argv contract for the single shipped `namtor` executable.
//!
//! The packaging smoke test runs `--version` and `--help` on the installed
//! binary and requires exit 0 with no daemon startup. Regression guard for the
//! release pipeline hang (runs 34802156299, 34804866795, 34807137454), now
//! covering every subcommand the merge folded in.

use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_namtor")
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(bin()).args(args).output().expect("spawn")
}

#[test]
fn top_level_version_exits_zero() {
    let out = run(&["--version"]);
    assert!(out.status.success(), "status: {:?}", out.status);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.starts_with("namtor "), "stdout: {s}");
}

#[test]
fn top_level_help_exits_zero_and_lists_commands() {
    let out = run(&["--help"]);
    assert!(out.status.success(), "status: {:?}", out.status);
    let s = String::from_utf8_lossy(&out.stdout);
    for word in ["daemon", "worker", "cli", "emitter-off"] {
        assert!(s.contains(word), "help omits {word}: {s}");
    }
}

#[test]
fn no_command_exits_two() {
    let out = run(&[]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn unknown_command_exits_two() {
    let out = run(&["--nope"]);
    assert_eq!(out.status.code(), Some(2));
    let out = run(&["frobnicate"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn daemon_version_exits_zero_without_starting() {
    let out = run(&["daemon", "--version"]);
    assert!(out.status.success(), "status: {:?}", out.status);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("namtor daemon"), "stdout: {s}");
    assert!(!s.contains("listening"), "daemon started: {s}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("starting"), "daemon started: {err}");
}

#[test]
fn daemon_help_exits_zero() {
    assert!(run(&["daemon", "--help"]).status.success());
}

#[test]
fn daemon_unknown_arg_exits_two() {
    assert_eq!(run(&["daemon", "--nope"]).status.code(), Some(2));
}

#[test]
fn worker_version_and_help_exit_zero() {
    let out = run(&["worker", "--version"]);
    assert!(out.status.success(), "status: {:?}", out.status);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("namtor worker"), "stdout: {s}");
    assert!(run(&["worker", "--help"]).status.success());
}

#[test]
fn worker_without_fds_exits_two() {
    assert_eq!(run(&["worker"]).status.code(), Some(2));
}

#[test]
fn cli_help_exits_zero() {
    let out = run(&["cli", "--help"]);
    assert!(out.status.success(), "status: {:?}", out.status);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("status"), "clap help missing subcommands: {s}");
}

#[test]
fn cli_version_exits_zero() {
    // `#[command(version)]` on the clap parser, preserved across the merge.
    let out = run(&["cli", "--version"]);
    assert!(out.status.success(), "status: {:?}", out.status);
}

#[test]
fn emitter_off_version_and_help_exit_zero() {
    let out = run(&["emitter-off", "--version"]);
    assert!(out.status.success(), "status: {:?}", out.status);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("namtor emitter-off"), "stdout: {s}");
    assert!(run(&["emitter-off", "--help"]).status.success());
}

/// No device is the SHIPPED form, not a usage error.
///
/// `ExecStopPost=/usr/bin/namtor emitter-off` passes no argument: the device
/// is re-resolved from the record the daemon writes before arming the
/// emitter, because a `/dev/videoN` literal is not stable across boots. With
/// no record there is nothing to turn off, and exiting non-zero on every
/// ordinary stop of a machine that never armed the emitter would train
/// operators to ignore the backstop.
#[test]
fn emitter_off_without_a_device_reads_the_record_and_exits_zero_when_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let absent = dir.path().join("no-such-record.toml");
    let out = run(&["emitter-off", "--state", absent.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "an absent record must be quiet: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A record that exists but cannot be read IS a failure: something armed the
/// emitter, and we could not find out how to turn it off.
#[test]
fn emitter_off_reports_a_corrupt_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("emitter-state.toml");
    std::fs::write(
        &path,
        "vid = 1
this is not toml",
    )
    .expect("write");
    let out = run(&["emitter-off", "--state", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "a corrupt record must be loud");
}

/// More than one positional is still a usage error.
#[test]
fn emitter_off_with_two_devices_exits_two() {
    assert_eq!(
        run(&["emitter-off", "/dev/video0", "/dev/video1"])
            .status
            .code(),
        Some(2)
    );
}
