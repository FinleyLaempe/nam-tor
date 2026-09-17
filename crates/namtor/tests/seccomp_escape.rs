// SPDX-License-Identifier: GPL-3.0-only
//! Seccomp escape tests (Task 6): spawn `namtor worker --seccomp-self-test
//! <kind>` and assert the filter denies `open`/`socket` and allows `write`.
//!
//! The worker is a subcommand of the single shipped `namtor` executable, so
//! the suite drives that binary — the same file the daemon re-execs.

use std::process::Command;

fn namtor_bin() -> String {
    env!("CARGO_BIN_EXE_namtor").to_string()
}

fn run_self_test(kind: &str) -> (std::process::ExitStatus, String) {
    let out = Command::new(namtor_bin())
        .arg("worker")
        .arg("--seccomp-self-test")
        .arg(kind)
        .output()
        .expect("spawn namtor worker");
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

/// SEC-6 W^X: `mmap(PROT_EXEC)` must be denied by the conditional allow rule.
#[test]
fn seccomp_denies_mmap_exec() {
    let (status, stdout) = run_self_test("mmap_exec");
    assert!(
        !status.success(),
        "mmap(PROT_EXEC) must be denied, status={status}, stdout={stdout}"
    );
    assert!(stdout.contains("errno 1"), "expected EPERM, got: {stdout}");
}

/// SEC-6 W^X: `mprotect(PROT_EXEC)` must be denied by the conditional allow
/// rule. This closes the bypass that would exist if only `mmap` were filtered.
#[test]
fn seccomp_denies_mprotect_exec() {
    let (status, stdout) = run_self_test("mprotect_exec");
    assert!(
        !status.success(),
        "mprotect(PROT_EXEC) must be denied, status={status}, stdout={stdout}"
    );
    assert!(stdout.contains("errno 1"), "expected EPERM, got: {stdout}");
}

/// SEC-6 W^X positive control: `mmap(PROT_READ|PROT_WRITE)` (no PROT_EXEC)
/// must still succeed — the conditional allow rule passes when the PROT_EXEC
/// bit is clear.
#[test]
fn seccomp_allows_mmap_noexec() {
    let (status, stdout) = run_self_test("mmap_noexec");
    assert!(
        status.success(),
        "mmap(PROT_READ|PROT_WRITE) must succeed, status={status}, stdout={stdout}"
    );
    assert!(stdout.contains("ok"), "expected ok, got: {stdout}");
}
