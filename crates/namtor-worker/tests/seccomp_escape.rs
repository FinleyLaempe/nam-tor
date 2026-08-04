// SPDX-License-Identifier: GPL-3.0-only
//! Seccomp escape tests (Task 6): spawn `namtor-worker --seccomp-self-test
//! <kind>` and assert the filter denies `open`/`socket` and allows `write`.

use std::process::Command;

fn worker_bin() -> String {
    env!("CARGO_BIN_EXE_namtor-worker").to_string()
}

fn run_self_test(kind: &str) -> (std::process::ExitStatus, String) {
    let out = Command::new(worker_bin())
        .arg("--seccomp-self-test")
        .arg(kind)
        .output()
        .expect("spawn namtor-worker");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    (out.status, stdout)
}

#[test]
fn seccomp_denies_open() {
    let (status, stdout) = run_self_test("open");
    assert!(
        !status.success(),
        "open must be denied, status={status}, stdout={stdout}"
    );
    assert!(stdout.contains("errno 1"), "expected EPERM, got: {stdout}");
}

#[test]
fn seccomp_denies_socket() {
    let (status, stdout) = run_self_test("socket");
    assert!(
        !status.success(),
        "socket must be denied, status={status}, stdout={stdout}"
    );
    assert!(stdout.contains("errno 1"), "expected EPERM, got: {stdout}");
}

#[test]
fn seccomp_allows_write() {
    let (status, stdout) = run_self_test("write");
    assert!(
        status.success(),
        "write must succeed, status={status}, stdout={stdout}"
    );
    assert!(stdout.contains("ok"), "expected ok, got: {stdout}");
}
