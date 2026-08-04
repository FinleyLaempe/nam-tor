// SPDX-License-Identifier: GPL-3.0-only
//! Seccomp syscall filter (deny-by-default allowlist).
//!
//! Applied after landlock (Task 8 `apply_sandbox`); the worker mmaps its frame
//! memfd and loads models first, then calls [`apply_seccomp`] to lock the
//! syscall surface to only what the inference loop needs.
//!
//! Allowlist: `read, write, close, fcntl, fstat, lseek, mmap, mprotect,
//! munmap, brk, rt_sigaction, rt_sigprocmask, rt_sigreturn, ioctl, recvfrom,
//! sendto, shutdown, poll, nanosleep, clock_gettime, getpid, exit, exit_group`.
//! Deny (EPERM): `open, openat, creat, socket, connect, bind, listen, accept,
//! socketpair, execve, execveat, fork, clone, clone3`.
//!
//! `socketpair` is used pre-sandbox only (the daemon constructs the pipes);
//! the worker inherits connected fds and never opens a new socket.

use libseccomp::error::SeccompError;
use libseccomp::{ScmpAction, ScmpArch, ScmpFilterContext, ScmpSyscall};

use crate::WorkerError;

const EPERM: i32 = 1;

fn allowlist() -> &'static [&'static str] {
    &[
        "read",
        "write",
        "close",
        "fcntl",
        "fstat",
        "lseek",
        "mmap",
        "mprotect",
        "munmap",
        "brk",
        "rt_sigaction",
        "rt_sigprocmask",
        "rt_sigreturn",
        "ioctl",
        "recvfrom",
        "sendto",
        "shutdown",
        "poll",
        "nanosleep",
        "clock_gettime",
        "getpid",
        "exit",
        "exit_group",
    ]
}

fn denylist() -> &'static [&'static str] {
    &[
        "open",
        "openat",
        "creat",
        "socket",
        "connect",
        "bind",
        "listen",
        "accept",
        "socketpair",
        "execve",
        "execveat",
        "fork",
        "clone",
        "clone3",
    ]
}

fn resolve(name: &str) -> Result<ScmpSyscall, WorkerError> {
    ScmpSyscall::from_name(name)
        .map_err(|e| WorkerError::Sandbox(format!("resolve syscall {name}: {e}")))
}

/// Build and load the seccomp filter. Idempotent: calling twice is safe (the
/// second call's filter stacks on the first, which is a no-op since the first
/// already denies everything the second would deny).
pub fn apply_seccomp() -> Result<(), WorkerError> {
    let mut ctx =
        ScmpFilterContext::new(ScmpAction::Errno(EPERM)).map_err(seccomp_err("new filter"))?;
    ctx.add_arch(ScmpArch::Native)
        .map_err(seccomp_err("add arch"))?;
    for name in allowlist() {
        let nr = resolve(name)?;
        ctx.add_rule(ScmpAction::Allow, nr)
            .map_err(seccomp_err(format!("allow {name}")))?;
    }
    // Explicit deny rules are belt-and-braces; the default action (Errno EPERM)
    // already denies everything not in the allowlist. We add them best-effort
    // because libseccomp rewrites some syscalls (e.g. `open`→`openat` on
    // x86_64) and may refuse an explicit Errno rule for the rewritten name —
    // the default action still catches it.
    for name in denylist() {
        if let Ok(nr) = resolve(name) {
            let _ = ctx.add_rule(ScmpAction::Errno(EPERM), nr);
        }
    }
    ctx.load().map_err(seccomp_err("load"))?;
    Ok(())
}

fn seccomp_err(ctx: impl Into<String>) -> impl Fn(SeccompError) -> WorkerError {
    let ctx = ctx.into();
    move |e: SeccompError| WorkerError::Sandbox(format!("seccomp {ctx}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_and_denylist_are_nonempty_and_disjoint() {
        assert!(!allowlist().is_empty());
        assert!(!denylist().is_empty());
        let allow: std::collections::HashSet<_> = allowlist().iter().copied().collect();
        for d in denylist() {
            assert!(!allow.contains(d), "{d} in both lists");
        }
    }

    #[test]
    fn resolve_known_syscalls() {
        // These must resolve on x86_64 Linux.
        assert!(resolve("read").is_ok());
        assert!(resolve("open").is_ok());
        assert!(resolve("clone3").is_ok());
    }
}
