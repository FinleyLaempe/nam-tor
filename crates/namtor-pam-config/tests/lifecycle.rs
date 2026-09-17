// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the PAM registry lifecycle: snapshot, validate,
//! migrate, disable-all, purge.
//!
//! Tests: snapshot saves registry + PAM files, validate detects
//! inconsistencies, migrate preserves enabled services and rolls back on
//! failure, disable-all removes every block, purge leaves no Nam-tor state.

use std::fs;
use std::path::{Path, PathBuf};

use namtor_pam_config::{
    disable_all_services, enable_service, migrate, purge, snapshot, validate, EditOptions,
    NamtorControl, Registry, ValidationStatus,
};
use tempfile::TempDir;

// --- fixture helpers --------------------------------------------------------

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/tests/pam-fixtures")
}

fn copy_fixture(src: &str, dest_dir: &Path) {
    let src_path = fixture_dir().join(src);
    let content = fs::read_to_string(&src_path)
        .unwrap_or_else(|_| panic!("fixture {src} not found at {}", src_path.display()));
    let dest = dest_dir.join(Path::new(src).file_name().unwrap());
    fs::write(&dest, content).expect("write fixture");
}

fn build_pamd(fixtures: &[&str]) -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let pamd = tmp.path().join("pam.d");
    let reg = tmp.path().join("namtor");
    fs::create_dir_all(&pamd).expect("mkdir pam.d");
    fs::create_dir_all(&reg).expect("mkdir reg");
    // Stage a stub pam_namtor.so so the enable preflight finds a module.
    // Existence is all the preflight checks, so an empty file suffices.
    stage_module(tmp.path());
    for f in fixtures {
        copy_fixture(f, &pamd);
    }
    (tmp, pamd, reg)
}

/// Create an empty `pam_namtor.so` in the canonical module directory under
/// `root`, so `enable_service` with `module_root = root` passes preflight.
fn stage_module(root: &Path) {
    let dir = root.join("usr/lib/security");
    fs::create_dir_all(&dir).expect("mkdir security");
    fs::write(dir.join("pam_namtor.so"), b"").expect("write stub module");
}

/// The staging root for a pamd path produced by `build_pamd`: the temp dir
/// that holds both `pam.d` and `usr/lib/security`.
fn module_root_for(pamd: &Path) -> PathBuf {
    pamd.parent().expect("pamd has a parent").to_path_buf()
}

fn opts(pamd: PathBuf, reg: PathBuf) -> EditOptions {
    let root = module_root_for(&pamd);
    EditOptions::new(pamd, reg).with_module_root(root)
}

// --- snapshot tests ---------------------------------------------------------

#[test]
fn snapshot_saves_registry_and_pam_files() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth", "arch/sudo"]);
    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    enable_service("sudo", &o).unwrap();

    let snap_dir = snapshot(&o).unwrap();
    assert!(snap_dir.join("registry.json").exists());
    assert!(snap_dir.join("pam.d").join("gdm-password").exists());
    assert!(snap_dir.join("pam.d").join("sudo").exists());
}

#[test]
fn snapshot_of_empty_registry_creates_empty_snapshot() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd, reg);
    let snap_dir = snapshot(&o).unwrap();
    assert!(snap_dir.join("registry.json").exists());
    // No PAM files since no services are enabled.
    let pam_snapshot = snap_dir.join("pam.d");
    if pam_snapshot.exists() {
        assert!(fs::read_dir(&pam_snapshot).unwrap().count() == 0);
    }
}

// --- validate tests ---------------------------------------------------------

#[test]
fn validate_passes_when_consistent() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth", "arch/sudo"]);
    let o = opts(pamd.clone(), reg);
    enable_service("gdm-password", &o).unwrap();
    enable_service("sudo", &o).unwrap();

    let report = validate(&o).unwrap();
    assert_eq!(report.status, ValidationStatus::Consistent);
    assert!(report.inconsistencies.is_empty());
}

