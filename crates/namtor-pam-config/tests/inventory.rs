// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for PAM service inventory and classification.
//!
//! Uses fixtures from `packaging/tests/pam-fixtures/` — copies them into
//! a temp `pam.d/` directory and scans descriptor-relative.

use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use namtor_pam_config::{
    classify_service, known_service_label, scan_pamd, PamServiceCompatibility, ScanError,
    ServiceStatus,
};
use tempfile::TempDir;

// --- fixture helpers -------------------------------------------------------

/// Path to the fixture directory in the repo.
fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/tests/pam-fixtures")
}

/// Copy a fixture file into a temp pam.d directory.
fn copy_fixture(src: &str, dest_dir: &Path) {
    let src_path = fixture_dir().join(src);
    let content = fs::read_to_string(&src_path)
        .unwrap_or_else(|_| panic!("fixture {src} not found at {}", src_path.display()));
    let dest = dest_dir.join(Path::new(src).file_name().unwrap());
    fs::write(&dest, content).expect("write fixture");
}

/// Build a temp pam.d directory from a list of fixture paths (relative to
/// the fixture root). Returns the temp dir (kept alive) and the pam.d path.
fn build_pamd(fixtures: &[&str]) -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let pamd = tmp.path().join("pam.d");
    fs::create_dir_all(&pamd).expect("mkdir pam.d");
    for f in fixtures {
        copy_fixture(f, &pamd);
    }
    (tmp, pamd)
}

// --- Arch classification tests --------------------------------------------

#[test]
fn arch_gdm_password_classifies_safe_disabled() {
    let (_tmp, pamd) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let info =
        classify_service("gdm-password", &pamd.join("gdm-password"), &pamd).expect("classify");
    assert_eq!(info.id, "gdm-password");
    assert!(!info.enabled);
    assert_eq!(info.compatibility, PamServiceCompatibility::Safe);
    assert_eq!(info.status(), ServiceStatus::Disabled);
    assert!(
        info.reason.contains("safe"),
        "reason should mention safe: {}",
        info.reason
    );
}

#[test]
fn arch_gdm_password_has_known_label() {
    let (_tmp, pamd) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let info =
        classify_service("gdm-password", &pamd.join("gdm-password"), &pamd).expect("classify");
    assert!(info.label.contains("GDM"));
    assert!(info.label.contains("GNOME"));
}

#[test]
fn arch_sudo_classifies_safe_disabled() {
    let (_tmp, pamd) = build_pamd(&["arch/sudo", "arch/system-auth"]);
    let info = classify_service("sudo", &pamd.join("sudo"), &pamd).expect("classify");
    assert!(!info.enabled);
    assert_eq!(info.compatibility, PamServiceCompatibility::Safe);
    assert_eq!(info.status(), ServiceStatus::Disabled);
}

#[test]
fn arch_su_classifies_safe_disabled() {
    let (_tmp, pamd) = build_pamd(&["arch/su", "arch/system-auth"]);
    let info = classify_service("su", &pamd.join("su"), &pamd).expect("classify");
    assert!(!info.enabled);
    assert_eq!(info.compatibility, PamServiceCompatibility::Safe);
    assert_eq!(info.status(), ServiceStatus::Disabled);
}

#[test]
fn arch_polkit_classifies_safe_disabled() {
    let (_tmp, pamd) = build_pamd(&["arch/polkit-1", "arch/system-auth"]);
    let info = classify_service("polkit-1", &pamd.join("polkit-1"), &pamd).expect("classify");
    assert!(!info.enabled);
    assert_eq!(info.compatibility, PamServiceCompatibility::Safe);
    assert_eq!(info.status(), ServiceStatus::Disabled);
}

#[test]
fn arch_system_auth_has_known_label() {
    assert_eq!(
        known_service_label("system-auth"),
        "System Auth (Arch/RHEL shared auth stack)"
    );
}

// --- Fedora classification tests ------------------------------------------

