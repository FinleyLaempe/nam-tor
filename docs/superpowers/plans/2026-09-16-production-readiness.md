# Nam-tor Production Readiness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship Nam-tor as a production-ready product: a green release pipeline, one desktop-neutral GTK4 UI that actually renders, and an enrollment scan screen whose quality guidance is derived from real measurements instead of fabricated constants.

**Architecture:** Three independent workstreams. (P1) Fix the release pipeline and the two production daemon bugs it exposed. (P2) Collapse two desktop-specific UIs into one desktop-neutral `namtor-ui` crate and fix the window that never displays. (P3) Replace the fabricated `CaptureObservation` with real per-frame measurement, and build the scan screen on top of it. No live video preview — raw frames never leave the daemon.

**Tech Stack:** Rust 2021, GTK4 0.9 + libadwaita 0.7 (v1_5), zbus 5, ONNX Runtime via `ort`, systemd, polkit, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-08-04-nam-tor-design.md`

## Global Constraints

- TDD: failing test -> minimal impl -> green -> commit, per task.
- SPDX header on every `.rs`. `namtor-hwdb`/`namtor-probe`/`quirks/` are Apache-2.0; everything else GPL-3.0-only.
- No `unwrap()`/`expect()` in non-test code. `unsafe` only for FFI, with a SAFETY comment.
- Raw frames, embeddings, match scores and thresholds MUST NOT reach the UI. `crates/namtor-gnome/src/privacy.rs` `FORBIDDEN_SUBSTRINGS` is the enforcement point and stays intact.
- Any new D-Bus field name must avoid the substrings: `score`, `landmark`, `pixel`, `threshold`, `vector`, `embedding`, `template`, `key`, `nonce`, `biometric`.
- PAD is a veto, never a weighted term. No GPU execution provider in the auth path.
- HW-gated tests are `#[ignore]` with reason `"requires the reference camera; run with NAMTOR_HW_TEST=1"`.
- Reference hardware present on the dev box: Sonix `3277:0018`, IR at `/dev/video2` (GREY 640x360@30), RGB at `/dev/video0`, TPM2 at `/dev/tpm0`.
- Baseline before this plan: 1512 tests passing, 0 failing, 13 ignored.

---

# Phase 1 — Release pipeline green

### Task 1.1: `--version` / `--help` preamble shared by every binary

**Files:**
- Create: `crates/namtor-layout/src/cli_preamble.rs`
- Modify: `crates/namtor-layout/src/lib.rs`
- Test: `crates/namtor-layout/src/cli_preamble.rs` (unit tests inline)