#[test]
fn validate_detects_missing_block() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth", "arch/sudo"]);
    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    enable_service("sudo", &o).unwrap();

    // Manually remove the block from gdm-password (simulate external edit).
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    let stock = content
        .lines()
        .filter(|l| !l.contains("namtor"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(pamd.join("gdm-password"), format!("{stock}\n")).unwrap();

    let report = validate(&o).unwrap();
    assert_eq!(report.status, ValidationStatus::Inconsistent);
    assert!(report
        .inconsistencies
        .iter()
        .any(|i| i.service_id == "gdm-password" && i.kind == "missing_block"));
}

#[test]
fn validate_detects_orphaned_block() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth", "arch/sudo"]);
    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    // Manually add a block to sudo (not in registry).
    let content = fs::read_to_string(pamd.join("sudo")).unwrap();
    let block = "# namtor-begin\nauth sufficient pam_namtor.so\n# namtor-end\n";
    let first_auth = content.find("auth").unwrap();
    let injected = format!(
        "{}{}{}",
        &content[..first_auth],
        block,
        &content[first_auth..]
    );
    fs::write(pamd.join("sudo"), injected).unwrap();

    let report = validate(&o).unwrap();
    assert_eq!(report.status, ValidationStatus::Inconsistent);
    assert!(report
        .inconsistencies
        .iter()
        .any(|i| i.service_id == "sudo" && i.kind == "orphaned_block"));
}

#[test]
fn validate_passes_on_empty_registry() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd, reg);
    let report = validate(&o).unwrap();
    assert_eq!(report.status, ValidationStatus::Consistent);
}

// --- migrate tests ----------------------------------------------------------

#[test]
fn migrate_preserves_enabled_services() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth", "arch/sudo"]);
    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    enable_service("sudo", &o).unwrap();

    let report = migrate(&o).unwrap();
    assert!(report
        .migrated_services
        .contains(&"gdm-password".to_string()));
    assert!(report.migrated_services.contains(&"sudo".to_string()));
    assert!(report.restored);

    // Services still have the block.
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(content.contains("# namtor-begin"));
    let content = fs::read_to_string(pamd.join("sudo")).unwrap();
    assert!(content.contains("# namtor-begin"));

    // Registry still tracks them.
    let registry = Registry::load(&reg).unwrap();
    assert!(registry.is_enabled("gdm-password"));
    assert!(registry.is_enabled("sudo"));
}

#[test]
fn migrate_updates_block_format() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o_sufficient = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o_sufficient).unwrap();

    // Migrate with a different control (simulates format change).
    let o_ignore = EditOptions {
        control: NamtorControl::DefaultIgnore,
        ..opts(pamd.clone(), reg.clone())
    };
    let report = migrate(&o_ignore).unwrap();
    assert!(report
        .migrated_services
        .contains(&"gdm-password".to_string()));

    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(content.contains("auth [default=ignore] pam_namtor.so"));
    assert!(!content.contains("auth sufficient pam_namtor.so"));
}

#[test]
fn migrate_on_empty_registry_is_noop() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd, reg);
    let report = migrate(&o).unwrap();
    assert!(report.migrated_services.is_empty());
    assert!(report.restored);
}

#[test]
fn failed_migration_restores_previous_state() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth", "arch/sudo"]);
    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    enable_service("sudo", &o).unwrap();

    // Tamper with sudo's backup so re-enable fails (stale edit).
    let backup = pamd.join("sudo.namtor.bak");
    fs::write(&backup, "tampered backup content").unwrap();

    let result = migrate(&o);
    assert!(result.is_err(), "migrate should fail on stale edit");

    // gdm-password should still have its block (restored from snapshot).
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(
        content.contains("# namtor-begin"),
        "gdm-password block must be restored after failed migration"
    );

    // Registry should still track gdm-password.
    let registry = Registry::load(&reg).unwrap();
    assert!(
        registry.is_enabled("gdm-password"),
        "registry must be restored after failed migration"
    );
}

// --- disable-all tests ------------------------------------------------------

#[test]
fn disable_all_removes_every_block() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth", "arch/sudo"]);
    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    enable_service("sudo", &o).unwrap();

    let disabled = disable_all_services(&o).unwrap();
    assert!(disabled.contains(&"gdm-password".to_string()));
    assert!(disabled.contains(&"sudo".to_string()));

    for svc in &["gdm-password", "sudo"] {
        let content = fs::read_to_string(pamd.join(svc)).unwrap();
        assert!(!content.contains("namtor"), "{svc} still has namtor");
    }

    let registry = Registry::load(&reg).unwrap();
    assert!(registry.services().is_empty());
}

