// SPDX-License-Identifier: GPL-3.0-only
//! Combined sandbox escape suite (G5.2).
//!
//! Runs `namtor-worker --apply-sandbox --self-test <kind>` for each escape
//! kind and asserts the sandbox denies it. Positive control: `write` to an
//! inherited fd succeeds.

use std::process::Command;

fn worker_bin() -> String {
    env!("CARGO_BIN_EXE_namtor-worker").to_string()
}

fn run_self_test(kind: &str) -> (std::process::ExitStatus, String) {
    let out = Command::new(worker_bin())
        .arg("--apply-sandbox")
        .arg("--self-test")
        .arg(kind)
        .output()
        .expect("spawn namtor-worker");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    (out.status, stdout)
}

#[test]
fn denied_open_file() {
    let (status, stdout) = run_self_test("open_file");
    assert!(
        !status.success(),
        "open_file must be denied, status={status}, stdout={stdout}"
    );
    assert!(
        stdout.contains("errno 1") || stdout.contains("errno 13"),
        "expected EPERM/EACCES, got: {stdout}"
    );
}

#[test]
fn denied_open_socket() {
    let (status, stdout) = run_self_test("open_socket");
    assert!(
        !status.success(),
        "open_socket must be denied, status={status}, stdout={stdout}"
    );
    assert!(stdout.contains("errno 1"), "expected EPERM, got: {stdout}");
}

#[test]
fn denied_connect() {
    let (status, stdout) = run_self_test("connect");
    assert!(
        !status.success(),
        "connect must be denied, status={status}, stdout={stdout}"
    );
    assert!(stdout.contains("errno 1"), "expected EPERM, got: {stdout}");
}

#[test]
fn denied_exec() {
    let (status, stdout) = run_self_test("exec");
    assert!(
        !status.success(),
        "exec must be denied, status={status}, stdout={stdout}"
    );
    assert!(stdout.contains("errno 1"), "expected EPERM, got: {stdout}");
}

#[test]
fn denied_fork() {
    let (status, stdout) = run_self_test("fork");
    assert!(
        !status.success(),
        "fork must be denied, status={status}, stdout={stdout}"
    );
    assert!(stdout.contains("errno 1"), "expected EPERM, got: {stdout}");
}

#[test]
fn allowed_write_to_inherited_fd() {
    let (status, stdout) = run_self_test("write_inherited");
    assert!(
        status.success(),
        "write to inherited fd must succeed, status={status}, stdout={stdout}"
    );
    assert!(stdout.contains("ok"), "expected ok, got: {stdout}");
}

/// The worker runs as `namtor` (same UID as the daemon), which has read
/// access to `/var/lib/namtor/templates`. Seccomp denies the `open` syscall
/// itself (EPERM) before the kernel resolves the path, so the test works
/// even when the path does not exist on the CI host.
#[test]
fn denied_open_templates() {
    let (status, stdout) = run_self_test("open_templates");
    assert!(
        !status.success(),
        "open_templates must be denied, status={status}, stdout={stdout}"
    );
    assert!(
        stdout.contains("errno 1") || stdout.contains("errno 13"),
        "expected EPERM/EACCES, got: {stdout}"
    );
}

#[test]
fn denied_open_keys() {
    let (status, stdout) = run_self_test("open_keys");
    assert!(
        !status.success(),
        "open_keys must be denied, status={status}, stdout={stdout}"
    );
    assert!(
        stdout.contains("errno 1") || stdout.contains("errno 13"),
        "expected EPERM/EACCES, got: {stdout}"
    );
}

#[test]
fn denied_open_models() {
    let (status, stdout) = run_self_test("open_models");
    assert!(
        !status.success(),
        "open_models must be denied, status={status}, stdout={stdout}"
    );
    assert!(
        stdout.contains("errno 1") || stdout.contains("errno 13"),
        "expected EPERM/EACCES, got: {stdout}"
    );
}