**Interfaces:**
- Produces: `namtor_layout::cli_preamble::handle(bin: &str, version: &str, usage: &str, args: &[String]) -> Option<u8>` — returns `Some(exit_code)` when the process should exit immediately, `None` when it should continue.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn no_args_continues() {
        assert_eq!(handle("namtor-daemon", "0.1.0", "usage", &a(&[])), None);
    }

    #[test]
    fn version_exits_zero() {
        assert_eq!(handle("namtor-daemon", "0.1.0", "usage", &a(&["--version"])), Some(0));
        assert_eq!(handle("namtor-daemon", "0.1.0", "usage", &a(&["-V"])), Some(0));
    }

    #[test]
    fn help_exits_zero() {
        assert_eq!(handle("namtor-daemon", "0.1.0", "usage", &a(&["--help"])), Some(0));
        assert_eq!(handle("namtor-daemon", "0.1.0", "usage", &a(&["-h"])), Some(0));
    }

    #[test]
    fn unknown_arg_exits_two() {
        assert_eq!(handle("namtor-daemon", "0.1.0", "usage", &a(&["--wat"])), Some(2));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-layout cli_preamble`
Expected: FAIL — module `cli_preamble` does not exist.

- [ ] **Step 3: Write minimal implementation**

```rust
// SPDX-License-Identifier: GPL-3.0-only
//! Shared `--version` / `--help` handling for every Nam-tor binary.
//!
//! systemd starts the daemon with no arguments. Packaging smoke tests call
//! `--version` and `--help` and require exit 0 without side effects, so this
//! MUST run before any tracing, model loading or socket binding.

/// Handle `--version`/`-V` and `--help`/`-h`.
///
/// Returns `Some(code)` when the caller must exit immediately with `code`,
/// `None` when the caller should proceed with normal startup.
pub fn handle(bin: &str, version: &str, usage: &str, args: &[String]) -> Option<u8> {
    match args.first().map(String::as_str) {
        None => None,
        Some("--version" | "-V") => {
            println!("{bin} {version}");
            Some(0)
        }
        Some("--help" | "-h") => {
            println!("{usage}");
            Some(0)
        }
        Some(other) => {
            eprintln!("{bin}: unknown argument {other}");
            eprintln!("{usage}");
            Some(2)
        }
    }
}
```

Add to `crates/namtor-layout/src/lib.rs`:

```rust
pub mod cli_preamble;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-layout cli_preamble`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-layout/src/cli_preamble.rs crates/namtor-layout/src/lib.rs
git commit -m "feat(layout): shared --version/--help preamble for all binaries"
```

---

### Task 1.2: Wire the preamble into all five binaries

**Files:**
- Modify: `crates/namtor-daemon/src/bin/namtor-daemon.rs:48` (`fn main`)
- Modify: `crates/namtor-worker/src/bin/namtor-worker.rs:12` (`fn main`, before the `other =>` arm at ~L71)
- Modify: `crates/namtor-camera/src/bin/namtor-camera-emitter-off.rs:11` (`fn main`, before the `args.len()` guard at ~L13)
- Modify: `crates/namtor-gnome/src/bin/wizard.rs:18` (`fn main`, before `adw::Application::builder()`)
- Test: `crates/namtor-daemon/tests/cli_args.rs` (create)

**Interfaces:**
- Consumes: `namtor_layout::cli_preamble::handle` from Task 1.1.

`namtor-cli` already handles both via clap `#[command(version)]` — leave it alone.

- [ ] **Step 1: Write the failing test**

```rust
// SPDX-License-Identifier: GPL-3.0-only
//! The packaging smoke test runs `namtor-daemon --version` and `--help` and
//! requires exit 0 with no daemon startup. Regression guard for the release
//! pipeline hang (runs 34802156299, 34804866795, 34807137454).

use std::process::Command;

fn bin() -> &'static str { env!("CARGO_BIN_EXE_namtor-daemon") }

#[test]
fn version_exits_zero_without_starting() {
    let out = Command::new(bin()).arg("--version").output().expect("spawn");
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-daemon --test cli_args`
Expected: FAIL/HANG — the daemon boots and binds a socket instead of printing a version.

- [ ] **Step 3: Write minimal implementation**

In `crates/namtor-daemon/src/bin/namtor-daemon.rs`, make `main` start with the guard, before `init_tracing()`:

```rust
const USAGE: &str = "\
Usage: namtor-daemon

Nam-tor authentication daemon. Started by systemd with no arguments.

Options:
  -h, --help       Print this help and exit
  -V, --version    Print the version and exit";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) =
        namtor_layout::cli_preamble::handle("namtor-daemon", env!("CARGO_PKG_VERSION"), USAGE, &args)
    {
        return ExitCode::from(code);
    }
    init_tracing();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "daemon startup failed");
            ExitCode::FAILURE
        }
    }
}
```

Apply the same guard as the first statement of `main` in `namtor-worker`, `namtor-camera-emitter-off` and `namtor-enroll-wizard`, each with its own `USAGE` string. For the wizard the guard MUST precede `adw::Application::builder()` so `--version` works headlessly.

Add `namtor-layout = { path = "../namtor-layout" }` to the `[dependencies]` of `namtor-worker`, `namtor-camera` and `namtor-gnome` if not already present.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-daemon --test cli_args`
Expected: PASS, 3 tests.
Then: `for b in namtor-daemon namtor-worker namtor-camera-emitter-off namtor-enroll-wizard; do ./target/debug/$b --version || echo "FAIL $b"; done`
Expected: each prints `<name> <version>` and exits 0.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-daemon crates/namtor-worker crates/namtor-camera crates/namtor-gnome
git commit -m "fix(cli): handle --version/--help in every binary; daemon no longer boots on --version"
```

---

### Task 1.3: Make the daemon accept loop interruptible (SIGTERM actually stops it)

**Files:**
- Modify: `crates/namtor-daemon/src/listener.rs:108-120` (`accept_one`)
- Modify: `crates/namtor-daemon/src/bin/namtor-daemon.rs:246-260` (signal handler + main loop)
- Test: `crates/namtor-daemon/src/listener.rs` (inline test)

**Interfaces:**
- Produces: `Listener::accept_one(&self) -> Result<Option<UnixStream>, ListenerError>` — `Ok(None)` now means "interrupted, re-check shutdown flag" rather than blocking forever.
- Produces: `Listener::set_poll_timeout(&mut self, d: Duration)`.

Background: `std::os::unix::net::UnixListener::accept` retries `EINTR` internally, so the `SIGTERM` handler sets the flag but the loop never regains control. `systemctl stop` therefore always ends in SIGKILL after `TimeoutStopSec=2` (`packaging/systemd/namtor-daemon.service:17`). This is a production bug, not only a CI one.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn accept_one_returns_none_on_poll_timeout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("t.sock");
    let listener = Listener::bind(&path, None, Box::new(NoopDispatch)).expect("bind");
    let start = std::time::Instant::now();
    // No client connects. The call must return within the poll timeout
    // instead of blocking forever, so the caller can observe SHUTDOWN.
    let got = listener.accept_one().expect("accept_one");
    assert!(got.is_none(), "expected timeout, got a connection");
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-daemon listener::tests::accept_one_returns_none_on_poll_timeout`
Expected: HANG (test times out) — `accept()` blocks indefinitely.

- [ ] **Step 3: Write minimal implementation**

Set the listener non-blocking at bind time and poll before accepting:

```rust
use std::os::fd::AsRawFd;

/// How long `accept_one` waits before returning `Ok(None)` so the caller can
/// re-check its shutdown flag.
const POLL_TIMEOUT_MS: i32 = 200;

/// Accept one connection.
///
/// Returns `Ok(None)` when the poll timed out or was interrupted by a signal;
/// the caller re-checks its shutdown flag and calls again. `accept()` on a
/// blocking listener retries `EINTR` internally and would never return, which
/// is why this polls explicitly.
pub fn accept_one(&self) -> Result<Option<UnixStream>, ListenerError> {
    let mut fds = [libc::pollfd {
        fd: self.listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }];
    // SAFETY: `fds` is a valid, correctly-sized array of pollfd for the
    // duration of the call, and the fd is owned by `self.listener`.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), 1, POLL_TIMEOUT_MS) };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            return Ok(None);
        }
        return Err(ListenerError::Io(err));
    }
    if rc == 0 {
        return Ok(None);
    }
    match self.listener.accept() {
        Ok((stream, _)) => Ok(Some(stream)),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(ListenerError::Io(e)),
    }
}
```

In `Listener::bind` and `Listener::from_fd`, call `listener.set_nonblocking(true)?`. Remove the `set_nonblocking(false)` currently at `listener.rs:113`.

Update the daemon main loop so `Ok(None)` is not treated as a connection:

```rust
while !SHUTDOWN.load(Ordering::Relaxed) {
    match listener.accept_one() {
        Ok(Some(stream)) => handle_connection(stream),
        Ok(None) => continue,
        Err(e) => tracing::warn!(error = %e, "accept failed"),
    }
}
```

Callers that previously treated `Ok(None)` as end-of-stream must be updated — check every `accept_one()` call site.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-daemon listener`
Expected: PASS.
Then verify the real behaviour: `./target/debug/namtor-daemon & sleep 2; time kill -TERM %1; wait`
Expected: exits in well under 2 s, status 0, no SIGKILL.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-daemon/src/listener.rs crates/namtor-daemon/src/bin/namtor-daemon.rs
git commit -m "fix(daemon): poll the listener so SIGTERM stops the daemon instead of SIGKILL"
```

---

### Task 1.4: Ship man pages and fix `systemd-analyze verify`

**Files:**
- Create: `docs/man/namtor-daemon.8`
- Create: `docs/man/namtor-cli.1`
- Modify: `packaging/install-manifest.toml`
- Modify: `packaging/arch/PKGBUILD`
- Modify: `.github/workflows/release.yml:264,274,285`
- Test: `packaging/tests/test_man_pages.sh` (create)

`packaging/systemd/namtor-daemon.service:4` and `namtor-daemon.socket:3` both declare `Documentation=man:namtor-daemon(8)`, but no man page exists anywhere in the repo. `systemd-analyze verify` shells out to `man`, fails, reports `Protocol error` and exits 1 — this is the command that actually fails the release step.

- [ ] **Step 1: Write the failing test**

WARNING: the draft below has two bugs — `while read` in a pipeline runs in a
subshell so `rc=1` is lost and the gate can never fail, and under `set -o
pipefail` a unit with no `man:` URI makes `grep` exit 1 and aborts the script.
Use a here-string plus `|| true`, and add a vacuity guard so an empty scan is
not mistaken for a pass.

```bash
#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Gate: every Documentation=man: URI in a shipped unit resolves to a man page
# source in the repo AND an entry in the install manifest.
set -euo pipefail
cd "$(dirname "$0")/../.."
rc=0
for unit in packaging/systemd/*.service packaging/systemd/*.socket; do
    [ -e "$unit" ] || continue
    while read -r ref; do
        page="${ref%(*}"; sec="${ref##*(}"; sec="${sec%)}"
        src="docs/man/${page}.${sec}"
        if [ ! -f "$src" ]; then
            echo "::error::$unit references man:$ref but $src does not exist"; rc=1
        fi
        if ! grep -q "$(basename "$src")" packaging/install-manifest.toml; then
            echo "::error::$src is not in packaging/install-manifest.toml"; rc=1
        fi
    done
done
exit $rc
```

- [ ] **Step 2: Run test to verify it fails**

Run: `bash packaging/tests/test_man_pages.sh`
Expected: FAIL — `docs/man/namtor-daemon.8` does not exist.

- [ ] **Step 3: Write minimal implementation**

Create `docs/man/namtor-daemon.8`:

```roff
.TH NAMTOR-DAEMON 8 "2026-09-16" "namtor 0.1.0" "System Manager's Manual"
.SH NAME
namtor\-daemon \- Nam-tor infrared face authentication daemon
.SH SYNOPSIS
.B namtor\-daemon
.SH DESCRIPTION
.B namtor\-daemon
owns the infrared camera and emitter, runs face detection, liveness
detection and template matching, and serves authentication requests on
.IR /run/namtor/auth.sock .
It is started by systemd and takes no arguments. Inference runs in a
separate sandboxed worker process.
.SH OPTIONS
.TP
.BR \-h ", " \-\-help
Print usage and exit.
.TP
.BR \-V ", " \-\-version
Print the version and exit.
.SH FILES
.TP
.I /run/namtor/auth.sock
PAM authentication socket, group
.IR namtor\-auth .
.TP
.I /var/lib/namtor/templates
Encrypted enrolled face templates.
.SH SEE ALSO
.BR namtor\-cli (1),
.BR pam_namtor (8)
```

Create `docs/man/namtor-cli.1` in the same style. Add both to `packaging/install-manifest.toml` installing to `/usr/share/man/man8/` and `/usr/share/man/man1/`, add the install lines to `packaging/arch/PKGBUILD` `package_namtor()`, and add `man-db` to the release toolchain step (`release.yml:105-108`). Also pass `--man=no` at `release.yml:264,274,285` so the gate does not depend on a man binary being present in the container.

- [ ] **Step 4: Run test to verify it passes**

Run: `bash packaging/tests/test_man_pages.sh && echo GATE-OK`
Expected: `GATE-OK`.
Then: `man --warnings -l docs/man/namtor-daemon.8 >/dev/null` — no warnings.

- [ ] **Step 5: Commit**

```bash
git add docs/man packaging/install-manifest.toml packaging/arch/PKGBUILD packaging/tests/test_man_pages.sh .github/workflows/release.yml
git commit -m "fix(packaging): ship man pages; systemd-analyze verify no longer fails"
```

---

### Task 1.5: Fix the remaining release-workflow defects

**Files:**
- Modify: `.github/workflows/release.yml` lines 133-138, 263, 273, 284, 302, 309, 328, 361-373
- Modify: `.github/workflows/ci.yml` lines 16, 63, 131, 151, 133, 152

- [ ] **Step 1: Write the failing test**

Create `packaging/tests/test_workflow_lint.sh`:

```bash
#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Gate: release workflow must not contain known-broken shell idioms.
set -euo pipefail
cd "$(dirname "$0")/../.."
rc=0
W=.github/workflows/release.yml

# D5: `grep -v 'not found'` can never fail; the check must be inverted.
if grep -n "ldd .* | grep -v 'not found'" "$W"; then
    echo "::error::inverted ldd check (grep -v can never fail)"; rc=1
fi
# D8: invalid workflow command.
if grep -n '::uninstall verified clean' "$W"; then
    echo "::error::invalid workflow command, expected ::notice::"; rc=1
fi
# D4: uninstall must remove all three split packages.
if grep -n 'pacman -Rns --noconfirm namtor$' "$W"; then
    echo "::error::uninstall removes only 'namtor'; split deps will abort"; rc=1
fi
exit $rc
```

- [ ] **Step 2: Run test to verify it fails**

Run: `bash packaging/tests/test_workflow_lint.sh`
Expected: FAIL with three errors.

- [ ] **Step 3: Write minimal implementation**

- `release.yml:263,273,284,302` — the `ldd` gate. NOTE: `! ldd X | grep 'not found'` is ALSO broken — `!` puts the pipeline in a tested context, which suppresses `set -e` for that statement, and GitHub runs `run:` blocks as `bash -e {0}`. Any step with a command after it swallows the failure. Verified empirically. Use the explicit form:

```bash
if ldd /usr/lib/security/pam_namtor.so | grep 'not found'; then
  echo "::error::pam_namtor.so has unresolved shared libraries"
  exit 1
fi
```
- `release.yml:309` — `timeout -k 10 120 pacman -Rns --noconfirm namtor-ui namtor` (package list per Phase 2; before Phase 2 lands use `namtor-gnome namtor-kde namtor`).
- `release.yml:328` — `echo "::notice::uninstall verified clean"`.
- `release.yml:133-138` — convert both plain-scalar `run:` values with trailing backslashes into `run: |` block scalars.
- `release.yml:361-373` — drive the evidence matrix from the real build matrix instead of hardcoding pass results for `fedora40` and `debian12`, which never ran. Minimum acceptable: reduce the hardcoded list to `[("arch","x86_64")]`.
- Bump `actions/checkout@v4` -> `@v5` at `release.yml:98,348,408` and `ci.yml:16,63,131,151`; `actions/setup-python@v5` -> `@v6` at `ci.yml:133,152`.

- [ ] **Step 4: Run test to verify it passes**

Run: `bash packaging/tests/test_workflow_lint.sh && echo GATE-OK`
Expected: `GATE-OK`.
Then: `actionlint .github/workflows/*.yml` (if available) — no errors.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows packaging/tests/test_workflow_lint.sh
git commit -m "fix(ci): correct ldd gate, split-package uninstall, workflow commands, action versions"
```

---

### Task 1.6: Wire the new gates into CI and cut a dry-run release

**Files:**
- Modify: `.github/workflows/ci.yml` (`packaging-gates` job)

- [ ] **Step 1: Add the two new gates to the packaging-gates job**

```yaml
      - name: Gate — man pages exist and are installed
        run: bash packaging/tests/test_man_pages.sh
      - name: Gate — release workflow shell idioms
        run: bash packaging/tests/test_workflow_lint.sh
```

- [ ] **Step 2: Verify the whole suite locally**

Run: `cargo test --workspace --exclude namtor-kde -- --skip concurrent_connections_serialise --test-threads=1`
Expected: >= 1515 passed, 0 failed.

- [ ] **Step 3: Commit and push**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: enforce man-page and workflow-lint gates"
git push
```

- [ ] **Step 4: Trigger a dry-run release and confirm green**

Run: `gh workflow run release.yml -f dry_run=true && sleep 30 && gh run list --workflow=release.yml --limit 1`
Then: `gh run watch <id>`
Expected: the `build` job reaches `upload-artifact` and the run concludes `success`. If it fails, capture with `gh run view <id> --log-failed` and iterate before moving to Phase 2.

---

# Phase 2 — One desktop-neutral UI

### Task 2.1: Delete the KDE KCM, preserving the lock-screen integration

**Files:**
- Delete: `crates/namtor-kde/` (entire directory, including the untracked `build-kde/` spew)
- Create: `crates/namtor-client/src/kscreenlocker.rs` (moved from `crates/namtor-kde/src/kscreenlocker.rs`)
- Modify: `Cargo.toml` (remove the `crates/namtor-kde` member)
- Modify: `.github/workflows/ci.yml` (remove `-p namtor-kde`, the CMake/KCM steps, the KDE ABI smoke test, and every `--exclude namtor-kde`)
- Modify: `.github/workflows/release.yml` (drop qt6/kirigami from the toolchain, drop the KDE package)
- Delete: `packaging/applications/org.namtor.kcm.desktop`, `packaging/metainfo/org.namtor.kcm.metainfo.xml`
- Modify: `packaging/arch/PKGBUILD`, `packaging/fedora/*.spec`, `packaging/debian/control`, `packaging/debian/rules`, `packaging/install-manifest.toml`
- Test: `crates/namtor-client/src/kscreenlocker.rs` (the existing tests move with it)

`kscreenlocker.rs` writes `[Authenticators] Face=true` into `~/.config/kscreenlockerrc` and refuses to leave `Face` as the only authenticator. That is a plain config-file writer with no Qt dependency; KDE Plasma users must not regress, so it moves into the shared client crate rather than dying with the KCM.

- [ ] **Step 1: Move the module and prove its tests still pass**

```bash
git mv crates/namtor-kde/src/kscreenlocker.rs crates/namtor-client/src/kscreenlocker.rs
```

Add `pub mod kscreenlocker;` to `crates/namtor-client/src/lib.rs`. Fix the doc comment's `crate::bridge::KcmBridge` reference, which no longer exists.

- [ ] **Step 2: Run its tests**

Run: `cargo test -p namtor-client kscreenlocker`
Expected: PASS — the same tests that passed inside `namtor-kde`.

- [ ] **Step 3: Delete the crate and every reference**

```bash
git rm -r crates/namtor-kde
rm -rf crates/namtor-kde   # removes untracked build-kde/ spew
```

Then remove `"crates/namtor-kde"` from `Cargo.toml` members and strip every `namtor-kde` / `libnamtor_kde` / `namtorqml` / Qt6 / KF6 / Kirigami / CMake reference from the workflows, packaging and docs listed above.

- [ ] **Step 4: Verify nothing references it**

Run: `grep -rn "namtor-kde\|namtor_kde\|namtorqml\|kcmshell\|Kirigami" --include="*.rs" --include="*.yml" --include="*.toml" --include="*.sh" --include="*.md" . | grep -v "^./target" | grep -v CHANGELOG`
Expected: no output.
Then: `cargo build --workspace --all-targets && cargo test --workspace -- --test-threads=1`
Expected: builds clean, all tests pass.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: drop the Plasma-specific KCM; keep kscreenlockerrc support in namtor-client"
```

---

### Task 2.2: Rename `namtor-gnome` to `namtor-ui`

**Files:**
- Rename: `crates/namtor-gnome/` -> `crates/namtor-ui/`
- Modify: `Cargo.toml`, `crates/namtor-ui/Cargo.toml`
- Modify: every `namtor_gnome::` / `namtor-gnome` reference across workflows, packaging, docs
- Modify: `crates/namtor-ui/src/bin/wizard.rs:20` — application id
- Modify: `packaging/applications/org.namtor.Wizard.desktop`

The app id in code is `org.namtor.Gnome` while the desktop entry is `org.namtor.Wizard` and sets `StartupWMClass=org.namtor.Wizard`. They do not match, so on both Wayland and X11 the window fails to associate with its desktop entry and shows a wrong icon and name in the taskbar. Fix by standardising on `org.namtor.Wizard`.

Polkit action ids (`org.namtor.*`) and D-Bus names (`org.namtor.Manager2/3`) are already desktop-neutral and MUST NOT change — they are ABI for installed systems.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-ui/tests/desktop_integration.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only
//! The GTK application id must match the installed desktop entry, or the
//! compositor cannot associate the window with its .desktop file and shows a
//! fallback icon and name.

const APP_ID: &str = namtor_ui::APP_ID;

#[test]
fn app_id_matches_desktop_entry_basename() {
    let entry = std::path::Path::new("../../packaging/applications")
        .join(format!("{APP_ID}.desktop"));
    assert!(entry.exists(), "no desktop entry for app id {APP_ID}: {entry:?}");
}

#[test]
fn app_id_matches_startup_wm_class() {
    let txt = std::fs::read_to_string("../../packaging/applications/org.namtor.Wizard.desktop")
        .expect("read desktop entry");
    assert!(
        txt.lines().any(|l| l.trim() == format!("StartupWMClass={APP_ID}")),
        "StartupWMClass does not match app id {APP_ID}"
    );
}

#[test]
fn desktop_entry_is_not_desktop_specific() {
    let txt = std::fs::read_to_string("../../packaging/applications/org.namtor.Wizard.desktop")
        .expect("read desktop entry");
    for line in txt.lines() {
        if let Some(cats) = line.strip_prefix("Categories=") {
            assert!(!cats.contains("GNOME"), "Categories pins the app to GNOME: {cats}");
        }
        assert!(!line.starts_with("X-GNOME-"), "GNOME-specific key: {line}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-ui --test desktop_integration`
Expected: FAIL — `namtor_ui::APP_ID` does not exist; `Categories` contains `GNOME`.

- [ ] **Step 3: Write minimal implementation**

```bash
git mv crates/namtor-gnome crates/namtor-ui
```

In `crates/namtor-ui/Cargo.toml`: `name = "namtor-ui"`, and
`description = "Nam-tor management UI — GTK4/libadwaita, desktop-neutral"`.

In `crates/namtor-ui/src/lib.rs`:

```rust
/// GTK application id. MUST equal the installed desktop entry basename and
/// its `StartupWMClass`, or the compositor shows a fallback icon and name.
pub const APP_ID: &str = "org.namtor.Wizard";
```

In `crates/namtor-ui/src/bin/wizard.rs`, replace the literal with `.application_id(namtor_ui::APP_ID)`.

In `packaging/applications/org.namtor.Wizard.desktop`, drop `GNOME` from `Categories` (leaving `Settings;Security;`) and delete the `X-GNOME-UsesNotifications` line.

Rewrite the module doc comments that say "GNOME UI" to say "Nam-tor UI" (`lib.rs`, `app.rs`, `shell.rs`, `presenter.rs`, `view_model.rs`, `privacy.rs`, `a11y.rs`, `polkit_auth.rs`, `components/*.rs`, `bin/wizard.rs`). Update `client.rs:6`, which claims "both GUI front-ends (GNOME and KDE)".

Update `Cargo.toml` members, both workflows, `packaging/install-manifest.toml`, the PKGBUILD/spec/control split-package definitions (`namtor-gnome` -> `namtor-ui`), and the docs.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-ui --test desktop_integration`
Expected: PASS, 3 tests.
Then: `grep -rn "namtor-gnome\|namtor_gnome" --include="*.rs" --include="*.toml" --include="*.yml" --include="*.sh" . | grep -v "^./target"`
Expected: no output.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(ui): rename namtor-gnome to namtor-ui; fix app-id/desktop-entry mismatch"
```

---

### Task 2.3: Make the window actually render

**Files:**
- Modify: `crates/namtor-ui/src/shell.rs:72-160` (`Shell::new`), `:338-343` (`Shell::present`)
- Test: `crates/namtor-ui/tests/shell_navigation.rs` (extend)

`Shell::new` builds an `AdwApplicationWindow` with **no content** and never presents it, then `present()` calls `dialog.present(Some(&self.window))`. An `AdwDialog` renders *inside* its parent window, so with an unmapped, empty 1x1 host nothing is displayed at all. Verified empirically: before the fix the process' only X11 toplevel was 1x1; adding `window.present()` produced a real 770x590 window.

The correct structure for a standalone settings app is to make the window itself the UI: an `AdwToolbarView` with an `AdwHeaderBar` and an `AdwViewStack` + `AdwViewSwitcher`, so the three destinations are real pages rather than a dialog floating in an empty window. This also removes the black void visible around the dialog in the captured screenshot.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn window_has_content_and_is_presented() {
    common::init_gtk();
    let app = libadwaita::Application::builder()
        .application_id("org.namtor.Wizard.test")
        .build();
    let shell = Shell::new(&app);
    assert!(
        shell.window().content().is_some(),
        "application window has no content; nothing would render"
    );
    shell.present();
    assert!(shell.window().is_visible(), "window was never presented");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `xvfb-run -a cargo test -p namtor-ui --test shell_navigation window_has_content`
Expected: FAIL — `content()` is `None`.

- [ ] **Step 3: Write minimal implementation**

Replace the dialog-in-empty-window construction with a real window:

```rust
let view_stack = adw::ViewStack::new();
view_stack.add_titled_with_icon(
    &profiles_page, Some("profiles"), &tr("Profiles"), "system-users-symbolic");
view_stack.add_titled_with_icon(
    &auth_page, Some("authentication"), &tr("Authentication"), "dialog-password-symbolic");
view_stack.add_titled_with_icon(
    &system_page, Some("system"), &tr("System"), "emblem-system-symbolic");

let switcher = adw::ViewSwitcher::builder()
    .stack(&view_stack)
    .policy(adw::ViewSwitcherPolicy::Wide)
    .build();

let header = adw::HeaderBar::builder().title_widget(&switcher).build();

let toolbar = adw::ToolbarView::builder().content(&view_stack).build();
toolbar.add_top_bar(&header);

let window = adw::ApplicationWindow::builder()
    .application(app)
    .title(tr("Nam-tor"))
    .default_width(800)
    .default_height(620)
    .width_request(360)
    .height_request(294)
    .build();
window.set_content(Some(&toolbar));
```

and:

```rust
/// Present the settings window.
pub fn present(&self) {
    self.window.present();
}
```

Note `authenticator-symbolic` (`shell.rs:52`) does not exist in the Adwaita icon theme — the captured screenshot shows a broken-icon placeholder for the Authentication tab. Use `dialog-password-symbolic`.

The `width_request`/`height_request` of 360x294 keep the window usable at phone width, which is what makes the switcher collapse to a bottom bar on narrow layouts.

Any code calling `shell.dialog()` (`app.rs` enrollment sub-page push/pop) must move to `view_stack` navigation or an `AdwNavigationView`; update `push_subpage`/`pop_subpage` call sites accordingly.

- [ ] **Step 4: Run test to verify it passes**

Run: `xvfb-run -a cargo test -p namtor-ui`
Expected: PASS, including the existing `shell_navigation` suite.
Then capture a screenshot and confirm a real window with three working tabs and no broken icon.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-ui/src/shell.rs crates/namtor-ui/tests/shell_navigation.rs
git commit -m "fix(ui): render the settings window; replace empty-host dialog with ToolbarView+ViewStack"
```

---

### Task 2.4: Follow the system colour scheme on every desktop

**Files:**
- Modify: `crates/namtor-ui/src/bin/wizard.rs`
- Test: `crates/namtor-ui/tests/desktop_integration.rs` (extend)

Running the app in a dark Plasma session emits:
`Adwaita-WARNING: Using GtkSettings:gtk-application-prefer-dark-theme with libadwaita is unsupported. Please use AdwStyleManager:color-scheme instead.`
KDE sets the GTK setting; libadwaita ignores it, so the app can render light inside a dark desktop. The portable fix is to follow `AdwStyleManager`, which reads the `org.freedesktop.appearance color-scheme` portal and therefore works on GNOME, Plasma, Sway and others.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn startup_does_not_use_gtk_prefer_dark_theme() {
    let src = std::fs::read_to_string("src/bin/wizard.rs").expect("read wizard.rs");
    assert!(
        !src.contains("gtk-application-prefer-dark-theme"),
        "use AdwStyleManager::color-scheme, not the deprecated GtkSettings property"
    );
    assert!(
        src.contains("StyleManager"),
        "wizard must set an explicit AdwStyleManager colour scheme"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-ui --test desktop_integration startup_does_not_use`
Expected: FAIL — no `StyleManager` reference.

- [ ] **Step 3: Write minimal implementation**

```rust
fn build_ui(app: &adw::Application) {
    // Follow the desktop's colour scheme via the freedesktop appearance
    // portal. KDE sets GtkSettings:gtk-application-prefer-dark-theme, which
    // libadwaita ignores; ForceDark/ForceLight would ignore the desktop.
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::Default);
    let controller = AppController::build(app);
    controller.borrow().present();
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-ui --test desktop_integration`
Expected: PASS.
Then run the app under both a light and a dark Plasma colour scheme and confirm it follows, with no Adwaita-WARNING in the log.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-ui
git commit -m "fix(ui): follow the desktop colour scheme via AdwStyleManager"
```

---

# Phase 3 — Real scan quality guidance

> The guidance wire contract already exists end-to-end (`CaptureObservation` -> `Guidance` -> D-Bus `(ssuuuuuys)` -> `namtor-client` -> `EnrollmentViewV3`). The defect is that nothing real feeds it. These tasks replace fabricated inputs with measurements; they do NOT change the frozen wire signature except where a task says so.

### Task 3.1: Fix the guidance wire serialization bug

**Files:**
- Modify: `crates/namtor-daemon/src/manager/dbus.rs:793` (`push_progress`)
- Test: `crates/namtor-daemon/src/manager/dbus.rs` (inline)

`push_progress` serializes guidance as `format!("{:?}", p.guidance).to_lowercase()`, producing `movecloser`, `betterlighting`, `holdstill`, `facefront`, `lookinggood`, `moveback`, `insufficientpose`. `Guidance::from_wire` (`namtor-client/src/types.rs:276-291`) expects snake_case, so 7 of 9 variants fail to parse and the UI falls back to displaying the raw token — the user literally sees the text `betterlighting`. The correct helper already exists at `session.rs:547-561` (`guidance_to_snake_case`) and is used on the review path; the progress path just doesn't call it.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn every_guidance_variant_round_trips_over_the_wire() {
    use crate::manager::observation::Guidance;
    let all = [
        Guidance::Centering, Guidance::MoveCloser, Guidance::MoveBack,
        Guidance::BetterLighting, Guidance::HoldStill, Guidance::FaceFront,
        Guidance::LookingGood, Guidance::Capturing, Guidance::InsufficientPose,
    ];
    for g in all {
        let wire = guidance_to_snake_case(g);
        assert!(
            namtor_client::types::Guidance::from_wire(&wire).is_some(),
            "client cannot parse wire form {wire:?} for {g:?}"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-daemon every_guidance_variant_round_trips`
Expected: FAIL for `MoveCloser` (`movecloser` unparseable) if the test is written against the current `format!("{:?}")` path.

- [ ] **Step 3: Write minimal implementation**

At `dbus.rs:793` replace `format!("{:?}", p.guidance).to_lowercase()` with `crate::manager::session::guidance_to_snake_case(p.guidance)`. Make `guidance_to_snake_case` `pub(crate)`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-daemon guidance`
Expected: PASS, all 9 variants.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-daemon/src/manager
git commit -m "fix(enroll): serialize guidance as snake_case so the UI shows text, not raw tokens"
```

---

### Task 3.2: Fix `diversity_percent` (USA-0b is not actually fixed)

**Files:**
- Modify: `crates/namtor-daemon/src/manager/session.rs:422` (make `diversity_percent` `pub`)
- Modify: `crates/namtor-daemon/src/manager/worker.rs:474`
- Modify: `todo.md` (the entry claiming this was fixed)
- Test: `crates/namtor-daemon/src/manager/worker.rs` (inline)

`todo.md:82-83` claims USA-0b was fixed by using `capture.diversity_percent()` in both places. It was not: `worker.rs:474` still reads `snapshot.coverage_percent` (reservoir fill %, `session.rs:350-354`) while `review_snapshot()` uses mean pairwise cosine distance (`session.rs:422-440`). `diversity_percent` is still a private `fn` with exactly one caller, which is structural proof the change was never made. The user-visible symptom stands: the capture screen climbs to 100% as the 100-slot reservoir fills, then review drops to a single-digit number under the same label.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn capture_and_review_report_the_same_diversity_metric() {
    let mut capture = test_capture_with_similar_frames(40);
    let snap = capture.snapshot();
    let progress = compute_progress(&snap, &capture);
    let review = capture.review_snapshot();
    assert_eq!(
        progress.diversity_percent, review.diversity_percent,
        "capture screen and review screen disagree under the same label"
    );
}
```

Build the fixture from near-identical embeddings so true pairwise diversity is low while reservoir fill is high — that is exactly the case that exposes the bug.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-daemon capture_and_review_report_the_same`
Expected: FAIL — e.g. `100 != 4`.

- [ ] **Step 3: Write minimal implementation**

Make `diversity_percent` public:

```rust
/// Mean pairwise `1 - cosine_similarity` across the reservoir, as a
/// percentage. This is the true pose-spread metric and MUST be the only
/// thing reported as "diversity" — reservoir fill is a different quantity.
pub fn diversity_percent(&self) -> u32 { /* unchanged body */ }
```

At `worker.rs:474`:

```rust
let diversity_percent = capture.diversity_percent().min(100);
```

Update `todo.md` to record that USA-0b was previously mis-marked as fixed and is now genuinely fixed, with the test name as evidence.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-daemon diversity`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-daemon/src/manager todo.md
git commit -m "fix(enroll): report true pairwise diversity on both capture and review (USA-0b)"
```

---

### Task 3.3: Count accepted/rejected/evicted frames honestly

**Files:**
- Modify: `crates/namtor-daemon/src/manager/session.rs:236-273` (`pump`), `:455-477` (`offer`), `:67-71` (`ProgressSnapshot`)
- Modify: `crates/namtor-daemon/src/manager/worker.rs:471-473`
- Test: `crates/namtor-daemon/src/manager/session.rs` (inline)

`worker.rs:473` computes `frames_rejected = frames_seen - reservoir_len()`, but `reservoir_len()` is capped at `DEFAULT_MAX_RESERVOIR = 100`. Once the reservoir fills, `frames_accepted` freezes at 100 and every subsequent frame is counted as rejected — including frames that successfully evicted a worse candidate. After a 2000-frame session the UI reports 1900 rejected, which is simply false.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn accepted_count_keeps_rising_after_the_reservoir_fills() {
    let mut c = test_capture(/* max_reservoir */ 4);
    for _ in 0..12 { c.pump().expect("pump"); }   // all frames admissible
    let s = c.snapshot();
    assert_eq!(s.frames_seen, 12);
    assert!(s.frames_accepted > 4, "accepted froze at the reservoir cap: {}", s.frames_accepted);
    assert_eq!(s.frames_seen, s.frames_accepted + s.frames_rejected);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-daemon accepted_count_keeps_rising`