#[test]
fn fedora_sddm_classifies_risky_due_to_deny() {
    let (_tmp, pamd) = build_pamd(&["fedora/sddm", "fedora/password-auth"]);
    let info = classify_service("sddm", &pamd.join("sddm"), &pamd).expect("classify");
    assert!(!info.enabled);
    // password-auth has pam_deny.so — risky.
    assert_eq!(info.compatibility, PamServiceCompatibility::Risky);
    assert_eq!(info.status(), ServiceStatus::Risky);
    assert!(
        info.reason.contains("pam_deny"),
        "reason should mention pam_deny: {}",
        info.reason
    );
}

#[test]
fn fedora_sudo_classifies_risky_due_to_deny() {
    let (_tmp, pamd) = build_pamd(&["fedora/sudo", "fedora/password-auth"]);
    let info = classify_service("sudo", &pamd.join("sudo"), &pamd).expect("classify");
    assert_eq!(info.compatibility, PamServiceCompatibility::Risky);
}

#[test]
fn fedora_su_classifies_risky_due_to_deny() {
    let (_tmp, pamd) = build_pamd(&["fedora/su", "fedora/password-auth"]);
    let info = classify_service("su", &pamd.join("su"), &pamd).expect("classify");
    assert_eq!(info.compatibility, PamServiceCompatibility::Risky);
}

#[test]
fn fedora_password_auth_has_known_label() {
    assert_eq!(
        known_service_label("password-auth"),
        "Password Auth (Fedora/RHEL shared auth stack)"
    );
}

// --- Debian classification tests ------------------------------------------

#[test]
fn debian_common_auth_classifies_risky_due_to_deny() {
    let (_tmp, pamd) = build_pamd(&["debian/common-auth"]);
    let info = classify_service("common-auth", &pamd.join("common-auth"), &pamd).expect("classify");
    assert!(!info.enabled);
    // common-auth has pam_deny.so — risky.
    assert_eq!(info.compatibility, PamServiceCompatibility::Risky);
    assert_eq!(info.status(), ServiceStatus::Risky);
}

#[test]
fn debian_gdm_password_includes_common_auth_and_classifies_risky() {
    let (_tmp, pamd) = build_pamd(&["debian/gdm-password", "debian/common-auth"]);
    let info =
        classify_service("gdm-password", &pamd.join("gdm-password"), &pamd).expect("classify");
    assert!(!info.enabled);
    // gdm-password @includes common-auth which has pam_deny — risky.
    assert_eq!(info.compatibility, PamServiceCompatibility::Risky);
}

#[test]
fn debian_sudo_includes_common_auth() {
    let (_tmp, pamd) = build_pamd(&["debian/sudo", "debian/common-auth"]);
    let info = classify_service("sudo", &pamd.join("sudo"), &pamd).expect("classify");
    assert!(!info.enabled);
    assert_eq!(info.compatibility, PamServiceCompatibility::Risky);
}

#[test]
fn debian_su_includes_common_auth() {
    let (_tmp, pamd) = build_pamd(&["debian/su", "debian/common-auth"]);
    let info = classify_service("su", &pamd.join("su"), &pamd).expect("classify");
    assert!(!info.enabled);
    assert_eq!(info.compatibility, PamServiceCompatibility::Risky);
}

#[test]
fn debian_polkit_includes_common_auth() {
    let (_tmp, pamd) = build_pamd(&["debian/polkit-1", "debian/common-auth"]);
    let info = classify_service("polkit-1", &pamd.join("polkit-1"), &pamd).expect("classify");
    assert!(!info.enabled);
    assert_eq!(info.compatibility, PamServiceCompatibility::Risky);
}

#[test]
fn debian_common_auth_has_known_label() {
    assert_eq!(
        known_service_label("common-auth"),
        "Common Auth (Debian shared auth stack)"
    );
}

// --- Enabled service test --------------------------------------------------

#[test]
fn service_with_pam_namtor_already_present_is_enabled() {
    let (_tmp, pamd) = build_pamd(&["reject/enabled-service"]);
    let info = classify_service("enabled-service", &pamd.join("enabled-service"), &pamd)
        .expect("classify");
    assert!(info.enabled);
    assert_eq!(info.compatibility, PamServiceCompatibility::Safe);
    assert_eq!(info.status(), ServiceStatus::Enabled);
    assert!(info.reason.contains("already present"));
}

