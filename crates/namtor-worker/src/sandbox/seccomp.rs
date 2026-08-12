// SPDX-License-Identifier: GPL-3.0-only
//! Seccomp syscall filter (deny-by-default allowlist) with W^X enforcement.
//!
//! Applied after landlock (Task 8 `apply_sandbox`); the worker mmaps its frame
//! memfd and loads models first, then calls [`apply_seccomp`] to lock the
//! syscall surface to only what the inference loop needs.
//!
//! # Allowlist (unconditional)
//!
//! `read, write, close, fcntl, fstat, lseek, munmap, brk, rt_sigaction,
//! rt_sigprocmask, rt_sigreturn, ioctl, recvfrom, sendto, shutdown, poll,
//! nanosleep, clock_gettime, getpid, exit, exit_group`.
//!
//! Deny (EPERM) by default action: `open, openat, creat, socket, connect,
//! bind, listen, accept, socketpair, execve, execveat, fork, clone, clone3`.
//!
//! `socketpair` is used pre-sandbox only (the daemon constructs the pipes);
//! the worker inherits connected fds and never opens a new socket.
//!
//! # W^X invariant (SEC-6)
//!
//! `mmap` and `mprotect` are NOT in the unconditional allowlist. They are
//! allowed only via conditional rules that check the `prot` argument (arg 2):
//! the syscall is allowed iff `(prot & PROT_EXEC) == 0`. This enforces a
//! write-xor-execute (W^X) invariant on all post-sandbox memory mappings —
//! the worker cannot introduce new executable pages after the sandbox is
//! applied, closing the classic JIT-codegen bypass of a file-only sandbox.
//!
//! Both `mmap` and `mprotect` must be conditionally filtered: denying only
//! `mprotect(PROT_EXEC)` leaves `mmap(PROT_EXEC, ...)` as a bypass to map a
//! fresh executable region. Denying only `mmap(PROT_EXEC)` leaves
//! `mprotect(..., PROT_EXEC)` as a bypass to flip an existing writable region
//! to executable. Both gates are required for the invariant to hold.
//!
//! ## Measurement (PROT_EXEC)
//!
//! `strace -f -e mmap,mprotect` on the worker with the `identity.onnx` and
//! `fc_fp32.onnx` fixtures (5 inference iterations each, `intra_threads=1`)
//! showed 0 `PROT_EXEC` mmaps/mprotects during inference. All 5 `PROT_EXEC`
//! mmaps observed occur during dynamic-linker loading (before `main`), which
//! is before the sandbox is applied. ONNX Runtime's Level3 graph
//! optimizations run at `Load` time (also pre-sandbox). The W^X filter
//! therefore does not break the inference path for the measured models.
//!
//! The three production models (YuNet, SFace, MiniFASNet) are `#[ignore]`
//! HW-gated and were not measured here. If a future ORT version or model
//! requires `PROT_EXEC` during inference, the conditional allow rule will
//! return EPERM and the failure will surface loudly via `WorkerRsp::Error` —
//! prompting re-measurement, not a silent widening.
//!
//! # ioctl narrowing (SEC-6 task 9)
//!
//! `strace -f -e ioctl` on the same fixtures showed 0 `ioctl` calls during
//! inference (all threads, 5 iterations). `ioctl` remains in the broad
//! allowlist because the measurement covers only the two fixture models;
//! the three production models are HW-gated and could exercise different ORT
//! code paths. The `--ioctl-probe <model-path>` diagnostic in the worker
//! binary enables re-measurement with the production models when HW tests
//! are run. Narrowing to a code-specific allowlist is deferred until the
//! production models are measured — this is the plan's "fallback to broad
//! allow if measurement inconclusive" path, documented here per the plan.

use libseccomp::error::SeccompError;
use libseccomp::{
    ScmpAction, ScmpArch, ScmpArgCompare, ScmpCompareOp, ScmpFilterContext, ScmpSyscall,
};

use crate::WorkerError;

const EPERM: i32 = 1;

/// `PROT_EXEC` as a `u64` for seccomp arg comparisons. On Linux this is
/// `0x4` regardless of architecture.
const PROT_EXEC: u64 = nix::libc::PROT_EXEC as u64;