Expected: FAIL — accepted is 4.

- [ ] **Step 3: Write minimal implementation**

Add real counters to `EnrollmentCapture` and expose them on `ProgressSnapshot`:

```rust
/// Frames admitted to the reservoir, including those that evicted a worse
/// candidate. Monotonic; NOT the reservoir length, which is capped.
accepted: u32,
/// Frames the capture path declined, by reason.
rejected_no_face: u32,
rejected_below_floor: u32,
rejected_torn: u32,
```

Increment `accepted` in `offer()` on both admit and evict paths; increment the matching `rejected_*` counter at each decline site, including the `Ok(None)` "no face detected" branch at `session.rs:263-266` which currently drops the frame silently. Report `frames_accepted = accepted`, `frames_rejected = rejected_no_face + rejected_below_floor + rejected_torn`, and assert `seen == accepted + rejected` in a debug assertion.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-daemon session`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-daemon/src/manager
git commit -m "fix(enroll): real accept/reject counters with per-reason breakdown"
```

---

### Task 3.4: Measure real per-frame quality in the daemon

**Files:**
- Create: `crates/namtor-recog/src/quality.rs`
- Modify: `crates/namtor-recog/src/lib.rs`
- Test: `crates/namtor-recog/src/quality.rs` (inline)

`worker.rs:475-482` fabricates `CaptureObservation` by copying one scalar into four fields and hardcoding `landmark_stability: 100`. `compute_quality_percent` therefore degenerates to `q` (weights sum to 1.0), so quality can only ever be 20, 50 or 85, and `derive_guidance` can only reach 3 of its 9 variants — `MoveCloser`, `MoveBack`, `HoldStill`, `FaceFront`, `InsufficientPose` and `Centering` are dead code in production.

