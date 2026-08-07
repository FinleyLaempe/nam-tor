// SPDX-License-Identifier: GPL-3.0-only
//! Install-layout contract: derives the binary, model, store, and socket
//! paths the doctor probes from `packaging/install-manifest.toml` instead of
//! hard-coding reference-hardware constants.
//!
//! The doctor needs to report correctly across three deployment shapes:
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
//! entry (its `dest`), and the doctor follows automatically — no reference
//! path is hard-coded in Rust.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Identifier of the deployment shape the doctor is probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallRoot {
    /// Real installed system: paths are absolute and read from
    /// `/usr/share/namtor/install-manifest.toml`.
    Installed,
    /// DESTDIR staging root: paths are rebased under `root`.
    Staged,
    /// Source checkout: binaries come from `target/release/`, the manifest
    /// from `packaging/install-manifest.toml`.
    Source,
}

/// The concrete paths the doctor probes, derived from the install manifest
/// for a given [`InstallRoot`].
#[derive(Debug, Clone)]
pub struct LayoutPaths {
    pub daemon_bin: PathBuf,
    pub cli_bin: PathBuf,
    pub models_dir: PathBuf,
    pub templates_dir: PathBuf,
    pub key_path: PathBuf,
    pub daemon_socket: PathBuf,
    pub quirks_dirs: Vec<PathBuf>,
    pub manifest_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    files: Vec<ManifestFile>,
}

#[derive(Debug, Deserialize)]
struct ManifestFile {
    dest: String,
}

/// Logical path constants the doctor cannot derive from the manifest because
/// they are runtime/secret paths, not installed files. These are part of the
/// install-layout *contract* (declared in `packaging/tmpfiles.d/namtor.conf`
/// and `sysusers.d/namtor.conf`) and are rebased under the staging root for
/// DESTDIR runs so the doctor reports the path the operator should create.
const RUNTIME_SOCKET_REL: &str = "run/namtor/auth.sock";
const STORE_DIR_REL: &str = "var/lib/namtor/templates";
const STORE_KEY_REL: &str = "var/lib/namtor/templates.key";
const MODELS_DIR_REL: &str = "var/lib/namtor/models";

impl LayoutPaths {
    /// Resolve concrete paths for `root` (the staging root, or `/` for an
    /// installed system). `manifest_path` is the on-disk install manifest to
    /// parse; for source roots pass the repo's
    /// `packaging/install-manifest.toml`.
    pub fn resolve(root: &Path, manifest_path: &Path, kind: InstallRoot) -> Self {
        let (daemon_dest, cli_dest) = match Self::parse_dests(manifest_path) {
            Some(d) => d,
            None => {
                // Manifest unreadable: fall back to the manifest's declared
                // defaults so the doctor still reports *a* path (and an
                // actionable error) rather than panicking.
                (
                    PathBuf::from("/usr/lib/namtor/namtor-daemon"),
                    PathBuf::from("/usr/bin/namtor-cli"),
                )
            }
        };

        let (daemon_bin, cli_bin) = match kind {
            InstallRoot::Source => {
                // In a source tree the binaries live under target/release/.
                let daemon = PathBuf::from("target/release").join(
                    daemon_dest
                        .file_name()
                        .unwrap_or_else(|| "namtor-daemon".as_ref()),
                );
                let cli = PathBuf::from("target/release").join(
                    cli_dest
                        .file_name()
                        .unwrap_or_else(|| "namtor-cli".as_ref()),
                );
                (daemon, cli)
            }
            InstallRoot::Staged => {
                let daemon = rebase_abs(root, &daemon_dest);
                let cli = rebase_abs(root, &cli_dest);
                (daemon, cli)
            }
            InstallRoot::Installed => (daemon_dest.clone(), cli_dest.clone()),
        };

        let models_dir = Self::rebase(root, kind, MODELS_DIR_REL);
        let templates_dir = Self::rebase(root, kind, STORE_DIR_REL);
        let key_path = Self::rebase(root, kind, STORE_KEY_REL);
        let daemon_socket = Self::rebase(root, kind, RUNTIME_SOCKET_REL);

        // Quirks search order: installed/staged share dir first, then the
        // in-tree source directory (works for source-root runs and for
        // installed runs that want to fall back to a checked-out db).
        let installed_quirks = Self::rebase(root, kind, "usr/share/namtor/quirks/devices");
        let source_quirks = PathBuf::from("quirks/devices");
        let quirks_dirs = vec![installed_quirks, source_quirks];

        Self {
            daemon_bin,
            cli_bin,
            models_dir,
            templates_dir,
            key_path,
            daemon_socket,
            quirks_dirs,
            manifest_path: manifest_path.to_path_buf(),
        }
    }

    fn rebase(root: &Path, kind: InstallRoot, rel: &str) -> PathBuf {
        match kind {
            InstallRoot::Installed => PathBuf::from("/").join(rel),
            InstallRoot::Staged | InstallRoot::Source => root.join(rel),
        }
    }

    fn parse_dests(manifest_path: &Path) -> Option<(PathBuf, PathBuf)> {
        let text = std::fs::read_to_string(manifest_path).ok()?;
        let manifest: Manifest = toml::from_str(&text).ok()?;
        let daemon = manifest
            .files
            .iter()
            .find(|f| f.dest.ends_with("namtor-daemon"))?
            .dest
            .as_str();
        let cli = manifest
            .files
            .iter()
            .find(|f| f.dest.ends_with("/namtor-cli"))?
            .dest
            .as_str();
        Some((PathBuf::from(daemon), PathBuf::from(cli)))
    }
}

/// Reference constants that must never appear in the doctor's default
/// config. The static guard test asserts none of these strings are present
/// in [`LayoutPaths`] defaults derived from the manifest — they encode the
/// reference hardware, not a portable contract.
pub const REFERENCE_CONSTANTS: &[&str] =
    &["/dev/video2", "3277:0018", "/usr/libexec/namtor-daemon"];

/// Rebase an absolute `dest` path (e.g. `/usr/lib/namtor/...`) under `root`
/// by stripping the leading `/` so `PathBuf::join` doesn't discard `root`.
fn rebase_abs(root: &Path, dest: &Path) -> PathBuf {
    let stripped = dest.strip_prefix("/").unwrap_or(dest);
    root.join(stripped)
}