fn allowlist() -> &'static [&'static str] {
    &[
        "read",
        "write",
        "close",
        "fcntl",
        "fstat",
        "lseek",
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

/// W^X conditional allow rules (SEC-6). Each entry names a syscall whose
/// `prot` argument (arg 2) is checked: the syscall is allowed only when
/// `(prot & PROT_EXEC) == 0`. A syscall appearing here MUST NOT also appear
/// in [`allowlist`] (unconditional allow) or [`denylist`] (unconditional
/// deny) — the conditional rule is the sole allow path for it.
fn wx_filtered_syscalls() -> &'static [(&'static str, &'static [ScmpArgCompare])] {
    &[("mmap", MMAP_RULES), ("mprotect", MPROTECT_RULES)]
}

/// `mmap(addr, len, prot, flags, fd, off)` — allow iff `(prot & PROT_EXEC) == 0`.
const MMAP_PROT_NOT_EXEC: ScmpArgCompare =
    ScmpArgCompare::new(2, ScmpCompareOp::MaskedEqual(PROT_EXEC), 0);

/// `mprotect(addr, len, prot)` — allow iff `(prot & PROT_EXEC) == 0`.
const MPROTECT_PROT_NOT_EXEC: ScmpArgCompare =
    ScmpArgCompare::new(2, ScmpCompareOp::MaskedEqual(PROT_EXEC), 0);

const MMAP_RULES: &[ScmpArgCompare] = &[MMAP_PROT_NOT_EXEC];
const MPROTECT_RULES: &[ScmpArgCompare] = &[MPROTECT_PROT_NOT_EXEC];

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
    // W^X conditional allow: mmap/mprotect allowed only without PROT_EXEC.
    for (name, conds) in wx_filtered_syscalls() {
        let nr = resolve(name)?;
        ctx.add_rule_conditional(ScmpAction::Allow, nr, conds)
            .map_err(seccomp_err(format!("wx allow {name}")))?;
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
        assert!(resolve("read").is_ok());
        assert!(resolve("open").is_ok());
        assert!(resolve("clone3").is_ok());
        assert!(resolve("mmap").is_ok());
        assert!(resolve("mprotect").is_ok());
    }

    /// SEC-6 structural test: `mmap` and `mprotect` are NOT in the bare
    /// allowlist — they must only be reachable via the W^X conditional rules
    /// so that a `PROT_EXEC` request hits the default EPERM action.
    #[test]
    fn mmap_and_mprotect_not_in_bare_allowlist() {
        let allow: std::collections::HashSet<_> = allowlist().iter().copied().collect();
        assert!(
            !allow.contains("mmap"),
            "mmap must not be unconditionally allowed"
        );
        assert!(
            !allow.contains("mprotect"),
            "mprotect must not be unconditionally allowed"
        );
    }

    /// SEC-6 structural test: `mmap` and `mprotect` are NOT in the denylist
    /// either — they are conditionally allowed, not unconditionally denied.
    #[test]
    fn mmap_and_mprotect_not_in_denylist() {
        let deny: std::collections::HashSet<_> = denylist().iter().copied().collect();
        assert!(!deny.contains("mmap"));
        assert!(!deny.contains("mprotect"));
    }

    /// SEC-6 structural test: the W^X ruleset covers exactly `mmap` and
    /// `mprotect`, each with a single condition on arg 2 (`prot`) that
    /// matches iff the `PROT_EXEC` bit is clear. This is the BPF-level
    /// invariant that enforces write-xor-execute.
    #[test]
    fn wx_rules_deny_prot_exec_on_arg2() {
        let rules = wx_filtered_syscalls();
        assert_eq!(rules.len(), 2, "expected exactly 2 W^X-filtered syscalls");

        let mmap = rules
            .iter()
            .find(|(name, _)| *name == "mmap")
            .expect("mmap must have a W^X conditional rule");
        assert_eq!(mmap.1.len(), 1, "mmap must have exactly one condition");
        assert_eq!(
            mmap.1[0],
            ScmpArgCompare::new(2, ScmpCompareOp::MaskedEqual(PROT_EXEC), 0),
            "mmap condition must be: arg2 & PROT_EXEC == 0"
        );

        let mprot = rules
            .iter()
            .find(|(name, _)| *name == "mprotect")
            .expect("mprotect must have a W^X conditional rule");
        assert_eq!(mprot.1.len(), 1, "mprotect must have exactly one condition");
        assert_eq!(
            mprot.1[0],
            ScmpArgCompare::new(2, ScmpCompareOp::MaskedEqual(PROT_EXEC), 0),
            "mprotect condition must be: arg2 & PROT_EXEC == 0"
        );
    }

    /// SEC-6 structural test: no syscall appears in more than one of the
    /// three lists (allowlist, denylist, W^X-filtered). A syscall in two
    /// lists would create an ambiguous filter.
    #[test]
    fn no_syscall_in_multiple_lists() {
        let allow: std::collections::HashSet<_> = allowlist().iter().copied().collect();
        let deny: std::collections::HashSet<_> = denylist().iter().copied().collect();
        for (name, _) in wx_filtered_syscalls() {
            assert!(!allow.contains(name), "{name} in both allowlist and wx");
            assert!(!deny.contains(name), "{name} in both denylist and wx");
        }
    }

    /// SEC-6 structural test: `PROT_EXEC` matches the libc constant, not a
    /// hardcoded magic number that could drift across platforms.
    #[test]
    fn prot_exec_constant_matches_libc() {
        assert_eq!(PROT_EXEC, nix::libc::PROT_EXEC as u64);
        assert_eq!(PROT_EXEC, 0x4, "PROT_EXEC is 0x4 on Linux");
    }
}