// --- Unsupported cases -----------------------------------------------------

#[test]
fn service_with_no_auth_lines_is_unsupported() {
    let (_tmp, pamd) = build_pamd(&["reject/no-auth-lines"]);
    let info =
        classify_service("no-auth-lines", &pamd.join("no-auth-lines"), &pamd).expect("classify");
    assert!(!info.enabled);
    assert_eq!(info.compatibility, PamServiceCompatibility::Unsupported);
    assert_eq!(info.status(), ServiceStatus::Unsupported);
    assert!(info.reason.contains("no auth"));
}

#[test]
fn service_with_no_password_fallback_is_unsupported() {
    let (_tmp, pamd) = build_pamd(&["reject/no-fallback"]);
    let info = classify_service("no-fallback", &pamd.join("no-fallback"), &pamd).expect("classify");
    assert!(!info.enabled);
    assert_eq!(info.compatibility, PamServiceCompatibility::Unsupported);
    assert_eq!(info.status(), ServiceStatus::Unsupported);
    assert!(info.reason.contains("no password-fallback"));
}

#[test]
fn service_with_substack_is_risky() {
    let (_tmp, pamd) = build_pamd(&["reject/substack-service"]);
    let info = classify_service("substack-service", &pamd.join("substack-service"), &pamd)
        .expect("classify");
    assert!(!info.enabled);
    assert_eq!(info.compatibility, PamServiceCompatibility::Risky);
    assert_eq!(info.status(), ServiceStatus::Risky);
    assert!(info.reason.contains("substack"));
}

// --- Scanner rejection tests -----------------------------------------------

#[test]
fn scanner_rejects_symlinks() {
    let tmp = TempDir::new().expect("tempdir");
    let pamd = tmp.path().join("pam.d");
    fs::create_dir_all(&pamd).expect("mkdir");
    // Write a real file.
    fs::write(
        pamd.join("real-service"),
        "#%PAM-1.0\nauth required pam_unix.so\n",
    )
    .expect("write");
    // Create a symlink to it.
    symlink(pamd.join("real-service"), pamd.join("linked-service")).expect("symlink");

    let result = scan_pamd(&pamd);
    assert!(
        matches!(result, Err(ScanError::SymlinkRejected(_))),
        "symlink should be rejected, got: {result:?}"
    );
}

#[test]
fn scanner_skips_backup_files() {
    let tmp = TempDir::new().expect("tempdir");
    let pamd = tmp.path().join("pam.d");
    fs::create_dir_all(&pamd).expect("mkdir");
    fs::write(pamd.join("sudo"), "#%PAM-1.0\nauth required pam_unix.so\n").expect("write");
    fs::write(pamd.join("sudo~"), "#%PAM-1.0\nauth required pam_unix.so\n").expect("write");
    fs::write(
        pamd.join("sudo.bak"),
        "#%PAM-1.0\nauth required pam_unix.so\n",
    )
    .expect("write");
    fs::write(
        pamd.join("sudo.namtor.bak"),
        "#%PAM-1.0\nauth required pam_unix.so\n",
    )
    .expect("write");

    let files = scan_pamd(&pamd).expect("scan");
    let ids: Vec<&str> = files.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, vec!["sudo"], "only non-backup files should be scanned");
}

#[test]
fn scanner_skips_dotfiles() {
    let tmp = TempDir::new().expect("tempdir");
    let pamd = tmp.path().join("pam.d");
    fs::create_dir_all(&pamd).expect("mkdir");
    fs::write(pamd.join("sudo"), "#%PAM-1.0\nauth required pam_unix.so\n").expect("write");
    fs::write(
        pamd.join(".hidden"),
        "#%PAM-1.0\nauth required pam_unix.so\n",
    )
    .expect("write");

    let files = scan_pamd(&pamd).expect("scan");
    let ids: Vec<&str> = files.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, vec!["sudo"]);
}

