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
    /// The single multi-call executable: `namtor daemon`, `namtor worker`,
    /// `namtor cli ...`, `namtor emitter-off <device>`. Four separate
    /// binaries were merged into this one file; the worker is still spawned
    /// as a separate process (the daemon re-execs it), and the PAM cdylib
    /// and the GTK wizard are deliberately still separate artifacts.
    pub namtor_bin: PathBuf,
    /// The `namtor-cli` compatibility symlink to [`Self::namtor_bin`].
    pub cli_symlink: PathBuf,
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
// Split-package ownership
// ---------------------------------------------------------------------------

/// Which split package owns an install manifest entry.
///
/// # On-disk compatibility
///
/// This enum is deserialized from `/usr/share/namtor/install-manifest.toml`,
/// which is **installed on the user's system** and read at runtime by
/// `packaging/uninstall.sh` and `namtor-cli` diagnostics. A newly built
/// `namtor-cli` routinely reads an *older* manifest than the one in this
/// source tree. Two things are therefore load-bearing:
///
/// - `#[serde(other)]` on [`Package::Unknown`]: without it, a manifest
///   written by a release that still had a `namtor-kde` package
///   (`package = "kde"`) would fail to deserialize at all, taking down
///   uninstall and diagnostics rather than degrading to "an entry this
///   build does not own".
/// - `#[serde(alias = "gnome")]` on [`Package::Ui`]: the UI package was
///   renamed from `namtor-gnome` to `namtor-ui`, so every manifest
///   installed before that rename says `package = "gnome"`. Without the
///   alias those entries would fall through to `Unknown` and
///   `uninstall.sh` would silently leave the wizard, its desktop entry,
///   its metainfo and its icon on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Package {
    /// `namtor` — daemon, worker, PAM, CLI, models, services, SBOM.
    Core,
    /// `namtor-ui` — GTK4/libadwaita wizard + settings.
    ///
    /// The alias is on-disk compatibility, not a synonym: see the
    /// `On-disk compatibility` note on this enum.
    #[serde(alias = "gnome")]
    Ui,
    /// A package name this build does not know — most likely `kde`, from a
    /// manifest installed before the Plasma KCM was dropped in favour of a
    /// single desktop-neutral UI. Entries owned by an unknown package are
    /// never staged by this build and never match a `--package` filter, but
    /// they must still parse.
    #[serde(other)]
    Unknown,
}

/// Lifecycle policy for generated runtime state on upgrade or purge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatePolicy {
    /// Keep the state across the operation.
    Preserve,
    /// Remove the state.
    Remove,
}

/// A runtime-generated state path (not an installed file) with its owning
/// package and lifecycle policy. Declared under `[[generated_state]]` in
/// the manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct GeneratedState {
    pub path: String,
    #[serde(default)]
    pub package: Option<Package>,
    pub on_upgrade: StatePolicy,
    pub on_purge: StatePolicy,
}

/// Which package owns the purge of biometric state. Declared under
/// `[retention]` in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Retention {
    pub purge_owner: Package,
}

/// A validation error found in a split-package manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// Two entries (file/dir, file/file, or dir/dir) claim the same path
    /// under different packages.
    Collision {
        path: String,
        packages: Vec<Package>,
    },
    /// An entry (file, dir, or generated_state) has no owning package.
    Orphan { path: String },
}