The measurements needed already exist in the wrong crate: `variance_of_laplacian` (`namtor-cli/src/enroll.rs:652-674`) and the `EnrollmentQuality` field set (`namtor-cli/src/enroll_session.rs:47-63`). Pose does not exist anywhere and must be derived from the 5-point `Landmarks` (`namtor-recog/src/detect.rs:21-27`).

**Interfaces:**
- Produces: `namtor_recog::quality::FrameMetrics { detection_confidence: f32, blur: f32, illumination: f32, yaw_deg: f32, roll_deg: f32, face_fraction: f32, landmark_spread: f32 }`
- Produces: `namtor_recog::quality::measure(frame: &[u8], w: u32, h: u32, det: &Detection) -> FrameMetrics`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn blur_is_lower_for_a_blurred_image() {
    let sharp = checkerboard(64, 64);
    let blurred = box_blur(&sharp, 64, 64, 3);
    let d = centered_detection(64, 64);
    assert!(measure(&blurred, 64, 64, &d).blur < measure(&sharp, 64, 64, &d).blur);
}

#[test]
fn illumination_tracks_mean_intensity() {
    let dark = vec![20u8; 64 * 64];
    let bright = vec![200u8; 64 * 64];
    let d = centered_detection(64, 64);
    assert!(measure(&dark, 64, 64, &d).illumination < 0.2);
    assert!(measure(&bright, 64, 64, &d).illumination > 0.7);
}

#[test]
fn yaw_is_near_zero_for_symmetric_landmarks() {
    let d = symmetric_detection(64, 64);
    assert!(measure(&vec![128u8; 64 * 64], 64, 64, &d).yaw_deg.abs() < 5.0);
}

#[test]
fn yaw_is_signed_for_asymmetric_landmarks() {
    let d = turned_left_detection(64, 64);
    assert!(measure(&vec![128u8; 64 * 64], 64, 64, &d).yaw_deg < -10.0);
}

