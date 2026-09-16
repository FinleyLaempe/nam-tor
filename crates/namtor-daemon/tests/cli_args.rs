// SPDX-License-Identifier: GPL-3.0-only
//! The packaging smoke test runs `namtor-daemon --version` and `--help` and
//! requires exit 0 with no daemon startup. Regression guard for the release
//! pipeline hang (runs 34802156299, 34804866795, 34807137454).

use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_namtor-daemon")
}

#[test]
fn version_exits_zero_without_starting() {
    let out = Command::new(bin())
        .arg("--version")
        .output()
        .expect("spawn");
    assert!(out.status.success(), "status: {:?}", out.status);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("namtor-daemon"), "stdout: {s}");
    assert!(!s.contains("listening"), "daemon started: {s}");
}

#[test]
fn help_exits_zero() {
    let out = Command::new(bin()).arg("--help").output().expect("spawn");
    assert!(out.status.success());
}

#[test]
fn unknown_arg_exits_two() {
    let out = Command::new(bin()).arg("--nope").output().expect("spawn");
    assert_eq!(out.status.code(), Some(2));
}