/// The full split-package manifest: files, dirs, generated state, retention,
/// all carrying package ownership.
#[derive(Debug, Clone)]
pub struct SplitManifest {
    pub schema_version: u32,
    pub files: Vec<SplitManifestFile>,
    pub dirs: Vec<SplitManifestDir>,
    pub generated_state: Vec<GeneratedState>,
    pub retention: Option<Retention>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SplitManifestFile {
    pub dest: String,
    #[serde(default)]
    pub package: Option<Package>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SplitManifestDir {
    pub path: String,
    #[serde(default)]
    pub package: Option<Package>,
}

#[derive(Debug, Deserialize)]
struct SplitManifestRaw {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    files: Vec<SplitManifestFile>,
    #[serde(default)]
    dirs: Vec<SplitManifestDir>,
    #[serde(default)]
    generated_state: Vec<GeneratedState>,
    #[serde(default)]
    retention: Option<Retention>,
}

impl SplitManifest {
    /// Validate the manifest for collisions and orphans. Returns a list of
    /// errors; empty means valid.
    pub fn validate(&self) -> Vec<ValidationError> {
        let mut errors: Vec<ValidationError> = Vec::new();

        // Collision: no two entries claim the same path.
        let mut seen: std::collections::HashMap<&str, Vec<Package>> =
            std::collections::HashMap::new();
        for d in &self.dirs {
            if let Some(pkg) = d.package {
                seen.entry(d.path.as_str()).or_default().push(pkg);
            }
        }
        for f in &self.files {
            if let Some(pkg) = f.package {
                seen.entry(f.dest.as_str()).or_default().push(pkg);
            }
        }
        for (path, pkgs) in &seen {
            let unique: Vec<Package> = pkgs.to_vec();
            if unique.len() > 1 {
                errors.push(ValidationError::Collision {
                    path: path.to_string(),
                    packages: unique,
                });
            }
        }

        // Duplicate file dests (same package, two entries) are also collisions.
        let mut file_dests: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
        for f in &self.files {
            *file_dests.entry(f.dest.as_str()).or_default() += 1;
        }
        for (dest, count) in &file_dests {
            if *count > 1 {
                let pkgs: Vec<Package> = self
                    .files
                    .iter()
                    .filter(|f| f.dest.as_str() == *dest)
                    .filter_map(|f| f.package)
                    .collect();
                if !errors
                    .iter()
                    .any(|e| matches!(e, ValidationError::Collision { path, .. } if path == dest))
                {
                    errors.push(ValidationError::Collision {
                        path: dest.to_string(),
                        packages: pkgs,
                    });
                }
            }
        }

        // Orphan: file/dir/generated_state without a package.
        for f in &self.files {
            if f.package.is_none() {
                errors.push(ValidationError::Orphan {
                    path: f.dest.clone(),
                });
            }
        }
        for d in &self.dirs {
            if d.package.is_none() {
                errors.push(ValidationError::Orphan {
                    path: d.path.clone(),
                });
            }
        }
        for gs in &self.generated_state {
            if gs.package.is_none() {
                errors.push(ValidationError::Orphan {
                    path: gs.path.clone(),
                });
            }
        }

        errors
    }

    /// Return the file and dir dests owned by `package`. Used by staging to
    /// filter which entries belong in a given split package.
    pub fn package_dests(&self, package: Package) -> ManifestDests {
        let files: Vec<String> = self
            .files
            .iter()
            .filter(|f| f.package == Some(package))
            .map(|f| f.dest.clone())
            .collect();
        let dirs: Vec<String> = self
            .dirs
            .iter()
            .filter(|d| d.package == Some(package))
            .map(|d| d.path.clone())
            .collect();
        ManifestDests { files, dirs }
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

        let (namtor_bin, cli_symlink, enroll_wizard_bin) = match kind {
            InstallRoot::Source => {
                // A source checkout has no symlink: `target/release/namtor` is
                // what `cargo build --release` produced, and the CLI is
                // reached through it as `namtor cli ...`.
                let namtor = source_binary(&dests.namtor, "namtor");
                let wizard = source_binary(&dests.enroll_wizard, "namtor-enroll-wizard");
                (namtor.clone(), namtor, wizard)
            }
            InstallRoot::Staged => (
                rebase_abs(root, &dests.namtor),
                rebase_abs(root, &dests.cli_symlink),
                rebase_abs(root, &dests.enroll_wizard),
            ),
            InstallRoot::Installed => (
                dests.namtor.clone(),
                dests.cli_symlink.clone(),
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
            namtor_bin,
            cli_symlink,
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
                namtor: find_dest(&m.files, "/namtor")
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_NAMTOR_BIN)),
                cli_symlink: find_dest(&m.files, "/namtor-cli")
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_CLI_SYMLINK)),
                enroll_wizard: find_dest(&m.files, "namtor-enroll-wizard")
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_ENROLL_WIZARD_BIN)),
            },
            None => DestPaths {
                namtor: PathBuf::from(DEFAULT_NAMTOR_BIN),
                cli_symlink: PathBuf::from(DEFAULT_CLI_SYMLINK),
                enroll_wizard: PathBuf::from(DEFAULT_ENROLL_WIZARD_BIN),
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

    /// Parse the full split-package manifest (schema v2) including package
    /// ownership, generated state, and retention. Returns `None` if the
    /// manifest is unreadable or unparseable.
    pub fn parse_split_manifest(manifest_path: &Path) -> Option<SplitManifest> {
        let text = std::fs::read_to_string(manifest_path).ok()?;
        let raw: SplitManifestRaw = toml::from_str(&text).ok()?;
        Some(SplitManifest {
            schema_version: raw.schema_version,
            files: raw.files,
            dirs: raw.dirs,
            generated_state: raw.generated_state,
            retention: raw.retention,
        })
    }
}

