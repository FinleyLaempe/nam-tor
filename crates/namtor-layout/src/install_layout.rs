// SPDX-License-Identifier: GPL-3.0-only
//! Install-layout derivation from `packaging/install-manifest.toml`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Identifier of the deployment shape being probed.
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

/// The concrete paths derived from the install manifest for a given
/// [`InstallRoot`]. Every binary path comes from the manifest's `[[files]]`
/// entries; runtime paths (socket, model store, template store, key, backoff)
/// come from the contract constants declared below (documented in
/// `packaging/tmpfiles.d/namtor.conf`).
#[derive(Debug, Clone)]
pub struct LayoutPaths {
    pub daemon_bin: PathBuf,
    pub cli_bin: PathBuf,
    pub worker_bin: PathBuf,
    pub enroll_wizard_bin: PathBuf,
    pub models_dir: PathBuf,
    pub templates_dir: PathBuf,
    pub key_path: PathBuf,
    pub backoff_path: PathBuf,
    pub daemon_socket: PathBuf,
    pub quirks_dirs: Vec<PathBuf>,
    pub manifest_path: PathBuf,
}

/// All file `dest` paths parsed from the manifest, for cross-check tests.
#[derive(Debug, Clone)]
pub struct ManifestDests {
    pub files: Vec<String>,
    pub dirs: Vec<String>,
}

impl ManifestDests {
    /// True if the manifest declares a file at `dest`.
    pub fn has_file(&self, dest: &str) -> bool {
        self.files.iter().any(|f| f == dest)
    }

    /// True if the manifest declares a directory at `path`.
    pub fn has_dir(&self, path: &str) -> bool {
        self.dirs.iter().any(|d| d == path)
    }
}

// ---------------------------------------------------------------------------
// Manifest deserialisation
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    files: Vec<ManifestFile>,
    #[serde(default)]
    dirs: Vec<ManifestDir>,
}

#[derive(Debug, Deserialize)]
struct ManifestFile {
    dest: String,
}

#[derive(Debug, Deserialize)]
struct ManifestDir {
    path: String,
}

// ---------------------------------------------------------------------------
// Runtime contract constants
//
// These paths are NOT installed files — they are runtime/secret paths
// declared in `packaging/tmpfiles.d/namtor.conf` and `sysusers.d/namtor.conf`.
// They are rebased under the staging root for DESTDIR runs so the doctor
// reports the path the operator should create.
// ---------------------------------------------------------------------------

const RUNTIME_SOCKET_REL: &str = "run/namtor/auth.sock";
const STORE_DIR_REL: &str = "var/lib/namtor/templates";
const STORE_KEY_REL: &str = "var/lib/namtor/templates.key";
const MODELS_DIR_REL: &str = "var/lib/namtor/models";
const BACKOFF_REL: &str = "var/lib/namtor/backoff.json";

/// The PAM module's default socket path. The PAM module (`namtor-pam`)
/// cannot depend on this crate (it must stay minimal — no TOML/serde deps,
/// < 500 KB binary). This constant is the single declaration of the socket
/// path for cross-check tests; the PAM module's `DEFAULT_SOCKET` must match.
pub const PAM_DEFAULT_SOCKET: &str = "/run/namtor/auth.sock";

