// SPDX-License-Identifier: GPL-3.0-only
//! Package-aware diagnostics: detect split-package installation state,
//! migration artifacts from prior manual/monolithic installs, enabled PAM
//! service count, and TPM/software protection state — all without exposing
//! file paths, boot IDs, or biometric data.
//!
//! This module provides the helpers that [`crate::diagnostics`] calls into
//! for the package, migration, PAM, and protection check categories added
//! in Task 27. The output of every function here is privacy-safe by
//! construction: no paths, no hashes (full), no scores, no gate names.

use std::path::Path;

/// Which split packages are detected on the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPackages {
    pub core: bool,
    pub gnome: bool,
    /// True if the obsolete `namtor-kde` package (the Plasma 6 KCM, dropped
    /// when the project consolidated on one desktop-neutral GTK4 UI) is
    /// still installed.
    ///
    /// This is reported rather than ignored because it is actionable: the
    /// old package declared an exact-version dependency on the core
    /// package, so a leftover install BLOCKS core upgrades on pacman and
    /// dnf until it is removed. The current packages declare
    /// `replaces`/`Obsoletes`/`Breaks` on it so a normal upgrade clears it,
    /// but a machine that never takes that upgrade path stays stuck.
    pub legacy_kde: bool,
    /// True if binaries exist but the install manifest is missing — indicates
    /// a manual/monolithic install rather than a split-package install.
    pub manual: bool,
}

impl std::fmt::Display for InstalledPackages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if self.core {
            parts.push("namtor");
        }
        if self.gnome {
            parts.push("namtor-gnome");
        }
        if self.legacy_kde {
            parts.push("namtor-kde (obsolete)");
        }
        if self.manual {
            parts.push("manual");
        }
        if parts.is_empty() {
            write!(f, "none")
        } else {
            write!(f, "{}", parts.join(", "))
        }
    }
}

/// Detect which split packages are installed by probing for characteristic
/// files declared in the install manifest. `root` is an optional fake root
/// for testing (default `/`).
pub fn detect_installed_packages(root: &Path) -> InstalledPackages {
    let daemon = root.join("usr/lib/namtor/namtor-daemon");
    let manifest = root.join("usr/share/namtor/install-manifest.toml");
    let wizard = root.join("usr/bin/namtor-enroll-wizard");
    let gnome_desktop = root.join("usr/share/applications/org.namtor.Wizard.desktop");
    // The KCM desktop entry was the only file namtor-kde owned that had a
    // stable, unambiguous path, so it stays the probe for a leftover
    // install of that obsolete package.
    let legacy_kcm_desktop = root.join("usr/share/applications/org.namtor.kcm.desktop");

    let core = daemon.exists() && manifest.exists();
    let gnome = wizard.exists() && gnome_desktop.exists();
    let legacy_kde = legacy_kcm_desktop.exists();
    let manual = daemon.exists() && !manifest.exists();

    InstalledPackages {
        core,
        gnome,
        legacy_kde,
        manual,
    }
}

/// An old-style install artifact detected on the system, indicating a prior
/// manual or monolithic install that should be migrated to split packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationArtifact {
    /// Operator-stable identifier for the artifact type.
    pub kind: &'static str,
    /// Human-readable description (privacy-safe: no paths).
    pub detail: String,
    /// Recommended migration action (privacy-safe: no paths).
    pub remediation: String,
}

impl std::fmt::Display for MigrationArtifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} -- remediation: {}",
            self.kind, self.detail, self.remediation
        )
    }
}