#[test]
fn scanner_skips_rpm_and_dpkg_save_files() {
    let tmp = TempDir::new().expect("tempdir");
    let pamd = tmp.path().join("pam.d");
    fs::create_dir_all(&pamd).expect("mkdir");
    fs::write(pamd.join("sudo"), "#%PAM-1.0\nauth required pam_unix.so\n").expect("write");
    fs::write(pamd.join("sudo.rpmsave"), "x\n").expect("write");
    fs::write(pamd.join("sudo.rpmnew"), "x\n").expect("write");
    fs::write(pamd.join("sudo.dpkg-old"), "x\n").expect("write");

    let files = scan_pamd(&pamd).expect("scan");
    let ids: Vec<&str> = files.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, vec!["sudo"]);
}

#[test]
fn scanner_returns_sorted_results() {
    let tmp = TempDir::new().expect("tempdir");
    let pamd = tmp.path().join("pam.d");
    fs::create_dir_all(&pamd).expect("mkdir");
    for name in &["zebra", "alpha", "mid"] {
        fs::write(pamd.join(name), "#%PAM-1.0\nauth required pam_unix.so\n").expect("write");
    }

    let files = scan_pamd(&pamd).expect("scan");
    let ids: Vec<&str> = files.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, vec!["alpha", "mid", "zebra"]);
}

#[test]
fn scanner_rejects_nonexistent_directory() {
    let result = scan_pamd(Path::new("/nonexistent/pam.d"));
    assert!(matches!(result, Err(ScanError::NotADirectory(_))));
}

// --- Include cycle and traversal tests -------------------------------------

#[test]
fn include_cycle_is_detected() {
    let (_tmp, pamd) = build_pamd(&["reject/cycle-target-a", "reject/cycle-target-b"]);
    let result = classify_service("cycle-target-a", &pamd.join("cycle-target-a"), &pamd);
    assert!(
        matches!(result, Err(ScanError::IncludeCycle(_))),
        "cycle should be detected, got: {result:?}"
    );
}

#[test]
fn directory_traversal_in_include_is_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let pamd = tmp.path().join("pam.d");
    fs::create_dir_all(&pamd).expect("mkdir");
    // Write a service that tries to include a path with traversal.
    fs::write(
        pamd.join("traversal-service"),
        "#%PAM-1.0\nauth required pam_unix.so\n@include ../../../etc/passwd\n",
    )
    .expect("write");

    let result = classify_service("traversal-service", &pamd.join("traversal-service"), &pamd);
    assert!(
        matches!(result, Err(ScanError::TraversalRejected(_))),
        "traversal should be rejected, got: {result:?}"
    );
}

// --- Full inventory scan test ----------------------------------------------

#[test]
fn full_arch_inventory_classifies_all_services() {
    let (_tmp, pamd) = build_pamd(&[
        "arch/gdm-password",
        "arch/sudo",
        "arch/su",
        "arch/polkit-1",
        "arch/system-auth",
    ]);

    let files = scan_pamd(&pamd).expect("scan");
    // 5 files, all non-backup.
    assert_eq!(files.len(), 5);

    let mut results = Vec::new();
    for f in &files {
        let info = classify_service(&f.id, &f.path, &pamd).expect("classify");
        results.push(info);
    }

    // All Arch services should be safe (pam_unix fallback, no pam_deny in arch).
    for info in &results {
        match info.id.as_str() {
            "gdm-password" | "sudo" | "su" | "polkit-1" | "system-auth" => {
                assert_eq!(
                    info.compatibility,
                    PamServiceCompatibility::Safe,
                    "{} should be safe: {}",
                    info.id,
                    info.reason
                );
            }
            _ => {}
        }
    }
}

#[test]
fn full_fedora_inventory_classifies_all_services() {
    let (_tmp, pamd) = build_pamd(&[
        "fedora/sddm",
        "fedora/sudo",
        "fedora/su",
        "fedora/password-auth",
    ]);

    let files = scan_pamd(&pamd).expect("scan");
    assert_eq!(files.len(), 4);

    for f in &files {
        let info = classify_service(&f.id, &f.path, &pamd).expect("classify");
        // All Fedora services that include password-auth are risky (pam_deny).
        // password-auth itself is also risky.
        assert_eq!(
            info.compatibility,
            PamServiceCompatibility::Risky,
            "{} should be risky: {}",
            info.id,
            info.reason
        );
    }
}