impl LayoutPaths {
    /// Resolve concrete paths for `root` (the staging root, or `/` for an
    /// installed system). `manifest_path` is the on-disk install manifest to
    /// parse; for source roots pass the repo's
    /// `packaging/install-manifest.toml`.
    pub fn resolve(root: &Path, manifest_path: &Path, kind: InstallRoot) -> Self {
        let dests = Self::parse_dests(manifest_path);

        let (daemon_bin, cli_bin, worker_bin, enroll_wizard_bin) = match kind {
            InstallRoot::Source => {
                let daemon = source_binary(&dests.daemon, "namtor-daemon");
                let cli = source_binary(&dests.cli, "namtor-cli");
                let worker = source_binary(&dests.worker, "namtor-worker");
                let wizard = source_binary(&dests.enroll_wizard, "namtor-enroll-wizard");
                (daemon, cli, worker, wizard)
            }
            InstallRoot::Staged => {
                let daemon = rebase_abs(root, &dests.daemon);
                let cli = rebase_abs(root, &dests.cli);
                let worker = rebase_abs(root, &dests.worker);
                let wizard = rebase_abs(root, &dests.enroll_wizard);
                (daemon, cli, worker, wizard)
            }
            InstallRoot::Installed => (
                dests.daemon.clone(),
                dests.cli.clone(),
                dests.worker.clone(),
                dests.enroll_wizard.clone(),
            ),
        };

        let models_dir = Self::rebase(root, kind, MODELS_DIR_REL);
        let templates_dir = Self::rebase(root, kind, STORE_DIR_REL);
        let key_path = Self::rebase(root, kind, STORE_KEY_REL);
        let backoff_path = Self::rebase(root, kind, BACKOFF_REL);
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
            worker_bin,
            enroll_wizard_bin,
            models_dir,
            templates_dir,
            key_path,
            backoff_path,
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

    /// Parse the binary `dest` paths from the manifest. Falls back to the
    /// manifest's declared defaults if the file is unreadable so callers
    /// still report *a* path (and an actionable error) rather than panicking.
    fn parse_dests(manifest_path: &Path) -> DestPaths {
        match Self::parse_manifest(manifest_path) {
            Some(m) => DestPaths {
                daemon: find_dest(&m.files, "namtor-daemon")
                    .unwrap_or_else(|| PathBuf::from("/usr/lib/namtor/namtor-daemon")),
                cli: find_dest(&m.files, "/namtor-cli")
                    .unwrap_or_else(|| PathBuf::from("/usr/bin/namtor-cli")),
                worker: find_dest(&m.files, "namtor-worker")
                    .unwrap_or_else(|| PathBuf::from("/usr/lib/namtor/namtor-worker")),
                enroll_wizard: find_dest(&m.files, "namtor-enroll-wizard")
                    .unwrap_or_else(|| PathBuf::from("/usr/bin/namtor-enroll-wizard")),
            },
            None => DestPaths {
                daemon: PathBuf::from("/usr/lib/namtor/namtor-daemon"),
                cli: PathBuf::from("/usr/bin/namtor-cli"),
                worker: PathBuf::from("/usr/lib/namtor/namtor-worker"),
                enroll_wizard: PathBuf::from("/usr/bin/namtor-enroll-wizard"),
            },
        }
    }

    fn parse_manifest(manifest_path: &Path) -> Option<Manifest> {
        let text = std::fs::read_to_string(manifest_path).ok()?;
        toml::from_str(&text).ok()
    }

    /// Parse all file and directory dests from the manifest. Returns `None`
    /// if the manifest is unreadable.
    pub fn parse_all_dests(manifest_path: &Path) -> Option<ManifestDests> {
        let manifest = Self::parse_manifest(manifest_path)?;
        Some(ManifestDests {
            files: manifest.files.into_iter().map(|f| f.dest).collect(),
            dirs: manifest.dirs.into_iter().map(|d| d.path).collect(),
        })
    }
}

struct DestPaths {
    daemon: PathBuf,
    cli: PathBuf,
    worker: PathBuf,
    enroll_wizard: PathBuf,
}

/// Find the first file `dest` ending with `suffix`.
fn find_dest(files: &[ManifestFile], suffix: &str) -> Option<PathBuf> {
    files
        .iter()
        .find(|f| f.dest.ends_with(suffix))
        .map(|f| PathBuf::from(f.dest.as_str()))
}

/// In a source tree, binaries live under `target/release/`.
fn source_binary(dest: &Path, fallback_name: &str) -> PathBuf {
    PathBuf::from("target/release").join(dest.file_name().unwrap_or_else(|| fallback_name.as_ref()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn repo_manifest_path() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packaging/install-manifest.toml"
        ))
    }