/// Check for old manual/monolithic install artifacts that should be migrated
/// to split packages. `root` is an optional fake root for testing.
///
/// Artifacts checked:
/// - `namtor-worker-spawn` — old setuid helper removed in Task 18
/// - `install.sh` — old monolithic install script (if present alongside
///   split-package files, indicates a half-migrated system)
/// - Binaries without install manifest — manual install
pub fn detect_migration_artifacts(root: &Path) -> Vec<MigrationArtifact> {
    let mut artifacts = Vec::new();

    let worker_spawn = root.join("usr/lib/namtor/namtor-worker-spawn");
    if worker_spawn.exists() {
        artifacts.push(MigrationArtifact {
            kind: "legacy-setuid",
            detail: "old setuid worker spawn helper detected".into(),
            remediation: "remove it and reinstall via split packages".into(),
        });
    }

    let old_install = root.join("usr/share/namtor/install.sh");
    if old_install.exists() {
        artifacts.push(MigrationArtifact {
            kind: "legacy-install-script",
            detail: "old monolithic install script detected".into(),
            remediation: "remove it and reinstall via split packages".into(),
        });
    }

    let daemon = root.join("usr/lib/namtor/namtor-daemon");
    let manifest = root.join("usr/share/namtor/install-manifest.toml");
    if daemon.exists() && !manifest.exists() {
        artifacts.push(MigrationArtifact {
            kind: "manual-install",
            detail: "daemon binary present without install manifest".into(),
            remediation: "reinstall via split packages for proper lifecycle management".into(),
        });
    }

    artifacts
}

/// Count how many PAM service files in `pamd_dir` contain `pam_namtor.so`.
/// Returns 0 if the directory is unreadable. This is the enabled PAM service
/// count — each file with the module in its auth stack is one enabled service.
pub fn count_enabled_pam_services(pamd_dir: &Path) -> usize {
    let entries = match std::fs::read_dir(pamd_dir) {
        Ok(rd) => rd,
        Err(_) => return 0,
    };
    let mut count = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // Skip backups and dotfiles (same rejection rules as namtor-pam-config).
        if name.ends_with('~')
            || name.ends_with(".bak")
            || name.ends_with(".namtor.bak")
            || name.ends_with(".namtor.bak.sha256")
            || name.ends_with(".rpmsave")
            || name.ends_with(".rpmnew")
            || name.ends_with(".rpmorig")
            || name.ends_with(".dpkg-dist")
            || name.ends_with(".dpkg-old")
            || name.ends_with(".dpkg-new")
            || name.starts_with('.')
        {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            if text.contains("pam_namtor.so") {
                count += 1;
            }
        }
    }
    count
}

/// TPM/software protection state. Privacy-safe: no key material, no paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectionInfo {
    pub tpm_present: bool,
    pub sealing: &'static str,
}

impl std::fmt::Display for ProtectionInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tpm_str = if self.tpm_present {
            "present"
        } else {
            "absent"
        };
        write!(f, "tpm: {}, sealing: {}", tpm_str, self.sealing)
    }
}

/// Determine protection state: TPM device presence + sealing kind.
/// `key_path` is the sealing material file; if it exists and TPM is present,
/// sealing is "tpm2". If it exists without TPM, sealing is "software". If it
/// doesn't exist, sealing is "uninitialized".
pub fn detect_protection_state(key_path: &Path) -> ProtectionInfo {
    let tpm_present = namtor_store::tpm2_device_present();
    let sealing = if !key_path.exists() {
        "uninitialized"
    } else if tpm_present {
        "tpm2"
    } else {
        "software"
    };
    ProtectionInfo {
        tpm_present,
        sealing,
    }
}

/// Redact a long hex string to its first 8 characters + "...". Short strings
/// (fewer than 16 hex chars) are returned unchanged. This is used for model
/// hashes, SHA-256 digests, and any other long hex identifiers so the
/// support bundle never exposes full hashes.
pub fn redact_hex_id(hex: &str) -> String {
    let hex_only: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex_only.len() < 16 {
        return hex.to_string();
    }
    format!("{}...", &hex_only[..8])
}

/// SHA-256 digest formatted as colon-separated byte pairs, then redacted to
/// the first 4 bytes (8 hex chars) + "...". This avoids both the
/// contiguous-hex check in the privacy scanner and full-hash exposure.
pub fn redact_sha256_colon(digest: &[u8]) -> String {
    if digest.len() < 4 {
        return digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":");
    }
    let first4: String = digest[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":");
    format!("{first4}...")
}

