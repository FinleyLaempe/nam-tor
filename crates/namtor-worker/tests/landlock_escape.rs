// SPDX-License-Identifier: GPL-3.0-only
//! Landlock escape tests (Task 7): spawn `namtor-worker --landlock-self-test
//! <kind>` and assert landlock denies `open`/`mkdir` and allows `read` on an
//! inherited pre-opened fd.

use std::process::Command;

fn worker_bin() -> String {
    env!("CARGO_BIN_EXE_namtor-worker").to_string()
}

fn run_self_test(kind: &str) -> (std::process::ExitStatus, String) {
    let out = Command::new(worker_bin())
        .arg("--landlock-self-test")
        .arg(kind)
        .output()
        .expect("spawn namtor-worker");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    (out.status, stdout)
}

#[test]
fn landlock_denies_open() {
    let (status, stdout) = run_self_test("open");
    assert!(
        !status.success(),
        "open must be denied, status={status}, stdout={stdout}"
    );
    // Landlock surfaces EACCES (13) or EPERM (1) depending on the path; both
    // are denials.
    assert!(
        stdout.contains("errno 13") || stdout.contains("errno 1"),
        "expected EACCES/EPERM, got: {stdout}"
    );
}

#[test]
fn landlock_denies_mkdir() {
    let (status, stdout) = run_self_test("mkdir");
    assert!(
        !status.success(),
        "mkdir must be denied, status={status}, stdout={stdout}"
    );
    assert!(
        stdout.contains("errno 13") || stdout.contains("errno 1"),
        "expected EACCES/EPERM, got: {stdout}"
    );
}

#[test]
fn landlock_allows_inherited_read() {
    let (status, stdout) = run_self_test("read_inherited");
    assert!(
        status.success(),
        "read on inherited fd must succeed, status={status}, stdout={stdout}"
    );
    assert!(stdout.contains("ok"), "expected ok, got: {stdout}");
}