    // -----------------------------------------------------------------------
    // Comprehensive: every path the layout exposes must match the manifest
    // or a declared runtime constant. This is the single test that proves
    // "all paths derive from the manifest or install_layout.rs."
    // -----------------------------------------------------------------------

    #[test]
    fn all_layout_paths_match_manifest_or_declared_constants() {
        let manifest = repo_manifest_path();
        let dests =
            LayoutPaths::parse_all_dests(&manifest).expect("repo install-manifest.toml must parse");

        // Binary paths must come from the manifest's [[files]] entries.
        let layout = LayoutPaths::resolve(Path::new("/"), &manifest, InstallRoot::Installed);

        assert_eq!(
            layout.daemon_bin,
            PathBuf::from("/usr/lib/namtor/namtor-daemon"),
        );
        assert!(dests.has_file("/usr/lib/namtor/namtor-daemon"));

        assert_eq!(layout.cli_bin, PathBuf::from("/usr/bin/namtor-cli"),);
        assert!(dests.has_file("/usr/bin/namtor-cli"));

        assert_eq!(
            layout.worker_bin,
            PathBuf::from("/usr/lib/namtor/namtor-worker"),
        );
        assert!(dests.has_file("/usr/lib/namtor/namtor-worker"));

        assert_eq!(
            layout.enroll_wizard_bin,
            PathBuf::from("/usr/bin/namtor-enroll-wizard"),
        );
        assert!(dests.has_file("/usr/bin/namtor-enroll-wizard"));

        // Runtime paths are declared constants (not installed files), but
        // the manifest must declare the parent directories.
        assert_eq!(layout.models_dir, PathBuf::from("/var/lib/namtor/models"));
        assert!(dests.has_dir("/var/lib/namtor/models"));

        assert_eq!(
            layout.templates_dir,
            PathBuf::from("/var/lib/namtor/templates"),
        );
        assert!(dests.has_dir("/var/lib/namtor/templates"));

        assert_eq!(layout.daemon_socket, PathBuf::from("/run/namtor/auth.sock"),);

        // Key and backoff are runtime paths without manifest dir entries
        // (they live under the templates parent / var/lib/namtor).
        assert_eq!(
            layout.key_path,
            PathBuf::from("/var/lib/namtor/templates.key"),
        );
        assert_eq!(
            layout.backoff_path,
            PathBuf::from("/var/lib/namtor/backoff.json"),
        );
    }

    #[test]
    fn manifest_declares_all_expected_installed_files() {
        let manifest = repo_manifest_path();
        let dests =
            LayoutPaths::parse_all_dests(&manifest).expect("repo install-manifest.toml must parse");

        // Binaries
        assert!(dests.has_file("/usr/lib/namtor/namtor-daemon"));
        assert!(dests.has_file("/usr/lib/namtor/namtor-worker"));
        assert!(dests.has_file("/usr/lib/namtor/namtor-worker-spawn"));
        assert!(dests.has_file("/usr/lib/namtor/namtor-camera-emitter-off"));
        assert!(dests.has_file("/usr/bin/namtor-cli"));
        assert!(dests.has_file("/usr/bin/namtor-enroll-wizard"));

        // PAM module
        assert!(dests.has_file("/usr/lib/security/pam_namtor.so"));

        // systemd
        assert!(dests.has_file("/usr/lib/systemd/system/namtor-daemon.service"));
        assert!(dests.has_file("/usr/lib/systemd/system/namtor-daemon.socket"));

        // sysusers / tmpfiles
        assert!(dests.has_file("/usr/lib/sysusers.d/namtor.conf"));
        assert!(dests.has_file("/usr/lib/tmpfiles.d/namtor.conf"));

        // D-Bus
        assert!(dests.has_file("/usr/share/dbus-1/system-services/org.namtor.Manager.service"));

        // Polkit
        assert!(dests.has_file("/usr/share/polkit-1/actions/org.namtor.policy"));

        // GNOME
        assert!(dests.has_file("/usr/share/applications/org.namtor.Wizard.desktop"));
        assert!(dests.has_file("/usr/share/metainfo/org.namtor.Wizard.metainfo.xml"));
        assert!(dests.has_file("/usr/share/icons/hicolor/scalable/apps/org.namtor.Wizard.svg"));

        // KDE
        assert!(dests.has_file("/usr/share/applications/org.namtor.kcm.desktop"));
        assert!(dests.has_file("/usr/share/metainfo/org.namtor.kcm.metainfo.xml"));
        assert!(dests.has_file("/usr/share/icons/hicolor/scalable/apps/org.namtor.kcm.svg"));

        // Manifests themselves
        assert!(dests.has_file("/usr/share/namtor/model-manifest.toml"));
        assert!(dests.has_file("/usr/share/namtor/install-manifest.toml"));
    }