#[test]
fn face_fraction_grows_with_box_size() {
    let small = detection_with_box(10, 10, 8, 8);
    let large = detection_with_box(10, 10, 40, 40);
    let f = vec![128u8; 64 * 64];
    assert!(measure(&f, 64, 64, &small).face_fraction < measure(&f, 64, 64, &large).face_fraction);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-recog quality`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Write minimal implementation**

Implement `measure`:
- `blur`: variance of the Laplacian over the face box only, normalised by `/50.0` and clamped to `0.0..=1.0` (port the kernel from `namtor-cli/src/enroll.rs:652-674`; do not duplicate it — move it here and have the CLI call this).
- `illumination`: mean intensity inside the face box, mapped through the band from `enroll_session.rs:75-85` (target 130, acceptable 90..170), so both too-dark and blown-out score low.
- `yaw_deg`: from the 5-point landmarks, the signed horizontal offset of the nose from the eye midpoint, normalised by inter-ocular distance and converted to degrees.
- `roll_deg`: `atan2` of the inter-ocular vector.
- `face_fraction`: `(box_w * box_h) / (w * h)` — the distance proxy.
- `landmark_spread`: inter-ocular distance over box width; degenerate geometry indicates an unreliable detection.

Every function is pure over a greyscale buffer plus a `Detection`, so it is fully unit-testable with synthetic images and no camera.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-recog quality`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-recog/src/quality.rs crates/namtor-recog/src/lib.rs
git commit -m "feat(recog): real per-frame quality metrics (blur, illumination, yaw, roll, size)"
```

---

### Task 3.5: Feed the real metrics into `CaptureObservation`

**Files:**
- Modify: `crates/namtor-daemon/src/manager/session.rs:49-60` (`Candidate`), `:116-120` (`EmbedSource`)
- Modify: `crates/namtor-daemon/src/manager/worker.rs:461-483` (`compute_progress`)
- Test: `crates/namtor-daemon/src/manager/worker.rs` (inline)

**Interfaces:**
- Consumes: `namtor_recog::quality::FrameMetrics` from Task 3.4.
- Produces: `EmbedSource::embed(&mut self, frame: &Frame) -> Result<Option<(Vec<f32>, FrameMetrics)>, ApiError>` — the `u8` quality scalar is replaced by the full metric set.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn observation_fields_are_independent_not_one_scalar_copied() {
    // A sharp but badly-lit frame must score high on blur and low on
    // illumination — the fabricated version made them identical.
    let m = FrameMetrics { detection_confidence: 0.9, blur: 0.9, illumination: 0.1,
                           yaw_deg: 0.0, roll_deg: 0.0, face_fraction: 0.3,
                           landmark_spread: 0.4 };
    let obs = observation_from(&m, /* diversity */ 50);
    assert!(obs.blur_estimate > 80, "blur: {}", obs.blur_estimate);
    assert!(obs.illumination < 20, "illumination: {}", obs.illumination);
    assert_ne!(obs.blur_estimate, obs.illumination);
}

#[test]
fn dark_frame_yields_better_lighting_guidance() {
    let m = FrameMetrics { detection_confidence: 0.9, blur: 0.9, illumination: 0.05,
                           yaw_deg: 0.0, roll_deg: 0.0, face_fraction: 0.3,
                           landmark_spread: 0.4 };
    assert_eq!(derive_guidance(&observation_from(&m, 50)), Guidance::BetterLighting);
}

#[test]
fn small_face_yields_move_closer_guidance() {
    let m = FrameMetrics { detection_confidence: 0.9, blur: 0.9, illumination: 0.6,
                           yaw_deg: 0.0, roll_deg: 0.0, face_fraction: 0.02,
                           landmark_spread: 0.4 };
    assert_eq!(derive_guidance(&observation_from(&m, 50)), Guidance::MoveCloser);
}

#[test]
fn turned_head_yields_face_front_guidance() {
    let m = FrameMetrics { detection_confidence: 0.9, blur: 0.9, illumination: 0.6,
                           yaw_deg: 35.0, roll_deg: 0.0, face_fraction: 0.3,
                           landmark_spread: 0.4 };
    assert_eq!(derive_guidance(&observation_from(&m, 50)), Guidance::FaceFront);
}

#[test]
fn all_nine_guidance_variants_are_reachable() {
    let mut seen = std::collections::HashSet::new();
    for m in representative_metric_sweep() { seen.insert(derive_guidance(&observation_from(&m, 50))); }
    assert_eq!(seen.len(), 9, "unreachable variants: {:?}", seen);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-daemon observation`
Expected: FAIL — `observation_from` does not exist; guidance variants unreachable.

- [ ] **Step 3: Write minimal implementation**

Add `observation_from(&FrameMetrics, diversity: u32) -> CaptureObservation` mapping each metric to its own field:

```rust
fn observation_from(m: &FrameMetrics, diversity: u32) -> CaptureObservation {
    CaptureObservation {
        detection_suitability: pct(m.detection_confidence),
        landmark_stability:    pct(m.landmark_spread),
        blur_estimate:         pct(m.blur),
        pose_frontality:       pct(1.0 - (m.yaw_deg.abs() / 45.0).min(1.0)),
        illumination:          pct(m.illumination),
        embedding_spread:      diversity.min(100) as u8,
    }
}
```

Replace the fabrication block at `worker.rs:475-482` with a call to this, threading the per-frame `FrameMetrics` through `ProgressSnapshot`. Widen `EmbedSource::embed` to return `FrameMetrics`, and update `MockEmbedSource` (`session.rs:125-159`) so its synthetic metrics still exercise every branch.

`derive_guidance` thresholds (`observation.rs:178-210`) may need retuning now that the inputs are independent; adjust with the sweep test as the arbiter, and add `face_fraction` as the driver for `MoveCloser`/`MoveBack`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-daemon`
Expected: PASS, including `all_nine_guidance_variants_are_reachable`.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-daemon crates/namtor-recog
git commit -m "feat(enroll): derive guidance from real per-frame metrics; all 9 variants reachable"
```

---

### Task 3.6: Provide a production `EmbedSource` so GUI enrollment works at all

**Files:**
- Create: `crates/namtor-daemon/src/manager/embed.rs`
- Modify: `crates/namtor-daemon/src/manager/dbus.rs:439-475` (`start_enrollment`)
- Test: `crates/namtor-daemon/src/manager/embed.rs` (inline) + `crates/namtor-daemon/tests/enrollment_live.rs` (HW-gated)

`start_enrollment` passes `None, None` for the capture and embed sources, so `pump()` immediately returns `Err(ApiError::State("no capture source for session"))` and the session tears down. There is no production `impl EmbedSource` anywhere — only `MockEmbedSource` and a test-only `FailingEmbedSource`. **GUI enrollment cannot currently work at all.** This task is what makes the feature real.

**Interfaces:**
- Consumes: `namtor_recog::RecognitionPipeline`, `namtor_recog::quality::measure` (Task 3.4).
- Produces: `PipelineEmbedSource::new(pipeline: RecognitionPipeline) -> Self`, implementing `EmbedSource`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn pipeline_embed_source_returns_none_when_no_face_detected() {
    let mut src = PipelineEmbedSource::new(pipeline_that_detects_nothing());
    let frame = grey_frame(640, 360);
    assert!(src.embed(&frame).expect("embed").is_none());
}

#[test]
fn pipeline_embed_source_returns_embedding_and_metrics_on_detection() {
    let mut src = PipelineEmbedSource::new(pipeline_that_detects_centered_face());
    let frame = grey_frame(640, 360);
    let (embedding, metrics) = src.embed(&frame).expect("embed").expect("detection");
    assert_eq!(embedding.len(), 128);
    assert!(metrics.detection_confidence > 0.0);
}

#[test]
fn start_enrollment_wires_a_real_capture_and_embed_source() {
    let svc = manager3_with_camera_and_models();
    let session = svc.start_enrollment("finley").expect("start");
    // Previously this produced no progress at all because both sources were None.
    assert!(svc.poll_progress(&session).expect("poll").has_progress);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-daemon embed`
Expected: FAIL — module does not exist; `start_enrollment` passes `None, None`.

- [ ] **Step 3: Write minimal implementation**

Implement `PipelineEmbedSource` wrapping `RecognitionPipeline::process_frame` (`namtor-recog/src/pipeline.rs:41-62`): detect, align, embed, and call `quality::measure` with the returned `Detection`. Return `Ok(None)` when detection fails. Then wire it into `start_enrollment` alongside a real `CaptureSource` built from the selected camera, replacing both `None`s.

Frames and embeddings stay inside the daemon — only `GuidanceProgress` crosses D-Bus. The privacy invariant is unchanged.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-daemon`
Expected: PASS.
Then, on the reference hardware: `NAMTOR_HW_TEST=1 cargo test -p namtor-daemon --test enrollment_live -- --ignored`
Expected: a real session against `/dev/video2` produces rising `frames_accepted` and varying guidance.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-daemon/src/manager
git commit -m "feat(enroll): production EmbedSource; GUI enrollment actually captures"
```

---

### Task 3.7: Build the scan screen on the real signals

**Files:**
- Modify: `crates/namtor-ui/src/components/enrollment.rs:111-248` (`render_capture_phase`)
- Modify: `crates/namtor-ui/src/navigation.rs:343-386`
- Modify: `crates/namtor-ui/src/view_model.rs:388-415`
- Test: `crates/namtor-ui/tests/enrollment_guidance.rs` (create)

Two divergent enrollment UIs exist (`navigation.rs` and `components/enrollment.rs`); this task collapses them onto one rendering path. Current defects to fix:
- `enrollment.rs:116` decides "good position" with `view.guidance.to_lowercase().contains("looking good")` — string-matching an already-translated label, so it breaks in every non-English locale. Use `quality_bucket` and a typed `Guidance` instead.
- `navigation.rs:371-386` (`update_review_summary`) never renders `view.guidance` at all — the review screen silently drops the one piece of advice it is given.
- `navigation.rs:345` labels `frames_accepted` as "Frames:" while `frames_seen` and `frames_rejected` are on the wire but never shown.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn good_position_is_not_decided_by_matching_translated_text() {
    let src = std::fs::read_to_string("src/components/enrollment.rs").expect("read");
    assert!(
        !src.contains("contains(\"looking good\")"),
        "locale-dependent string match decides UI state"
    );
}

#[test]
fn review_screen_renders_guidance() {
    let view = ReviewViewV3 { ready_to_save: false, quality_percent: 41,
                              diversity_percent: 12, frames_accepted: 30,
                              guidance: "move_closer".into() };
    let rendered = render_review_text(&view);
    assert!(rendered.contains("Move closer"), "guidance dropped: {rendered}");
}

#[test]
fn capture_screen_shows_accepted_and_seen_distinctly() {
    let view = capture_view(/* seen */ 120, /* accepted */ 30, /* rejected */ 90);
    let rendered = render_capture_text(&view);
    assert!(rendered.contains("30"), "accepted missing");
    assert!(rendered.contains("120"), "seen missing");
}

#[test]
fn no_forbidden_privacy_substring_reaches_the_ui() {
    for s in all_rendered_strings() {
        assert!(namtor_ui::privacy::forbidden_words_in_ui_string(&s).is_empty(), "leak in {s:?}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `xvfb-run -a cargo test -p namtor-ui --test enrollment_guidance`
Expected: FAIL on the string-match guard and the dropped review guidance.

- [ ] **Step 3: Write minimal implementation**

Carry a typed `Guidance` (not a pre-translated `String`) on `EnrollmentViewV3`, translating only at render time. Replace the `is_good_position` string match with `view.quality_bucket >= 2`. Render, on the capture screen:
- a large face-outline status icon whose style follows `quality_bucket` (this is the silhouette, not a video preview);
- the guidance line as the primary, largest piece of text — it is the only actionable element;
- an `AdwProgressBar` for diversity, plus distinct rows for `Accepted`, `Seen` and `Rejected`;
- a per-signal breakdown so the user can tell *why* quality is low rather than seeing one opaque percentage.

Add the guidance row to `update_review_summary` in `navigation.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `xvfb-run -a cargo test -p namtor-ui`
Expected: PASS, including the existing `privacy_scan` suite (893 LOC) unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-ui
git commit -m "feat(ui): scan screen driven by real guidance; locale-safe, review shows advice"
```

---

# Phase 4 — Finish and review

### Task 4.1: Full verification sweep

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --workspace -- -D warnings`
- [ ] `./scripts/check-spdx.sh`
- [ ] `cargo test --workspace -- --test-threads=1` — expect > 1512 passing, 0 failing
- [ ] every `packaging/tests/*.sh` gate green
- [ ] `cargo audit --file Cargo.lock`
- [ ] launch the UI on Plasma/Wayland and on a GNOME session; capture screenshots of Profiles, Authentication, System, capture and review; confirm no broken icons and correct dark/light following
- [ ] `NAMTOR_HW_TEST=1 cargo test --workspace -- --ignored` on the reference camera

### Task 4.2: Documentation truth pass

- [ ] `README.md` — remove `namtor-gnome`/`namtor-kde`, describe the single `namtor-ui` package; correct the "400+ tests" figure to the real count
- [ ] `AGENTS.md` — drop the KDE KCM from "Current state"; record the single-UI decision
- [ ] `docs/evidence/status-audit.md` — reflect the newly closed defects; keep every WITHHELD gate honestly withheld
- [ ] `todo.md` — correct the USA-0b entry that claimed a fix that was never applied
- [ ] `bash packaging/tests/test_doc_audit.sh`

### Task 4.3: Independent review

- [ ] Dispatch `superpowers:requesting-code-review` over the full diff
- [ ] Run the `security-review` skill over the diff, with specific attention to: the privacy invariant still holding, the new listener poll loop, and the new `EmbedSource` frame handling
- [ ] Address findings, re-run Task 4.1

---

## Self-review notes

**Spec coverage:** P1 covers every P0/P1 defect from the release forensics (D1-D8) plus the P2 argv work. P2 covers the single-UI requirement and the two rendering defects found empirically. P3 covers the guidance requirement, including the three bugs the audit surfaced (wire serialization, USA-0b, reject counting) and the blocker that GUI enrollment has no data source at all.

**Known risk:** Task 3.5 changes the `EmbedSource` trait signature, which ripples into `MockEmbedSource`, `FailingEmbedSource` and their tests. Task 3.6 depends on it. Sequence 3.4 -> 3.5 -> 3.6 strictly.

**Deliberately NOT in scope:** live video preview (dropped by the user); cutting WITHHELD gates or security features (scorched earth was clarified to mean distro-specific UI code only). The D-Bus wire signature `(ssuuuuuys)` stays frozen; no task widens it.

---

# Addendum — mandatory mitigations for Phase 2

These came out of an exhaustive blast-radius sweep. Phase 2 is **not** a safe mechanical rename without them.

## Facts that make Phase 2 easier than expected

- **`namtor-kde` has zero reverse dependencies.** `Cargo.lock` shows its deps are only `futures-util, namtor-client, serde, serde_json, tracing, zbus`. Nothing imports it. Deletion is Cargo-clean.
- **`crates/namtor-kde/src/contract.rs` is a 4-line re-export** of `namtor_client::contract::*`. Nothing unique is lost.
- **Deleting the KCM unblocks the Debian release cell.** `release.yml:10-16` disables Debian because *"Debian 12 bookworm has no KDE Frameworks 6 packages in its repos at all"*. That blocker disappears with the KCM. Fedora's blocker is separate and remains.
- **`crates/namtor-kde/build-kde/` is 5.2 MB of gitignored CMake spew** (`.gitignore:23`), which is why `git status` looked clean. Two things read from it and must be fixed first: `packaging/tests/test_desktop_cert.sh:137-138` and `packaging/generate-sbom.py:113-115`.

## R1 (CRITICAL) — package renames break upgrades

`pacman`/`dnf`/`apt` will not migrate `namtor-gnome` -> `namtor-ui`. The old package lingers and **file-conflicts** with the new one: both own `/usr/bin/namtor-enroll-wizard`, `org.namtor.Wizard.desktop`, the metainfo and the icon. `packaging/arch/PKGBUILD:215-219` currently has `provides=()`, `conflicts=()`, `replaces=()` all **empty**.

Task 2.2 MUST add, and Task 2.1 MUST do the equivalent for the deleted `namtor-kde`:

```bash
# packaging/arch/PKGBUILD, package_namtor-ui()
provides=('namtor-gnome')
conflicts=('namtor-gnome')
replaces=('namtor-gnome' 'namtor-kde')
```

```spec
# packaging/fedora/namtor.spec, %package ui
Provides:  namtor-gnome = %{version}-%{release}
Obsoletes: namtor-gnome < %{version}-%{release}
Obsoletes: namtor-kde   < %{version}-%{release}
```

```
# packaging/debian/control, Package: namtor-ui
Provides: namtor-gnome
Replaces: namtor-gnome, namtor-kde
Breaks:   namtor-gnome, namtor-kde
```

Without the `namtor-kde` obsoletion, a Plasma user keeps a dangling package whose `Depends: namtor (= 0.1.0-1)` **blocks core upgrades**.

## R2 / R7 (CRITICAL) — the `Package` enum is serialized into an installed file

`packaging/install-manifest.toml` is installed to `/usr/share/namtor/install-manifest.toml` and read at runtime by `packaging/uninstall.sh` and `namtor-cli` diagnostics. `crates/namtor-layout/src/install_layout.rs:64-74` derives `#[serde(rename_all = "lowercase")]` on `pub enum Package` with **no `#[serde(other)]` fallback**, and `package` is a hard `Option<Package>` field.

So a new `namtor-cli` reading an old on-disk manifest (`package = "gnome"` / `"kde"`) hits a hard deserialization error. **This is the single most likely runtime break in the whole plan.** Required:

```rust
pub enum Package {
    Core,
    /// The single desktop UI package.
    ///
    /// `alias` keeps old on-disk manifests (installed before the rename)
    /// loadable; `uninstall.sh` and `namtor-cli` read the INSTALLED manifest,
    /// which may predate this build.
    #[serde(alias = "gnome")]
    Ui,
}
```

and a test asserting `package = "gnome"` still deserializes to `Package::Ui`. For the removed `Kde` variant, either keep `#[serde(alias = "kde")]` on a retained variant or add `#[serde(other)] Unknown` so old manifests degrade instead of failing.

## R5 — `test_doc_audit.sh` fails on the *first* doc edit

`packaging/tests/test_doc_audit.sh:11,124-127` hard-requires the literal strings `namtor-gnome` **and** `namtor-kde` in both `README.md` and `packaging/README.md`. It runs in CI (`ci.yml:187-188`). The doc edits and this gate's `PACKAGE_NAMES=("namtor-ui")` change **must land in the same commit** or CI breaks mid-phase.

## R6 — the mutation meta-gate degrades silently

`packaging/tests/test_ci_gates.sh` mutations M08 (line 237, `apply_replace $'    namtor-gnome\n' ''`) and M13 (line 242, `rename 'Package: namtor-gnome'`) patch **literal strings**. If the strings change and the mutation definitions don't, `apply_replace` silently no-ops, the gate under test passes, and the meta-gate reports "mutation NOT caught" — a confusing CI failure. Retarget both to `namtor-ui` in the same commit.

## R3 / R4 — mechanical traps

- `Cargo.toml` `members` is alphabetised; `namtor-ui` sorts between `namtor-store` and `namtor-uvc`, not at the old `namtor-gnome` position.
- `ci.yml:36-37`, `PKGBUILD:134-135`, `namtor.spec:175-176`, `debian/rules:64-65` each end `-p namtor-gnome` with a trailing ` \` and `-p namtor-kde` without. Deleting the last line leaves a dangling continuation and a shell syntax error.

## R8 — three certification suites are NOT in CI

`test_desktop_cert.sh`, `test_core_cert.sh`, `test_lifecycle_cert.sh` (also `test_release_ledger.sh`, `test_pam_install.sh`) are absent from `ci.yml` and from `test_ci_gates.sh` `BASELINE_SUITES`. They will not break the build, so they rot silently — but `test_desktop_cert.sh` alone carries 47 crate-name references and is ~40% KDE-only. Update them, and add them to CI in Task 1.6 so this cannot recur.

## Revision to Task 2.1 — where `kscreenlocker.rs` goes, and an honest finding

The blast-radius sweep found that `KcmBridge::set_face_unlock_enabled` and `check_lockout_safety` are **never called from the C++ shim or the QML**. Grepping `src/cpp/`, `qml/` and `abi.rs` for `faceUnlock|FaceUnlock|lockout|setFaceUnlock` returns nothing; the C ABI exports only `namtor_kde_abi_version`, `namtor_kde_manager_bus_name`, `namtor_kde_manager_object_path`.

**So deleting the KCM regresses no shipped Plasma behaviour — the lock-screen integration was never wired up.** Plasma users must hand-edit `kscreenlockerrc` today regardless. This is an unfinished feature, not a regression risk.

That changes the destination. Move it to **`crates/namtor-pam-config`** as `src/desktop_lockscreen.rs`, not to `namtor-client` and not to the UI crate:
- The operation is "provision a desktop's lock-screen authenticator", the same family as PAM-service provisioning, and that crate already owns `/etc/pam.d` mutation with `flock`, atomic writes and rollback.
- No GUI toolkit dependency, so both CLI and UI can call it.
- The `kde-face` PAM service is already first-class in the daemon (`manager/dbus.rs:1114-1141`); both halves belong on the same side of the boundary.
- Name it `desktop_lockscreen.rs` so it can later grow a GDM/greetd arm.

Port with it: an `io::Error` error variant (equivalent of `types.rs:54`), the **ordering guarantee** from `bridge.rs:113-120` (D-Bus PAM toggle first, file write only on success — pinned by `tests.rs:497-507`), and all 9 tests. Then either **wire `check_lockout_safety` into a real caller in the UI, or delete it** — do not ship another documented-but-unreachable guard.

Note: `manager/dbus.rs:1114-1141` `kde_face_enable_disable_cycle` tests a PAM service literally named `kde-face` under `/etc/pam.d`. That is Plasma-side PAM config, unrelated to the crate. **Keep it.**

## Evidence artifacts

`docs/evidence/**` (~40 files) are frozen release artifacts. Do **not** rewrite them. Wholly-KDE items (`docs/evidence/kde-qa/`, `docs/evidence/ui/kde/`, `docs/evidence/public-release/15-kde-*.json`) are indexed by path at `docs/evidence/release-index.md:199-207`. Leave them byte-identical and add a superseding note to the index.

`docs/adr/turnkey-architecture.md` *is* the three-package design. It needs a **superseding ADR**, not an edit.

`docs/superpowers/specs/2026-08-04-nam-tor-design.md` is the frozen spec (§9.3 is "KDE"). Add an amendment note; do not rewrite it.

---

# Review findings (coordinator, in-flight)

Recorded during review of landed work. Each needs a fix or an explicit decision before Phase 4 closes.

### F1 — `Listener::shutdown()` is a write-only no-op (pre-existing)

`crates/namtor-daemon/src/listener.rs:115-117` stores `true` into `self.shutdown`, and **nothing anywhere reads that flag**. Confirmed pre-existing: the same dead write exists at `8967671:crates/namtor-daemon/src/listener.rs:105-106`, before the poll fix. So it is not a regression from Task 1.3 — but it is a public API that silently does nothing, which fails "every feature finished".

Now that `accept_one` polls on a 200 ms timeout, the flag can be made real cheaply. Either:
- have `accept_one` check `self.shutdown` after the poll returns and yield `Ok(None)` / a distinct "stopped" outcome, giving in-process callers (tests, embedders) a real shutdown path; or
- delete `shutdown()` and the field outright.

Do not leave it as-is. Pick one and add a test that pins the chosen behaviour.

### F2 — `!zipman` ships uncompressed man pages (needs a decision)

`packaging/arch/PKGBUILD` sets `!zipman` so the installed paths match `packaging/install-manifest.toml` (`.8`, not `.8.gz`), because `packaging/uninstall.sh` removes exactly the manifest paths. This works, but Arch convention is gzipped man pages and `namcap` may warn (the namcap step is non-blocking). The alternative is to teach the manifest about `.gz`. Owner decision; either is defensible.

### F3 — `docs/man/namtor-daemon.8` references a man page that does not exist

Its `SEE ALSO` cites `pam_namtor(8)`, which was never written. It is only a cross-reference, not a `Documentation=` URI, so `systemd-analyze verify` is unaffected — but Debian's lintian flags dangling cross-references. Either write `docs/man/pam_namtor.8` or drop the reference.

### F4 — daemon cannot run as an unprivileged user (testability, not a defect)

`crates/namtor-store/src/crypto.rs:39` hardcodes `DEFAULT_ROOT_SECRET_PATH = "/var/lib/namtor/root_secret"` with no environment override, so `./target/debug/namtor-daemon` fails with EACCES for a normal user. There IS a programmatic override (`BackendSelector::with_root_secret_path`), so tests are fine; only running the real binary locally needs `unshare -Urm` plus a tmpfs. Worth a documented dev-mode override before HIL work, since the reference IR camera (`/dev/video2`) is present on this machine and HIL gates are otherwise reachable.

### F5 — CI `--skip concurrent_connections_serialise` removed; watch for flake

Verified locally: the test passes 25/25 (implementing agent) and 5/5 (coordinator), and the full suite is 1522 passed / 0 failed with no skip flag. The fix is structural, not timing luck — previously both threads blocked inside `accept()`; now the thread losing the accept race gets `EWOULDBLOCK` -> `Ok(None)` -> retries via `accept_one_blocking`, so both connections are served. Still, confirm across a few CI runs under runner load before treating it as settled.

### F6 — illumination band: the plan was self-contradictory; the fix is defensible but needs a tuning decision

Task 3.4's plan text said to map mean intensity through the existing band at `enroll_session.rs:75-85` (target 130, acceptable 90..170) AND asserted in its own test that `bright = 200` scores `> 0.7` while `dark = 20` scores `< 0.2`. The literal formula `1 - |mean-130|/130` gives 200 -> **0.46**, so the plan's own test could not pass against the formula the plan specified. My error.

The implementing agent's claim that "no symmetric function can satisfy both" is **overstated** — verified: it is true for a Gaussian (needs `sigma^2 > 6869` and `sigma^2 < 3759` simultaneously, impossible) and for the literal linear formula, but an arbitrary symmetric monotone falloff with a steeper tail (e.g. piecewise linear with f(70)=0.75, f(110)=0.15) satisfies both fine. So asymmetry was not forced by the constraints.

It is still the right call on the merits, and the rationale in the source is sound: an underexposed IR frame is unrecoverable noise, whereas an overexposed one still carries usable gradient. Implemented as flat 1.0 across 90..=170, linear to 0.0 at mean 20 (dark) and at mean 280 (bright): 20 -> 0.00, 130 -> 1.00, 200 -> 0.73, 255 -> 0.23.

**Open tuning decision for Task 3.5:** a uniformly white (fully saturated) face box scores **0.23**, not 0.0. For enrollment that is arguably too lenient — a blown-out frame carries no usable identity information. The honest fix is a saturated-pixel-fraction term rather than a harsher mean-intensity curve, and `namtor-capture` already declares exactly that field: `FrameQuality.saturated_frac` (`crates/namtor-capture/src/frame.rs:59-67`, currently never populated anywhere). Decide during Task 3.5 threshold tuning; if adopted, populate `saturated_frac` rather than inventing a parallel metric.

### F7 — `variance_of_laplacian` is misnamed (returns a standard deviation)

`crates/namtor-recog/src/quality.rs` ports the CLI kernel at `crates/namtor-cli/src/enroll.rs:652-674` verbatim, including `var.sqrt()` — so it returns the standard deviation, not the variance — and including division by the full buffer length rather than the interior-pixel count, which averages the zeroed border in. Ported as-is deliberately so the CLI can later delegate here with zero behaviour change. Rename it (and fix the denominator, if wanted) in the SAME task that makes `namtor-cli` delegate, never separately, or the two will silently diverge.

### F8 — geometry metrics survive an unreadable frame; gate on `landmark_spread`

Two ambiguities the caller must respect, both documented by the implementer:
- A truncated frame buffer zeroes `blur` and `illumination` but still returns valid `face_fraction`, `landmark_spread`, `yaw_deg`, `roll_deg` (those derive from `Detection` alone). So `illumination == 0.0` is ambiguous between "pitch black" and "unmeasurable".
- Coincident landmarks yield `yaw_deg == 0.0` and `roll_deg == 0.0`, which reads as "perfectly frontal". `landmark_spread == 0.0` is the only signal that they are meaningless.

**Task 3.5 must gate on `landmark_spread` before trusting yaw/roll**, otherwise a degenerate detection is scored as an ideal frontal pose — which would be a false "LookingGood" on a frame with no usable geometry.

Note also that yaw is projected onto the inter-ocular axis rather than the image x-axis (roll-invariant; reduces to the plan's literal wording at zero roll), and the sign convention is: negative = nose left of the eye midpoint in image coordinates.

### F9 — CI compiles `cargo-audit` from source on every run (~20 min wasted per run)

`.github/workflows/ci.yml` (Gate 7) runs:

```yaml
cargo install cargo-audit --locked 2>/dev/null || true
cargo audit --file Cargo.lock
```

`cargo install` builds cargo-audit from source every single run with no cache. Observed on run 35131384383: every other step in the `rust` job — SPDX, fmt, clippy, the full test suite, all three G5.1 gates and the PAM size gate — completed, and the job then sat in this one step for roughly 20 minutes. Historically the whole CI run took ~14 minutes, so this step now dominates it.

Worse, `2>/dev/null || true` swallows the install failure, so if the build breaks, `cargo audit` runs from whatever is (or is not) on PATH and the gate can silently degrade.

Fix, in order of preference:
1. Use the maintained action: `rustsec/audit-check@v2` with `token: ${{ secrets.GITHUB_TOKEN }}`.
2. Or install a prebuilt binary via `taiki-e/install-action@v2` with `tool: cargo-audit`.
3. Or, at minimum, cache `~/.cargo/bin` keyed on the cargo-audit version AND drop the `2>/dev/null || true` so an install failure fails the gate loudly instead of silently.

This is a pipeline-quality defect in its own right: "the pipelines working" should mean fast and honest, not 20 minutes of avoidable compilation guarding a check that can silently no-op.

### F10 — `todo.md` is gitignored, so the USA-0b correction is not tracked

`.gitignore:51` contains `/todo.md`. The corrected USA-0b entry — the one recording that the item was previously mis-marked FIXED while the bug was live, with the new test named as evidence — therefore exists only in the local working tree and is in no commit. If this checkout is lost, the correction is lost, and the next reader of a fresh clone sees nothing at all.

This matters more than a normal doc nit, because the failure mode it documents is precisely "a FIXED marker that nobody could verify". Leaving that lesson in an untracked file repeats the mistake.

Fix during the Phase 4 documentation pass (Task 4.2): move the substance into the tracked authoritative ledger, `docs/evidence/status-audit.md`, which already exists for exactly this purpose. Keep `todo.md` as a local scratchpad if the ignore rule is deliberate — but nothing load-bearing may live only there. Add a line to the ledger stating the rule: **a FIXED marker must cite a named test, or it does not count.**

Related: the USA-0b fix also changed `ReviewSnapshot.frames_accepted` from the capped `reservoir.len()` to the monotonic counter, so capture and review now report the same accepted count. That is the same "two numbers, one label" class of bug and should be recorded in the ledger alongside it.

### D6 verified fixed — and two new defects the published artifact exposes

The evidence artifact from the green release run (35131438215) now contains exactly one cell, `arch/x86_64`, matching the only matrix cell that ran. The old behaviour would have published `fedora40` and `debian12` as `pass` on all six stages without either ever executing. Confirmed fixed against a real published artifact, not just the source.

Two defects remain visible in that same artifact:

### F11 — `generated_at` holds a run ID, not a timestamp

```json
"tag": "production-readiness",
"generated_at": "35131438215",
```

`generated_at` is populated with `github.run_id`. A field named `generated_at`, in a `schema_version: 1` document intended as release evidence, must be a timestamp — anything consuming this for provenance or auditing will mis-parse it or silently treat a run ID as a date. Either emit a real ISO-8601 UTC timestamp and add a separate `run_id` field (preferred, since both are useful), or rename the field to `run_id`. Bump `schema_version` if the meaning of an existing field changes.

### F12 — `tag` holds a branch name on `workflow_dispatch`

The same artifact records `"tag": "production-readiness"`, which is a branch, because the workflow uses `github.ref_name` for both the evidence `tag` field and `softprops/action-gh-release`'s `tag_name` (`release.yml:482`). This was flagged as unverified in the original forensics; the artifact now confirms it live.

Harmless in a dry run, but a `workflow_dispatch` with `dry_run: false` would reach the publish job and attempt to cut a GitHub Release **named after a branch**. Guard it: require a `refs/tags/` ref for the publish job, or take the tag from an explicit workflow input and fail fast when it is absent or does not look like `v*`. Until that is fixed, `workflow_dispatch` with `dry_run: false` is a foot-gun that should not be used.

### F13 — `enrollment_live` looks for the models in the wrong place; they DO exist

The HW-gated test `crates/namtor-daemon/tests/enrollment_live.rs` skipped with "no real ONNX weights on this machine", having found only 17-26 byte fixtures under `/tmp/tmp.*`. That conclusion is wrong. The real weights are present, at the repo-relative path `AGENTS.md` documents (`var/lib/namtor/models/`, gitignored):

| file | size |
|---|---|
| `var/lib/namtor/models/yunet_2023mar.onnx` | 232,589 B |
| `var/lib/namtor/models/sface_2021dec.onnx` | 38,696,353 B |
| `var/lib/namtor/models/minifasnet_pad.onnx` | 191,665 B |

plus `yunet_2023mar/fp32.onnx` and `sface_2021dec/fp32.onnx` symlinks. All comfortably over the test's 64 KiB threshold. `/var/lib/namtor/models/` (system path) does NOT exist — only the repo-relative one does.

Fix: resolve the model directory the way the daemon does (`namtor-layout` `InstallRoot`/`LayoutPaths`, honouring the `NAMTOR_*` overrides) rather than probing a hardcoded or temp path, and make the skip message print the directory it actually searched. A skip whose stated reason is false is worse than a failure — it hides a runnable test.

### F14 — end-to-end HIL enrollment is blocked by the IR emitter, not by the models (VERIFIED)

Measured directly, 5 live frames at 640x360 GREY8 from `/dev/video2`:

```
frame 0: mean=0.1/255  min=0 max=4
frame 4: mean=0.1/255  min=0 max=4
```

With the emitter unarmed the IR sensor returns an essentially black image. YuNet will never detect a face, so `frames_accepted` would be 0 no matter how good the pipeline is. `v4l2-ctl --list-ctrls` exposes no emitter control; activation lives behind the UVC XU + quirks table in `src/bin/namtor-daemon/live_capture.rs`.

So the true blocker for a genuine end-to-end enrollment run is **owner-gated item O1** (root: install the systemd units, create the `namtor` user and `namtor-auth` group) so the daemon can arm the emitter — not missing models and not missing code. This is the single highest-value owner action for closing HIL gates: the reference camera, the TPM2 device and the models are all present on this machine.

What IS verified against real hardware: `live_ir_frames_produce_plausible_pixel_metrics` genuinely ran, pulling 30 live frames through `quality::measure` with all metrics in range. The measurement path is exercised against real sensor data; only the detect/embed/accept path needs the emitter.

### F15 — `Manager3Service::poll_progress` drops progress when it drains `Exhausted` (pre-existing)

Found while wiring the production embedder. `poll_progress` returns `has_progress: false` as soon as it drains a `WorkerEvent::Exhausted` in the same pass, discarding progress accumulated earlier in that pass. A fast-draining finite source therefore reports "no progress" regardless of how many frames it actually captured.

Not introduced by this work, and worked around in the new test with a paced replay source rather than papered over. It matters for the UI: a quick enrollment could show nothing at all. Fix before the scan screen ships, or the guidance work is undermined by the transport.

### Task 3.7 verified independently by the coordinator

Rendered the capture page myself in two guidance states via a throwaway harness (since deleted) and read the PNGs. Confirms the agent's description rather than taking it on trust:

- **Low quality** (`better_lighting`, q=34, bucket=0): amber person silhouette in a rounded card; **"Find better lighting"** as the dominant headline; actionable one-liner "Turn towards a window or lamp, and avoid a bright light behind you."; Image quality 34% (amber), Angle variety 18%, Views kept 16%; **Needs attention -> Lighting** in amber, subtitled "What the scan is waiting on".
- **Good** (`looking_good`, q=87, bucket=2): green silhouette; **"Looking good — keep going"**; "Keep turning your head slowly so the scan sees every angle."; 87% / 72% / 78%; **Needs attention -> Nothing** in green, and the row's subtitle changes to "Every signal is good enough right now".

The silhouette tint, the headline colour, the percentage tint and the attention row agree in both states, so the screen reads as one coherent story rather than four unrelated widgets. Each meter carries a plain-language subtitle saying what it measures ("How well the camera sees you", "Head angles collected so far", "Usable share of what the camera sent"), which is what turns three numbers into something a user can act on.

### F16 — meter bars stay accent-blue while their percentage is tinted

In both captured states the `gtk4::ProgressBar` fill renders **accent blue** while the adjacent percentage label is tinted by quality (amber at 34%, green at 87%). A blue bar next to an amber "34%" reads as two different signals about the same measurement, and blue carries no warning weight at all — it is the one element on the screen that does not join the story the rest of the row is telling.

Minor, purely cosmetic, and NOT a blocker. Fix by applying the same `success`/`warning`/`error` style class to the bar that already drives the label, so the whole row changes colour together. DESIGN.md §8 forbids custom CSS, and libadwaita provides these classes on `GtkProgressBar` directly, so no new CSS is needed.

### F17 — `AGENTS.md` is gitignored, like `todo.md` (same class as F10)

`.gitignore:50` ignores `AGENTS.md`. The rename task updated it in the working tree but could not commit it, exactly as happened with `todo.md` (F10). Two of the repo's three agent-facing orientation documents are therefore untracked, so a fresh clone gets neither, and any correction written into them evaporates.

`AGENTS.md` is 15 KB and is the file that tells the next agent the project state, the shipped APIs, the hard rules and the hardware facts. Its "Current state" section still describes the KDE KCM as shipped and lists `namtor-gnome` among the crates — both now false.

Decide during Task 4.2 whether these are deliberately local scratch files. If they are, the load-bearing content (project state, the corrected USA-0b entry, the "a FIXED marker must cite a named test" rule) must move into tracked docs — `docs/evidence/status-audit.md` and `docs/architecture.md` are the natural homes. If they are not deliberate, un-ignore them and commit. Either way, do not leave the authoritative orientation document both stale and untracked.

### Task 2.2 verified by the coordinator

`crates/namtor-ui` exists, `crates/namtor-gnome` is gone, `namtor_ui::APP_ID = "org.namtor.Wizard"` and the wizard builds from the constant rather than a literal. `Categories=Settings;Security;GTK;` — the `GNOME` category and `X-GNOME-UsesNotifications` are gone.

All 79 residual `namtor-gnome` matches are deliberate and were spot-checked: `Provides:`/`Replaces:`/`Breaks:` in `debian/control`, `Provides:`/`Obsoletes:` in the RPM spec, the Arch `provides`/`conflicts`/`replaces`, the `#[serde(alias = "gnome")]` machinery, `legacy_gnome` stranded-install detection, the gate assertions that all of the above exist, and changelog/comment history.

**Two errors in the coordinator's own brief, caught by the implementer:**
1. The brief asserted the GTK application id was "already `org.namtor.Wizard`, a previous task fixed the mismatch". It was not — `src/bin/wizard.rs` still carried the literal `"org.namtor.Gnome"`. The mismatch had been *identified* early and written into Task 2.2, never fixed. Do not describe a planned fix as a completed one.
2. The brief claimed `Categories=...GNOME;` and `X-GNOME-UsesNotifications` were "already handled or trivial". Neither was handled.

Also worth recording: `legacy_gnome` could not be implemented as a file probe the way `legacy_kde` was. `namtor-kde` owned a unique file (`org.namtor.kcm.desktop`), but `namtor-gnome` owned *exactly* the same four files `namtor-ui` owns, so a file probe would flag every healthy install as stranded. It is instead implemented against dpkg's `info/<pkg>.list` and pacman's `local/<pkg>-<ver>/desc`. RPM's binary database exposes no equivalent path, so that case relies on dnf's `Obsoletes:` and is documented in the field.

### F18 — five gate suites exist but no workflow runs them

Verified by diffing the gates `ci.yml` invokes against `packaging/tests/*.sh`. CI runs 14. These 5 exist on disk and are invoked by nothing:

- `test_core_cert.sh`
- `test_desktop_cert.sh`
- `test_lifecycle_cert.sh`
- `test_pam_install.sh`
- `test_release_ledger.sh`

**Correction to earlier coordinator statements:** I repeatedly described these as "pre-existing CI failures". They are not failing CI, because they are not in CI. Whatever they assert is enforced nowhere, and they have been free to rot — which is exactly what happened (`test_desktop_cert.sh` carries 13 assertions naming tests and symbols that no longer exist).

Decide per suite: wire it into `packaging-gates`, or delete it. A certification suite nobody runs is worse than no suite, because its presence implies coverage that does not exist. If wiring up, fix the stale assertions first — `test_core_cert.sh` also needs `namtor-models-0.1.0.tar.gz`, which is not in the tree.

### F19 — the meta-gate emitted STALE evidence while printing "Evidence written:" (FIXED)

The most serious finding of the review, because it is the failure mode this project's entire thesis is built against.

`packaging/tests/test_ci_gates.sh` is the meta-gate: it runs every other gate, injects a known defect per category, and verifies the right gate catches it. It then emits `docs/evidence/public-release/28-ci-baseline.json` and `28-ci-mutations.json`.

Mutation M07 targets `HANDOFF.md`, which **no longer exists** in the repo (it is gitignored and was untracked in `9d82035`). The target-missing branch writes the literal string `SKIP` into the results TSV instead of an exit code. The Python emitter then did `int(parts[2])`, raised `ValueError`, and died — but it is a heredoc, so the surrounding script carried on, printed `Evidence written:` naming both files, and **exited 0**.

Consequence: those two JSON files had not been regenerated by any recent run. Measured before the fix — both dated **2026-08-09**, five weeks stale, while the gate reported success and named them as its evidence.

Fixed in two places:
1. The emitter tolerates a `SKIP` row (`exit_code: null`, `result: "SKIP"`) instead of crashing.
2. The script now asserts both files were actually rewritten within the last 120 seconds before printing the banner, and fails with `::error::` otherwise. A dead emitter can never again leave stale JSON under a success message.

The underlying M07 target still needs a decision: restore `HANDOFF.md`, repoint the mutation at a file that exists, or drop M07. The gate is now honest about skipping it either way.

### F20 — the Fedora spec packages no man pages at all (pre-existing)

`packaging/fedora/namtor.spec`'s `%files` lists neither `namtor-daemon.8` nor `namtor-cli.1` (nor the new `pam_namtor.8`). With `_unpackaged_files_terminate_build` enabled, rpmbuild would fail; without it, the Fedora package silently ships no manual pages while Arch and Debian do. `test_fedora_split.sh` passes, so nothing detects this. Low severity, but it is a real per-distro inconsistency in a project whose selling point is one behaviour across distros.

### F21 — `Frame::emitter` reports parity, NOT whether the emitter is lit

Surfaced while fixing F13 and worth recording on its own, because it is a trap of exactly the kind F13 was about.

Every frame pulled from the reference camera is labelled `emitter=On` while the image is uniformly black (measured `mean=0.10/255`). `Frame::emitter` records **which side of the strobe alternation a frame came from**, not whether the emitter actually illuminated the scene. The two disagree whenever the emitter is unarmed.

Anything that gates on that flag to decide "the emitter is working" gets a confident false signal. The F13 probe therefore measures pixels (brightest frame mean over 5 frames against a 4.0/255 floor) instead of trusting the metadata, and says so in its source.

Audit any other consumer of `Frame::emitter` for the same assumption before the HIL gates run. The field is correct for its actual purpose — pairing On/Off frames for the differential — so this is a documentation and usage hazard, not a bug in the field itself.

### F22 — latent V4L2 `EBUSY` race in the HW test binary (FIXED)

`crates/namtor-daemon/tests/enrollment_live.rs` has two tests that both open `/dev/video2`, an exclusive V4L2 node, and cargo runs them on separate threads. The first real run after F13 failed with:

```
start the IR stream: Device("ioctl failed: VIDIOC_REQBUFS: EBUSY: Device or resource busy")
```

The race was **latent, and hidden by F13 itself** — the enrollment test always bailed out at the false model check before it ever reached the camera, so the two never overlapped. Fixing the false skip exposed it. Now serialised on a `static CAMERA: Mutex<()>` with the poison discarded, so a panicking sibling cannot fail the other for an unrelated reason.

A good illustration of why a false skip is worse than a failure: it had been concealing a genuine concurrency bug in the same file.

### F18 update — measured state of the five unrun suites

Each run directly, 2026-09-16:

| suite | result | in CI now? |
|---|---|---|
| `test_lifecycle_cert.sh` | **79 passed, 0 failed** | **YES — wired up** |
| `test_core_cert.sh` | 95 passed, 6 failed | no |
| `test_pam_install.sh` | 31 passed, 6 failed | no |
| `test_desktop_cert.sh` | 78 passed, 15 failed | no |
| `test_release_ledger.sh` | not run — rewrites timestamps into `docs/evidence/` as a side effect | no |

`test_lifecycle_cert.sh` passed clean and is now a CI gate, so its assertions are enforced instead of merely existing.

The remaining four still need an owner decision, and each fails for a different reason:

- **`test_core_cert.sh`** — all 6 failures are the absent `namtor-models-0.1.0.tar.gz`. That is a generated artifact (`packaging/build-model-archive.sh`), not tracked. Either build it in the job before running the gate, or make the gate skip with exit 77 when the archive is absent, matching how `test_model_archive.sh` already handles the hardware-gated case.
- **`test_pam_install.sh`** — 6 failures around `--restore-backup` and preflight. Needs someone to read whether the script or the assertions are wrong; this one guards PAM stack edits, so it is the most valuable of the four to get running.
- **`test_desktop_cert.sh`** — 15 failures: 13 assertions naming tests and symbols that no longer exist (the drift traces to commit `46e442c`, long before this programme) plus 2 `git check-ignore` quirks where `.gitignore` entries use trailing slashes. The assertions must be re-derived from the current code, not patched one by one.
- **`test_release_ledger.sh`** — has a write side effect on tracked evidence files, which is presumably why it was never wired up. It needs a read-only mode before it can be a gate.

A certification suite nobody runs implies coverage that does not exist. Either wire each one up or delete it; leaving them is the worst of the three options.
