// SPDX-License-Identifier: GPL-3.0-only
//! Landlock filesystem and network restrictions.
//!
//! Applied before seccomp in [`crate::sandbox::apply_sandbox`] (Task 8). The
//! worker has no on-disk model — models are loaded in the daemon and frames
//! arrive via the shared `memfd` — so the worker needs no filesystem access at
//! all. Landlock denies every path for read/write/execute.
//!
//! Network bind/connect is restricted via landlock `Network` rules where the
//! kernel supports them (Linux 6.7+). The reference kernel is 7.1.x, which
//! supports landlock network rules. If the running kernel lacks net landlock,
//! the seccomp `socket`/`connect`/`bind` blocks (Task 6) are the primary net
//! barrier — this module documents that fallback and still applies the FS
//! restrictions alone.
//!
//! The worker binary's own shared libraries are already mapped before
//! `apply_landlock` runs (it is called early in `main`, after the loader has
//! finished), so denying execute access on disk paths does not break already-
//! loaded code.

use landlock::{
    Access, AccessFs, AccessNet, CompatLevel, Compatible, Ruleset, RulesetAttr, RulesetError, ABI,
};

use crate::WorkerError;

/// Apply landlock FS (and network, if the kernel supports it) restrictions.
///
/// Denies all filesystem read/write/execute and all network bind/connect by
/// handling all accesses and adding no allow rules. Stacked rulesets only get
/// stricter, so a second call cannot widen the first.
pub fn apply_landlock() -> Result<(), WorkerError> {
    let ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(AccessFs::from_all(ABI::V6))
        .map_err(landlock_err("handle fs"))?;

    // Network: try adding net restrictions. If the kernel doesn't support net
    // landlock, start over with FS-only (seccomp covers net per Task 6).
    let ruleset = match ruleset.handle_access(AccessNet::from_all(ABI::V6)) {
        Ok(r) => r,
        Err(_) => Ruleset::default()
            .set_compatibility(CompatLevel::BestEffort)
            .handle_access(AccessFs::from_all(ABI::V6))
            .map_err(landlock_err("handle fs (retry without net)"))?,
    };

    let created = ruleset.create().map_err(landlock_err("create ruleset"))?;
    let _status = created
        .restrict_self()
        .map_err(landlock_err("restrict_self"))?;
    Ok(())
}

fn landlock_err(ctx: impl Into<String>) -> impl Fn(RulesetError) -> WorkerError {
    let ctx = ctx.into();
    move |e: RulesetError| WorkerError::Sandbox(format!("landlock {ctx}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_landlock_does_not_panic() {
        // On a kernel without landlock or in CI without CAP_SYS_ADMIN, this
        // returns an error we tolerate. On the reference 7.1.x kernel it
        // succeeds. We only assert it does not panic.
        let _ = apply_landlock();
    }
}