struct DestPaths {
    namtor: PathBuf,
    cli_symlink: PathBuf,
    enroll_wizard: PathBuf,
}

/// Fallbacks used when the installed manifest is unreadable, so callers still
/// report *a* path (and an actionable error) instead of panicking.
const DEFAULT_NAMTOR_BIN: &str = "/usr/bin/namtor";
const DEFAULT_CLI_SYMLINK: &str = "/usr/bin/namtor-cli";
const DEFAULT_ENROLL_WIZARD_BIN: &str = "/usr/bin/namtor-enroll-wizard";

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

        assert_eq!(layout.namtor_bin, PathBuf::from("/usr/bin/namtor"));
        assert!(dests.has_file("/usr/bin/namtor"));

        assert_eq!(layout.cli_symlink, PathBuf::from("/usr/bin/namtor-cli"));
        assert!(dests.has_file("/usr/bin/namtor-cli"));

        // The merged binary replaced four separate ones; nothing may be left
        // behind at the old paths, or uninstall.sh would strand them.
        for gone in [
            "/usr/lib/namtor/namtor-daemon",
            "/usr/lib/namtor/namtor-worker",
            "/usr/lib/namtor/namtor-camera-emitter-off",
        ] {
            assert!(!dests.has_file(gone), "{gone} still declared");
        }

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

        // Binaries. The daemon, worker, CLI and emitter-off helper are one
        // multi-call executable; `namtor-cli` survives as a compat symlink.
        assert!(dests.has_file("/usr/bin/namtor"));
        assert!(dests.has_file("/usr/bin/namtor-cli"));
        assert!(dests.has_file("/usr/bin/namtor-enroll-wizard"));
        for gone in [
            "/usr/lib/namtor/namtor-daemon",
            "/usr/lib/namtor/namtor-worker",
            "/usr/lib/namtor/namtor-camera-emitter-off",
        ] {
            assert!(!dests.has_file(gone), "{gone} still declared");
        }

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

        // Desktop UI
        assert!(dests.has_file("/usr/share/applications/org.namtor.Wizard.desktop"));
        assert!(dests.has_file("/usr/share/metainfo/org.namtor.Wizard.metainfo.xml"));
        assert!(dests.has_file("/usr/share/icons/hicolor/scalable/apps/org.namtor.Wizard.svg"));

        // The Plasma KCM's files were removed with its package and must not
        // come back — nothing builds or stages them any more.
        assert!(!dests.has_file("/usr/share/applications/org.namtor.kcm.desktop"));
        assert!(!dests.has_file("/usr/share/metainfo/org.namtor.kcm.metainfo.xml"));
        assert!(!dests.has_file("/usr/share/icons/hicolor/scalable/apps/org.namtor.kcm.svg"));

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
            "{} {} {} {} {} {} {} {:?}",
            layout.namtor_bin.display(),
            layout.cli_symlink.display(),
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
        assert!(layout.namtor_bin.ends_with("target/release/namtor"));
        // A source checkout has no symlink; the CLI is `target/release/namtor
        // cli ...`, so both point at the one built binary.
        assert!(layout.cli_symlink.ends_with("target/release/namtor"));
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
        assert_eq!(layout.namtor_bin, tmp.path().join("usr/bin/namtor"));
        assert_eq!(layout.cli_symlink, tmp.path().join("usr/bin/namtor-cli"));
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
            "[[files]]\ndest = \"/usr/libexec/namtor\"\nsrc = \"x\"\nmode = \"0755\"\nowner = \"root\"\ngroup = \"root\"\n\n\
             [[files]]\ndest = \"/usr/libexec/namtor-cli\"\nsrc = \"x\"\nmode = \"0755\"\nowner = \"root\"\ngroup = \"root\"\n\n\
             [[files]]\ndest = \"/usr/bin/namtor-enroll-wizard\"\nsrc = \"x\"\nmode = \"0755\"\nowner = \"root\"\ngroup = \"root\"\n",
        )
        .unwrap();

        let layout = LayoutPaths::resolve(tmp.path(), &alt_manifest, InstallRoot::Installed);
        assert_eq!(
            layout.namtor_bin,
            PathBuf::from("/usr/libexec/namtor"),
            "alternate libexec dest followed from manifest"
        );
        assert_eq!(
            layout.cli_symlink,
            PathBuf::from("/usr/libexec/namtor-cli"),
            "compat symlink follows the manifest too"
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

    // -----------------------------------------------------------------------
    // Split-package schema: package ownership, generated state, retention,
    // collision validation, orphan validation.
    // -----------------------------------------------------------------------

    fn split_manifest_toml() -> &'static str {
        "\
schema_version = 2

[retention]
purge_owner = \"core\"

[[generated_state]]
path = \"/var/lib/namtor/templates\"
package = \"core\"
on_upgrade = \"preserve\"
on_purge = \"remove\"

[[generated_state]]
path = \"/var/lib/namtor/models\"
package = \"core\"
on_upgrade = \"preserve\"
on_purge = \"remove\"

[[dirs]]
path = \"/usr/lib/namtor\"
mode = \"0755\"
owner = \"root\"
group = \"root\"
package = \"core\"

[[dirs]]
path = \"/usr/share/applications\"
mode = \"0755\"
owner = \"root\"
group = \"root\"
package = \"core\"

[[files]]
dest = \"/usr/lib/namtor/namtor-daemon\"
src = \"target/release/namtor-daemon\"
mode = \"0755\"
owner = \"root\"
group = \"root\"
package = \"core\"

[[files]]
dest = \"/usr/bin/namtor-enroll-wizard\"
src = \"target/release/namtor-enroll-wizard\"
mode = \"0755\"
owner = \"root\"
group = \"root\"
package = \"ui\"

[[files]]
dest = \"/usr/share/applications/org.namtor.kcm.desktop\"
src = \"packaging/applications/org.namtor.kcm.desktop\"
mode = \"0644\"
owner = \"root\"
group = \"root\"
package = \"kde\"
"
    }

    #[test]
    fn parse_split_manifest_parses_package_ownership() {
        let tmp = TempDir::new().unwrap();
        let manifest = tmp.path().join("split.toml");
        fs::write(&manifest, split_manifest_toml()).unwrap();

        let split =
            LayoutPaths::parse_split_manifest(&manifest).expect("split manifest must parse");
        assert_eq!(split.schema_version, 2);
        assert_eq!(
            split.retention,
            Some(Retention {
                purge_owner: Package::Core
            })
        );
        assert_eq!(split.generated_state.len(), 2);
        assert_eq!(split.generated_state[0].path, "/var/lib/namtor/templates");
        assert_eq!(split.generated_state[0].package, Some(Package::Core));
        assert_eq!(split.generated_state[0].on_upgrade, StatePolicy::Preserve);
        assert_eq!(split.generated_state[0].on_purge, StatePolicy::Remove);

        let daemon = split
            .files
            .iter()
            .find(|f| f.dest == "/usr/lib/namtor/namtor-daemon")
            .expect("daemon entry exists");
        assert_eq!(daemon.package, Some(Package::Core));

        let wizard = split
            .files
            .iter()
            .find(|f| f.dest == "/usr/bin/namtor-enroll-wizard")
            .expect("wizard entry exists");
        assert_eq!(wizard.package, Some(Package::Ui));

        // The fixture deliberately carries a `package = "kde"` entry, which
        // no longer names a package this build ships. It must degrade to
        // `Package::Unknown` rather than failing the whole parse.
        let kcm = split
            .files
            .iter()
            .find(|f| f.dest == "/usr/share/applications/org.namtor.kcm.desktop")
            .expect("kcm entry exists");
        assert_eq!(kcm.package, Some(Package::Unknown));
    }

    /// Regression guard for the installed-manifest compatibility contract:
    /// `/usr/share/namtor/install-manifest.toml` is written by whichever
    /// release is installed, and `uninstall.sh` + `namtor-cli` read it with
    /// whichever `namtor-layout` is installed. A manifest from a release
    /// that still shipped `namtor-kde` says `package = "kde"`; a build
    /// without that variant must still load it, or uninstall and
    /// diagnostics break on every machine upgrading from such a release.
    #[test]
    fn old_manifest_with_removed_kde_package_still_deserialises() {
        let tmp = TempDir::new().unwrap();
        let manifest = tmp.path().join("old-install-manifest.toml");
        fs::write(
            &manifest,
            "\
schema_version = 2

[[files]]
dest = \"/usr/share/applications/org.namtor.kcm.desktop\"
src = \"packaging/applications/org.namtor.kcm.desktop\"
mode = \"0644\"
owner = \"root\"
group = \"root\"
package = \"kde\"
",
        )
        .unwrap();

        let split = LayoutPaths::parse_split_manifest(&manifest)
            .expect("an old manifest naming a removed package must still parse");
        assert_eq!(split.files.len(), 1);
        assert_eq!(split.files[0].package, Some(Package::Unknown));

        // An unknown package is owned by nothing this build stages, so it
        // never leaks into a core or UI DESTDIR.
        assert!(split.package_dests(Package::Core).files.is_empty());
        assert!(split.package_dests(Package::Ui).files.is_empty());

        // And it is not a validation error — degrading, not failing, is the
        // whole point.
        assert!(
            split.validate().is_empty(),
            "an unknown package must not be a validation error: {:?}",
            split.validate()
        );
    }

    /// The other half of the installed-manifest compatibility contract: the
    /// UI package was renamed `namtor-gnome` -> `namtor-ui`, so every
    /// manifest installed before that rename says `package = "gnome"`.
    ///
    /// This must map to [`Package::Ui`], not fall through to
    /// [`Package::Unknown`] — an `Unknown` entry is staged by nothing and
    /// matches no `--package` filter, so `uninstall.sh` reading an old
    /// manifest would silently leave the wizard binary, its desktop entry,
    /// its metainfo and its icon behind.
    #[test]
    fn old_manifest_with_renamed_gnome_package_deserialises_as_ui() {
        let tmp = TempDir::new().unwrap();
        let manifest = tmp.path().join("old-install-manifest.toml");
        fs::write(
            &manifest,
            "\
schema_version = 2

[[files]]
dest = \"/usr/bin/namtor-enroll-wizard\"
src = \"target/release/namtor-enroll-wizard\"
mode = \"0755\"
owner = \"root\"
group = \"root\"
package = \"gnome\"
",
        )
        .unwrap();

        let split = LayoutPaths::parse_split_manifest(&manifest)
            .expect("a manifest predating the namtor-gnome -> namtor-ui rename must still parse");
        assert_eq!(split.files.len(), 1);
        assert_eq!(
            split.files[0].package,
            Some(Package::Ui),
            "package = \"gnome\" must alias to Package::Ui, not degrade to Unknown"
        );

        // And it is really owned: the entry stages into the UI DESTDIR.
        let ui_dests = split.package_dests(Package::Ui);
        assert!(ui_dests
            .files
            .iter()
            .any(|d| d == "/usr/bin/namtor-enroll-wizard"));
        assert!(split.package_dests(Package::Core).files.is_empty());
    }

    #[test]
    fn parse_split_manifest_returns_none_for_unreadable() {
        assert!(LayoutPaths::parse_split_manifest(Path::new("/nonexistent")).is_none());
    }

    #[test]
    fn validate_split_manifest_passes_for_valid_manifest() {
        let tmp = TempDir::new().unwrap();
        let manifest = tmp.path().join("valid.toml");
        fs::write(&manifest, split_manifest_toml()).unwrap();

        let split = LayoutPaths::parse_split_manifest(&manifest).expect("manifest must parse");
        let errors = split.validate();
        assert!(
            errors.is_empty(),
            "valid manifest should have no validation errors, got: {errors:?}"
        );
    }

    #[test]
    fn validate_split_manifest_detects_collision_between_file_and_dir() {
        let tmp = TempDir::new().unwrap();
        let manifest = tmp.path().join("collision.toml");
        fs::write(
            &manifest,
            "\
[[dirs]]
path = \"/usr/lib/namtor\"
mode = \"0755\"
owner = \"root\"
group = \"root\"
package = \"core\"

[[files]]
dest = \"/usr/lib/namtor\"
src = \"x\"
mode = \"0755\"
owner = \"root\"
group = \"root\"
package = \"ui\"
",
        )
        .unwrap();

        let split = LayoutPaths::parse_split_manifest(&manifest).expect("manifest must parse");
        let errors = split.validate();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Collision { .. })),
            "expected a collision error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_split_manifest_detects_duplicate_file_dest() {
        let tmp = TempDir::new().unwrap();
        let manifest = tmp.path().join("dup.toml");
        fs::write(
            &manifest,
            "\
[[files]]
dest = \"/usr/bin/namtor-cli\"
src = \"x\"
mode = \"0755\"
owner = \"root\"
group = \"root\"
package = \"core\"

[[files]]
dest = \"/usr/bin/namtor-cli\"
src = \"y\"
mode = \"0755\"
owner = \"root\"
group = \"root\"
package = \"ui\"
",
        )
        .unwrap();

        let split = LayoutPaths::parse_split_manifest(&manifest).expect("manifest must parse");
        let errors = split.validate();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Collision { .. })),
            "expected a collision error for duplicate dest, got: {errors:?}"
        );
    }

    #[test]
    fn validate_split_manifest_detects_orphan_entry_without_package() {
        let tmp = TempDir::new().unwrap();
        let manifest = tmp.path().join("orphan.toml");
        fs::write(
            &manifest,
            "\
[[files]]
dest = \"/usr/bin/namtor-cli\"
src = \"x\"
mode = \"0755\"
owner = \"root\"
group = \"root\"
",
        )
        .unwrap();

        let split = LayoutPaths::parse_split_manifest(&manifest).expect("manifest must parse");
        let errors = split.validate();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Orphan { .. })),
            "expected an orphan error for missing package, got: {errors:?}"
        );
    }

    #[test]
    fn validate_split_manifest_detects_orphan_generated_state_without_package() {
        let tmp = TempDir::new().unwrap();
        let manifest = tmp.path().join("orphan_gs.toml");
        fs::write(
            &manifest,
            "\
[[generated_state]]
path = \"/var/lib/namtor/templates\"
on_upgrade = \"preserve\"
on_purge = \"remove\"
",
        )
        .unwrap();

        let split = LayoutPaths::parse_split_manifest(&manifest).expect("manifest must parse");
        let errors = split.validate();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Orphan { .. })),
            "expected an orphan error for generated_state missing package, got: {errors:?}"
        );
    }

    #[test]
    fn package_dests_filters_by_package() {
        let tmp = TempDir::new().unwrap();
        let manifest = tmp.path().join("split.toml");
        fs::write(&manifest, split_manifest_toml()).unwrap();

        let split = LayoutPaths::parse_split_manifest(&manifest).expect("manifest must parse");

        let core_dests = split.package_dests(Package::Core);
        assert!(core_dests
            .files
            .contains(&"/usr/lib/namtor/namtor-daemon".to_string()));
        assert!(!core_dests
            .files
            .contains(&"/usr/bin/namtor-enroll-wizard".to_string()));
        assert!(!core_dests
            .files
            .contains(&"/usr/share/applications/org.namtor.kcm.desktop".to_string()));

        let ui_dests = split.package_dests(Package::Ui);
        assert!(ui_dests
            .files
            .contains(&"/usr/bin/namtor-enroll-wizard".to_string()));
        assert!(!ui_dests
            .files
            .contains(&"/usr/lib/namtor/namtor-daemon".to_string()));

        // The fixture's `package = "kde"` entry parses as `Unknown` and so
        // belongs to neither shipped package — it is filtered out of both.
        assert!(!ui_dests
            .files
            .contains(&"/usr/share/applications/org.namtor.kcm.desktop".to_string()));
        let unknown_dests = split.package_dests(Package::Unknown);
        assert!(unknown_dests
            .files
            .contains(&"/usr/share/applications/org.namtor.kcm.desktop".to_string()));
    }

    #[test]
    fn repo_manifest_validates_with_no_errors() {
        let manifest = repo_manifest_path();
        let split = LayoutPaths::parse_split_manifest(&manifest).expect("repo manifest must parse");
        let errors = split.validate();
        assert!(
            errors.is_empty(),
            "repo install-manifest.toml should validate with no errors, got: {errors:?}"
        );
    }

    #[test]
    fn repo_manifest_has_package_ownership_on_every_entry() {
        let manifest = repo_manifest_path();
        let split = LayoutPaths::parse_split_manifest(&manifest).expect("repo manifest must parse");

        for f in &split.files {
            assert!(
                f.package.is_some(),
                "file dest {} is missing package ownership",
                f.dest
            );
        }
        for d in &split.dirs {
            assert!(
                d.package.is_some(),
                "dir path {} is missing package ownership",
                d.path
            );
        }
        for gs in &split.generated_state {
            assert!(!gs.path.is_empty(), "generated_state entry missing path");
        }
    }

    #[test]
    fn repo_manifest_ui_package_has_no_kde_payload() {
        let manifest = repo_manifest_path();
        let split = LayoutPaths::parse_split_manifest(&manifest).expect("repo manifest must parse");

        let ui_dests = split.package_dests(Package::Ui);
        for dest in &ui_dests.files {
            // GNOME files must not contain Qt/KDE indicators in the path.
            assert!(
                !dest.contains("kcm")
                    && !dest.contains("kde")
                    && !dest.contains("qt6")
                    && !dest.contains("qml")
                    && !dest.contains("kpackage"),
                "GNOME package contains KDE/Qt payload: {dest}"
            );
        }
    }

    /// The repo manifest declares only packages this build ships. An
    /// `Unknown` entry here would mean a typo or a stale package name, not
    /// an old on-disk manifest — that fallback exists for installed files,
    /// not for the source of truth.
    #[test]
    fn repo_manifest_declares_no_unknown_package() {
        let manifest = repo_manifest_path();
        let split = LayoutPaths::parse_split_manifest(&manifest).expect("repo manifest must parse");

        for f in &split.files {
            assert_ne!(
                f.package,
                Some(Package::Unknown),
                "file dest {} names a package this build does not ship",
                f.dest
            );
        }
        for d in &split.dirs {
            assert_ne!(
                d.package,
                Some(Package::Unknown),
                "dir path {} names a package this build does not ship",
                d.path
            );
        }
    }
}