    #[test]
    fn manifest_declares_all_expected_directories() {
        let manifest = repo_manifest_path();
        let dests =
            LayoutPaths::parse_all_dests(&manifest).expect("repo install-manifest.toml must parse");

        assert!(dests.has_dir("/usr/lib/namtor"));
        assert!(dests.has_dir("/usr/lib/security"));
        assert!(dests.has_dir("/usr/share/namtor"));
        assert!(dests.has_dir("/var/lib/namtor"));
        assert!(dests.has_dir("/var/lib/namtor/models"));
        assert!(dests.has_dir("/var/lib/namtor/templates"));
        assert!(dests.has_dir("/usr/share/dbus-1/system-services"));
        assert!(dests.has_dir("/usr/share/polkit-1/actions"));
        assert!(dests.has_dir("/usr/share/applications"));
        assert!(dests.has_dir("/usr/share/metainfo"));
        assert!(dests.has_dir("/usr/share/icons/hicolor/scalable/apps"));
    }

    #[test]
    fn pam_default_socket_matches_layout() {
        let manifest = repo_manifest_path();
        let layout = LayoutPaths::resolve(Path::new("/"), &manifest, InstallRoot::Installed);
        assert_eq!(
            layout.daemon_socket,
            PathBuf::from(PAM_DEFAULT_SOCKET),
            "PAM_DEFAULT_SOCKET must match the layout-derived socket path"
        );
    }

    // -----------------------------------------------------------------------
    // Deployment shape tests
    // -----------------------------------------------------------------------

    #[test]
    fn static_guard_rejects_reference_constants_in_defaults() {
        let layout = LayoutPaths::resolve(
            Path::new("/nonexistent-root"),
            Path::new("/nonexistent-manifest.toml"),
            InstallRoot::Installed,
        );
        let layout_text = format!(
            "{} {} {} {} {} {} {} {} {:?}",
            layout.daemon_bin.display(),
            layout.cli_bin.display(),
            layout.worker_bin.display(),
            layout.enroll_wizard_bin.display(),
            layout.models_dir.display(),
            layout.templates_dir.display(),
            layout.key_path.display(),
            layout.daemon_socket.display(),
            layout.quirks_dirs
        );
        for c in REFERENCE_CONSTANTS {
            assert!(
                !layout_text.contains(c),
                "reference constant {c:?} leaked into layout fallback defaults: {layout_text}"
            );
        }
    }

    #[test]
    fn layout_source_root_uses_target_release_binaries() {
        let layout = LayoutPaths::resolve(
            std::env::current_dir().unwrap().as_path(),
            &repo_manifest_path(),
            InstallRoot::Source,
        );
        assert!(layout.daemon_bin.ends_with("target/release/namtor-daemon"));
        assert!(layout.cli_bin.ends_with("target/release/namtor-cli"));
        assert!(layout.worker_bin.ends_with("target/release/namtor-worker"));
        assert!(layout
            .enroll_wizard_bin
            .ends_with("target/release/namtor-enroll-wizard"));
    }