/// Model state summary: which required models are present, missing, or
/// invalid. Privacy-safe: model names only, no paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelState {
    pub yunet: ModelStatus,
    pub sface: ModelStatus,
    pub minifasnet: ModelStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelStatus {
    Present,
    Missing,
}

impl ModelState {
    pub fn all_present(&self) -> bool {
        matches!(self.yunet, ModelStatus::Present)
            && matches!(self.sface, ModelStatus::Present)
            && matches!(self.minifasnet, ModelStatus::Present)
    }

    pub fn missing_names(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if matches!(self.yunet, ModelStatus::Missing) {
            missing.push("yunet");
        }
        if matches!(self.sface, ModelStatus::Missing) {
            missing.push("sface");
        }
        if matches!(self.minifasnet, ModelStatus::Missing) {
            missing.push("minifasnet");
        }
        missing
    }
}

impl std::fmt::Display for ModelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.all_present() {
            write!(f, "all present")
        } else {
            write!(f, "missing: {}", self.missing_names().join(", "))
        }
    }
}

/// Check model presence in `models_dir`. Each model is expected as a flat
/// `.onnx` file (matching the deployed layout from the model manifest).
pub fn detect_model_state(models_dir: &Path) -> ModelState {
    let check = |name: &str| {
        let path = models_dir.join(format!("{name}.onnx"));
        if path.exists() {
            ModelStatus::Present
        } else {
            ModelStatus::Missing
        }
    };
    ModelState {
        yunet: check("yunet_2023mar"),
        sface: check("sface_2021dec"),
        minifasnet: check("minifasnet_pad"),
    }
}

/// Service state: daemon socket active or inactive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Active,
    Inactive,
}

impl std::fmt::Display for ServiceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceState::Active => f.write_str("active"),
            ServiceState::Inactive => f.write_str("inactive"),
        }
    }
}