#[test]
fn disable_all_is_idempotent_on_empty_registry() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd, reg);
    let disabled = disable_all_services(&o).unwrap();
    assert!(disabled.is_empty());
}

#[test]
fn disable_all_preserves_stock_lines() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd.clone(), reg.clone());
    let original = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    enable_service("gdm-password", &o).unwrap();
    disable_all_services(&o).unwrap();

    let after = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert_eq!(after, original, "stock lines must be preserved");
}

// --- purge tests ------------------------------------------------------------

#[test]
fn purge_removes_all_pam_state() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth", "arch/sudo"]);
    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    enable_service("sudo", &o).unwrap();

    purge(&o).unwrap();

    // No PAM files have namtor blocks.
    for entry in fs::read_dir(&pamd).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.contains(".namtor.") {
            // Backup files should be gone.
            panic!("backup file left behind: {name}");
        }
        if let Ok(content) = fs::read_to_string(entry.path()) {
            assert!(
                !content.contains("namtor"),
                "PAM file {} still has namtor",
                name
            );
        }
    }

    // Registry file should be gone.
    assert!(
        !reg.join("pam-registry.json").exists(),
        "registry file must be purged"
    );
}

#[test]
fn purge_is_idempotent_on_clean_state() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd, reg);
    purge(&o).unwrap();
}

#[test]
fn purge_removes_backup_files() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    assert!(pamd.join("gdm-password.namtor.bak").exists());

    purge(&o).unwrap();
    assert!(!pamd.join("gdm-password.namtor.bak").exists());
    assert!(!pamd.join("gdm-password.namtor.bak.sha256").exists());
}

// --- lifecycle matrix tests -------------------------------------------------

#[test]
fn lifecycle_matrix_upgrade_preserves_data_and_config() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth", "arch/sudo"]);
    let o = opts(pamd.clone(), reg.clone());

    // Phase 1: initial install.
    enable_service("gdm-password", &o).unwrap();
    enable_service("sudo", &o).unwrap();

    // Phase 2: snapshot before upgrade.
    let _snap = snapshot(&o).unwrap();

    // Phase 3: migrate (upgrade).
    let report = migrate(&o).unwrap();
    assert!(report.restored);

    // Phase 4: verify data/config preserved.
    let registry = Registry::load(&reg).unwrap();
    assert!(registry.is_enabled("gdm-password"));
    assert!(registry.is_enabled("sudo"));

    for svc in &["gdm-password", "sudo"] {
        let content = fs::read_to_string(pamd.join(svc)).unwrap();
        assert!(content.contains("# namtor-begin"), "{svc} lost block");
        assert!(content.contains("pam_unix.so"), "{svc} lost fallback");
    }
}

#[test]
fn lifecycle_matrix_failed_migration_rollback() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth", "arch/sudo"]);
    let o = opts(pamd.clone(), reg.clone());

    // Initial install.
    enable_service("gdm-password", &o).unwrap();
    enable_service("sudo", &o).unwrap();

    // Corrupt sudo's backup to make re-enable fail.
    fs::write(pamd.join("sudo.namtor.bak"), "corrupted").unwrap();

    // Migrate should fail and restore.
    let result = migrate(&o);
    assert!(result.is_err());

    // gdm-password must still be operational.
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(content.contains("# namtor-begin"));
    assert!(content.contains("pam_unix.so"));

    let registry = Registry::load(&reg).unwrap();
    assert!(registry.is_enabled("gdm-password"));
}

#[test]
fn lifecycle_matrix_removal_purges_all() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth", "arch/sudo"]);
    let o = opts(pamd.clone(), reg.clone());

    // Initial install.
    enable_service("gdm-password", &o).unwrap();
    enable_service("sudo", &o).unwrap();

    // Removal sequence: disable-all → purge.
    disable_all_services(&o).unwrap();
    purge(&o).unwrap();

    // No Nam-tor state left.
    for entry in fs::read_dir(&pamd).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(!name.contains(".namtor."), "backup file left: {name}");
        if let Ok(content) = fs::read_to_string(entry.path()) {
            assert!(!content.contains("namtor"), "PAM file {name} has namtor");
        }
    }
    assert!(!reg.join("pam-registry.json").exists());
}