    #[test]
    fn layout_staged_root_rebases_paths_under_destdir() {
        let tmp = TempDir::new().unwrap();
        let manifest = repo_manifest_path();
        let staged_manifest = tmp.path().join("usr/share/namtor/install-manifest.toml");
        fs::create_dir_all(staged_manifest.parent().unwrap()).unwrap();
        fs::copy(&manifest, &staged_manifest).unwrap();

        let layout = LayoutPaths::resolve(tmp.path(), &staged_manifest, InstallRoot::Staged);
        assert_eq!(
            layout.daemon_bin,
            tmp.path().join("usr/lib/namtor/namtor-daemon"),
        );
        assert_eq!(layout.cli_bin, tmp.path().join("usr/bin/namtor-cli"));
        assert_eq!(
            layout.worker_bin,
            tmp.path().join("usr/lib/namtor/namtor-worker"),
        );
        assert_eq!(layout.models_dir, tmp.path().join("var/lib/namtor/models"));
        assert_eq!(
            layout.daemon_socket,
            tmp.path().join("run/namtor/auth.sock")
        );
        assert_eq!(
            layout.templates_dir,
            tmp.path().join("var/lib/namtor/templates")
        );
        assert_eq!(
            layout.backoff_path,
            tmp.path().join("var/lib/namtor/backoff.json")
        );
    }

    #[test]
    fn layout_alternate_libexec_follows_manifest() {
        let tmp = TempDir::new().unwrap();
        let alt_manifest = tmp.path().join("install-manifest.toml");
        fs::write(
            &alt_manifest,
            "[[files]]\ndest = \"/usr/libexec/namtor-daemon\"\nsrc = \"x\"\nmode = \"0755\"\nowner = \"root\"\ngroup = \"root\"\n\n\
             [[files]]\ndest = \"/usr/bin/namtor-cli\"\nsrc = \"x\"\nmode = \"0755\"\nowner = \"root\"\ngroup = \"root\"\n\n\
             [[files]]\ndest = \"/usr/lib/namtor/namtor-worker\"\nsrc = \"x\"\nmode = \"0755\"\nowner = \"root\"\ngroup = \"root\"\n\n\
             [[files]]\ndest = \"/usr/bin/namtor-enroll-wizard\"\nsrc = \"x\"\nmode = \"0755\"\nowner = \"root\"\ngroup = \"root\"\n",
        )
        .unwrap();

        let layout = LayoutPaths::resolve(tmp.path(), &alt_manifest, InstallRoot::Installed);
        assert_eq!(
            layout.daemon_bin,
            PathBuf::from("/usr/libexec/namtor-daemon"),
            "alternate libexec dest followed from manifest"
        );
        assert_eq!(
            layout.worker_bin,
            PathBuf::from("/usr/lib/namtor/namtor-worker"),
        );
    }

    #[test]
    fn parse_all_dests_returns_none_for_unreadable_manifest() {
        let result = LayoutPaths::parse_all_dests(Path::new("/nonexistent"));
        assert!(result.is_none());
    }

    #[test]
    fn parse_all_dests_parses_files_and_dirs() {
        let tmp = TempDir::new().unwrap();
        let manifest = tmp.path().join("m.toml");
        fs::write(
            &manifest,
            "[[dirs]]\npath = \"/usr/lib/namtor\"\nmode = \"0755\"\nowner = \"root\"\ngroup = \"root\"\n\n\
             [[files]]\ndest = \"/usr/lib/namtor/namtor-daemon\"\nsrc = \"x\"\nmode = \"0755\"\nowner = \"root\"\ngroup = \"root\"\n",
        )
        .unwrap();

        let dests = LayoutPaths::parse_all_dests(&manifest).unwrap();
        assert!(dests.has_file("/usr/lib/namtor/namtor-daemon"));
        assert!(dests.has_dir("/usr/lib/namtor"));
        assert!(!dests.has_file("/nonexistent"));
    }
}
