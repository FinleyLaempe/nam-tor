// SPDX-License-Identifier: GPL-3.0-only
//! Sandbox: seccomp + landlock self-applied by the worker before its request
//! loop.
//!
//! [`apply_sandbox`] calls landlock first, then seccomp. Landlock restricts
//! filesystem and network paths; seccomp restricts the syscall surface. The
//! order matters: landlock is applied first because it restricts paths (which
//! affects already-loaded code minimally), and seccomp is applied second
//! because it is the stricter barrier and its restrictions survive across
//! the entire process lifetime. Both are stacked (a second call cannot widen
//! the first), so `apply_sandbox` is safe to call once.

pub mod landlock;
pub mod seccomp;

use crate::WorkerError;

/// Apply landlock then seccomp. The worker must mmap its frame `memfd` and
/// load models *before* calling this — seccomp denies `mmap` on new files
/// and landlock denies opening model files from disk.
pub fn apply_sandbox() -> Result<(), WorkerError> {
    landlock::apply_landlock()?;
    seccomp::apply_seccomp()?;
    Ok(())
}