#[test]
fn full_debian_inventory_classifies_all_services() {
    let (_tmp, pamd) = build_pamd(&[
        "debian/common-auth",
        "debian/gdm-password",
        "debian/sudo",
        "debian/su",
        "debian/polkit-1",
    ]);

    let files = scan_pamd(&pamd).expect("scan");
    assert_eq!(files.len(), 5);

    for f in &files {
        let info = classify_service(&f.id, &f.path, &pamd).expect("classify");
        // All Debian services are risky (common-auth has pam_deny).
        assert_eq!(
            info.compatibility,
            PamServiceCompatibility::Risky,
            "{} should be risky: {}",
            info.id,
            info.reason
        );
    }
}

// --- Determinism test ------------------------------------------------------

#[test]
fn classification_is_deterministic_across_runs() {
    let (_tmp, pamd) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);

    let info1 =
        classify_service("gdm-password", &pamd.join("gdm-password"), &pamd).expect("classify 1");
    let info2 =
        classify_service("gdm-password", &pamd.join("gdm-password"), &pamd).expect("classify 2");

    assert_eq!(info1, info2);
}

// --- Malformed file test ---------------------------------------------------

#[test]
fn malformed_file_with_only_comments_is_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let pamd = tmp.path().join("pam.d");
    fs::create_dir_all(&pamd).expect("mkdir");
    fs::write(pamd.join("empty-service"), "# just a comment\n# another\n").expect("write");

    let result = classify_service("empty-service", &pamd.join("empty-service"), &pamd);
    assert!(
        matches!(result, Err(ScanError::Malformed(_))),
        "comments-only file should be malformed, got: {result:?}"
    );
}

// --- Unknown service gets generated label ----------------------------------

#[test]
fn unknown_service_uses_id_as_label() {
    let tmp = TempDir::new().expect("tempdir");
    let pamd = tmp.path().join("pam.d");
    fs::create_dir_all(&pamd).expect("mkdir");
    fs::write(
        pamd.join("custom-app"),
        "#%PAM-1.0\nauth required pam_unix.so\naccount required pam_unix.so\n",
    )
    .expect("write");

    let info = classify_service("custom-app", &pamd.join("custom-app"), &pamd).expect("classify");
    assert_eq!(info.label, "custom-app");
    assert_eq!(info.compatibility, PamServiceCompatibility::Safe);
}

// --- SSH service is unsupported (PAM_RHOST) --------------------------------

#[test]
fn sshd_is_unsupported_due_to_rhost() {
    let tmp = TempDir::new().expect("tempdir");
    let pamd = tmp.path().join("pam.d");
    fs::create_dir_all(&pamd).expect("mkdir");
    fs::write(
        pamd.join("sshd"),
        "#%PAM-1.0\nauth required pam_unix.so\naccount required pam_unix.so\n",
    )
    .expect("write");

    let info = classify_service("sshd", &pamd.join("sshd"), &pamd).expect("classify");
    assert_eq!(info.compatibility, PamServiceCompatibility::Unsupported);
    assert_eq!(info.status(), ServiceStatus::Unsupported);
    assert!(info.reason.contains("PAM_RHOST"));
}

// --- Backup file inside pam.d is not scanned -------------------------------

#[test]
fn scanner_does_not_scan_namtor_backup_files() {
    let tmp = TempDir::new().expect("tempdir");
    let pamd = tmp.path().join("pam.d");
    fs::create_dir_all(&pamd).expect("mkdir");
    fs::write(
        pamd.join("gdm-password"),
        "#%PAM-1.0\nauth required pam_unix.so\n",
    )
    .expect("write");
    fs::write(
        pamd.join("gdm-password.namtor.bak"),
        "#%PAM-1.0\nauth required pam_unix.so\n",
    )
    .expect("write");
    // Write a .sha256 sidecar.
    let mut sha_file =
        fs::File::create(pamd.join("gdm-password.namtor.bak.sha256")).expect("create");
    writeln!(sha_file, "abc123").expect("write");

    let files = scan_pamd(&pamd).expect("scan");
    let ids: Vec<&str> = files.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, vec!["gdm-password"]);
}
