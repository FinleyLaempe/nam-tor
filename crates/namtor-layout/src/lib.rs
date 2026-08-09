// SPDX-License-Identifier: GPL-3.0-only
//! Install-layout contract: derives the binary, model, store, socket, and
//! worker paths the daemon, CLI, and diagnostics need from
//! `packaging/install-manifest.toml` instead of hard-coding reference-
//! hardware constants.
//!
//! The manifest is the single source of truth for every *installed* file
//! path. Runtime-only paths (socket, model store, template store, key,
//! backoff) are not installed files — they are declared in
//! `packaging/tmpfiles.d/namtor.conf` and `sysusers.d/namtor.conf` — so they
//! live here as named constants that [`LayoutPaths`] rebases under the
//! staging root for DESTDIR runs.
//!
//! ## Deployment shapes
//!
//! - **Installed** — paths under the real root (`/usr/lib/namtor/...`,
//!   `/var/lib/namtor/...`, `/run/namtor/...`). The manifest is read from
//!   `/usr/share/namtor/install-manifest.toml`.
//! - **Staged (DESTDIR)** — paths under a staging root (e.g.
//!   `/mnt/staging/usr/lib/namtor/...`). The manifest is read from
//!   `<root>/usr/share/namtor/install-manifest.toml` and every path it
//!   declares is rebased under `<root>`.
//! - **Source** — a developer checkout with no install: binaries live under
//!   `target/release/`, quirks under `quirks/devices/`, and the runtime
//!   directories may not exist. The manifest is read from
//!   `packaging/install-manifest.toml` in the repo root and binary `dest`s
//!   are remapped to `target/release/<basename>`.
//!
//! [`LayoutPaths::resolve`] returns the concrete paths for a given root. The
//! daemon binary `dest` in the manifest is `/usr/lib/namtor/namtor-daemon`;
//! a distro that ships it under `/usr/libexec/` instead changes the manifest
//! entry (its `dest`), and all consumers follow automatically — no reference
//! path is hard-coded in Rust.

mod install_layout;

pub use install_layout::{
    GeneratedState, InstallRoot, LayoutPaths, ManifestDests, Package, Retention, SplitManifest,
    SplitManifestDir, SplitManifestFile, StatePolicy, ValidationError, PAM_DEFAULT_SOCKET,
    REFERENCE_CONSTANTS,
};