/// Check if the daemon socket exists (active) or not (inactive).
pub fn detect_service_state(socket_path: &Path) -> ServiceState {
    if socket_path.exists() {
        ServiceState::Active
    } else {
        ServiceState::Inactive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn touch(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    #[test]
    fn detect_both_split_packages_plus_obsolete_kde() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "usr/lib/namtor/namtor-daemon");
        touch(tmp.path(), "usr/share/namtor/install-manifest.toml");
        touch(tmp.path(), "usr/bin/namtor-enroll-wizard");
        touch(
            tmp.path(),
            "usr/share/applications/org.namtor.Wizard.desktop",
        );
        touch(tmp.path(), "usr/share/applications/org.namtor.kcm.desktop");
        let pkgs = detect_installed_packages(tmp.path());
        assert!(pkgs.core);
        assert!(pkgs.gnome);
        assert!(
            pkgs.legacy_kde,
            "a leftover namtor-kde install must be reported — it blocks core upgrades"
        );
        assert!(!pkgs.manual);
        assert_eq!(
            format!("{pkgs}"),
            "namtor, namtor-gnome, namtor-kde (obsolete)"
        );
    }

    #[test]
    fn detect_core_only() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "usr/lib/namtor/namtor-daemon");
        touch(tmp.path(), "usr/share/namtor/install-manifest.toml");
        let pkgs = detect_installed_packages(tmp.path());
        assert!(pkgs.core);
        assert!(!pkgs.gnome);
        assert!(!pkgs.legacy_kde);
        assert!(!pkgs.manual);
        assert_eq!(format!("{pkgs}"), "namtor");
    }

    #[test]
    fn detect_manual_install_without_manifest() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "usr/lib/namtor/namtor-daemon");
        let pkgs = detect_installed_packages(tmp.path());
        assert!(!pkgs.core);
        assert!(pkgs.manual);
        assert_eq!(format!("{pkgs}"), "manual");
    }

    #[test]
    fn detect_no_packages() {
        let tmp = TempDir::new().unwrap();
        let pkgs = detect_installed_packages(tmp.path());
        assert!(!pkgs.core);
        assert!(!pkgs.gnome);
        assert!(!pkgs.legacy_kde);
        assert!(!pkgs.manual);
        assert_eq!(format!("{pkgs}"), "none");
    }

    #[test]
    fn detect_legacy_setuid_artifact() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "usr/lib/namtor/namtor-worker-spawn");
        let artifacts = detect_migration_artifacts(tmp.path());
        assert!(artifacts.iter().any(|a| a.kind == "legacy-setuid"));
    }

    #[test]
    fn detect_legacy_install_script() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "usr/share/namtor/install.sh");
        let artifacts = detect_migration_artifacts(tmp.path());
        assert!(artifacts.iter().any(|a| a.kind == "legacy-install-script"));
    }

    #[test]
    fn detect_manual_install_artifact() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "usr/lib/namtor/namtor-daemon");
        let artifacts = detect_migration_artifacts(tmp.path());
        assert!(artifacts.iter().any(|a| a.kind == "manual-install"));
    }

    #[test]
    fn no_artifacts_when_clean_split_install() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "usr/lib/namtor/namtor-daemon");
        touch(tmp.path(), "usr/share/namtor/install-manifest.toml");
        let artifacts = detect_migration_artifacts(tmp.path());
        assert!(
            artifacts.is_empty(),
            "clean split install should have no artifacts: {artifacts:?}"
        );
    }

    #[test]
    fn migration_artifact_display_is_privacy_safe() {
        let a = MigrationArtifact {
            kind: "legacy-setuid",
            detail: "old setuid worker spawn helper detected".into(),
            remediation: "remove it and reinstall via split packages".into(),
        };
        let text = format!("{a}");
        assert!(text.contains("legacy-setuid"));
        assert!(
            !text.contains('/'),
            "no paths in migration artifact display: {text}"
        );
    }

    #[test]
    fn count_enabled_pam_services_zero_when_dir_missing() {
        assert_eq!(
            count_enabled_pam_services(Path::new("/nonexistent-pamd")),
            0
        );
    }

    #[test]
    fn count_enabled_pam_services_counts_namtor_entries() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        fs::create_dir_all(&pamd).unwrap();
        fs::write(
            pamd.join("gdm-password"),
            "auth sufficient pam_namtor.so\nauth required pam_unix.so\n",
        )
        .unwrap();
        fs::write(pamd.join("sudo"), "auth required pam_unix.so\n").unwrap();
        fs::write(pamd.join("sddm"), "auth sufficient pam_namtor.so\n").unwrap();
        assert_eq!(count_enabled_pam_services(&pamd), 2);
    }

    #[test]
    fn count_enabled_pam_services_skips_backups_and_dotfiles() {
        let tmp = TempDir::new().unwrap();
        let pamd = tmp.path().join("pam.d");
        fs::create_dir_all(&pamd).unwrap();
        fs::write(pamd.join("gdm-password"), "auth sufficient pam_namtor.so\n").unwrap();
        fs::write(
            pamd.join("gdm-password.bak"),
            "auth sufficient pam_namtor.so\n",
        )
        .unwrap();
        fs::write(
            pamd.join("gdm-password~"),
            "auth sufficient pam_namtor.so\n",
        )
        .unwrap();
        fs::write(pamd.join(".hidden"), "auth sufficient pam_namtor.so\n").unwrap();
        fs::write(
            pamd.join("sudo.namtor.bak"),
            "auth sufficient pam_namtor.so\n",
        )
        .unwrap();
        assert_eq!(count_enabled_pam_services(&pamd), 1);
    }

    #[test]
    fn redact_hex_id_short_unchanged() {
        assert_eq!(redact_hex_id("aabbccdd"), "aabbccdd");
        assert_eq!(redact_hex_id("short"), "short");
    }

    #[test]
    fn redact_hex_id_long_truncated() {
        let long = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let redacted = redact_hex_id(long);
        assert_eq!(redacted, "abcdef01...");
        assert!(!redacted.contains(&long[16..]));
    }

    #[test]
    fn redact_hex_id_colon_separated() {
        let colon = "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99";
        let redacted = redact_hex_id(colon);
        assert_eq!(redacted, "aabbccdd...");
    }

    #[test]
    fn redact_sha256_colon_truncates_to_first_4_bytes() {
        let digest = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x11, 0x22];
        let redacted = redact_sha256_colon(&digest);
        assert_eq!(redacted, "aa:bb:cc:dd...");
    }

    #[test]
    fn detect_model_state_all_present() {
        let tmp = TempDir::new().unwrap();
        let models = tmp.path().join("models");
        fs::create_dir_all(&models).unwrap();
        fs::write(models.join("yunet_2023mar.onnx"), b"x").unwrap();
        fs::write(models.join("sface_2021dec.onnx"), b"x").unwrap();
        fs::write(models.join("minifasnet_pad.onnx"), b"x").unwrap();
        let state = detect_model_state(&models);
        assert!(state.all_present());
        assert_eq!(format!("{state}"), "all present");
    }

    #[test]
    fn detect_model_state_missing_models() {
        let tmp = TempDir::new().unwrap();
        let models = tmp.path().join("models");
        fs::create_dir_all(&models).unwrap();
        fs::write(models.join("yunet_2023mar.onnx"), b"x").unwrap();
        let state = detect_model_state(&models);
        assert!(!state.all_present());
        assert_eq!(state.missing_names(), vec!["sface", "minifasnet"]);
        assert!(format!("{state}").contains("missing: sface, minifasnet"));
    }

    #[test]
    fn detect_model_state_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let models = tmp.path().join("models");
        fs::create_dir_all(&models).unwrap();
        let state = detect_model_state(&models);
        assert!(!state.all_present());
        assert_eq!(state.missing_names().len(), 3);
    }

    #[test]
    fn detect_service_state_active() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("auth.sock");
        fs::write(&sock, b"x").unwrap();
        assert_eq!(detect_service_state(&sock), ServiceState::Active);
    }

    #[test]
    fn detect_service_state_inactive() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("nonexistent.sock");
        assert_eq!(detect_service_state(&sock), ServiceState::Inactive);
    }

    #[test]
    fn protection_info_display_is_privacy_safe() {
        let info = ProtectionInfo {
            tpm_present: true,
            sealing: "tpm2",
        };
        let text = format!("{info}");
        assert!(text.contains("tpm: present"));
        assert!(text.contains("sealing: tpm2"));
        assert!(!text.contains("key"));
        assert!(!text.contains('/'));
    }

    #[test]
    fn protection_info_software_fallback() {
        let info = ProtectionInfo {
            tpm_present: false,
            sealing: "software",
        };
        let text = format!("{info}");
        assert!(text.contains("tpm: absent"));
        assert!(text.contains("sealing: software"));
    }

    #[test]
    fn detect_protection_state_uninitialized_when_no_key() {
        let tmp = TempDir::new().unwrap();
        let info = detect_protection_state(&tmp.path().join("nonexistent"));
        assert_eq!(info.sealing, "uninitialized");
    }

    #[test]
    fn detect_protection_state_software_when_key_no_tpm() {
        let tmp = TempDir::new().unwrap();
        let key = tmp.path().join("sealing");
        fs::write(&key, b"x").unwrap();
        // TPM check uses /dev/tpmrm0 which may or may not exist on the test
        // machine. We can't control it, so just verify the function doesn't
        // panic and returns a valid sealing string.
        let info = detect_protection_state(&key);
        assert!(info.sealing == "tpm2" || info.sealing == "software");
    }

    #[test]
    fn installed_packages_display_no_forbidden_tokens() {
        let pkgs = InstalledPackages {
            core: true,
            gnome: true,
            legacy_kde: false,
            manual: false,
        };
        let text = format!("{pkgs}");
        for forbidden in [
            "embedding",
            "template",
            "threshold",
            "score",
            "biometric",
            "gate",
            "raw",
            "path",
            "key",
        ] {
            assert!(
                !text.to_lowercase().contains(forbidden),
                "forbidden token '{forbidden}' in: {text}"
            );
        }
    }
}
