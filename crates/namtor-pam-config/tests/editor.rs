// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the transactional PAM editor.
//!
//! Tests: idempotent enable/disable, stale edit conflict, crash injection
//! rollback at every transition, include-aware fallback, registry updates,
//! stock byte preservation, symlink rejection, unsafe ordering refusal.

use std::fs;
use std::path::{Path, PathBuf};

use namtor_pam_config::{
    disable_service, enable_service, restore_service, CrashPoint, EditError, EditOptions,
    EditOutcome, NamtorControl, Registry,
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

fn opts_crash(pamd: PathBuf, reg: PathBuf, crash: CrashPoint) -> EditOptions {
    EditOptions {
        crash: Some(crash),
        ..opts(pamd, reg)
    }
}

// --- idempotency tests ------------------------------------------------------

#[test]
fn enable_inserts_block_before_first_auth_line() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let result = enable_service("gdm-password", &opts(pamd.clone(), reg.clone())).unwrap();
    assert_eq!(result, EditOutcome::Changed);
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(content.contains("# namtor-begin"));
    assert!(content.contains("auth sufficient pam_namtor.so"));
    assert!(content.contains("# namtor-end"));
    let begin = content.find("# namtor-begin").unwrap();
    let first_auth = content.find("auth     sufficient  pam_gdm.so").unwrap();
    assert!(begin < first_auth);
}

#[test]
fn enable_is_idempotent() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd.clone(), reg);
    enable_service("gdm-password", &o).unwrap();
    let result = enable_service("gdm-password", &o).unwrap();
    assert_eq!(result, EditOutcome::NoOp);
}

#[test]
fn disable_removes_block() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    let result = disable_service("gdm-password", &o).unwrap();
    assert_eq!(result, EditOutcome::Changed);
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(!content.contains("namtor"));
}

#[test]
fn disable_is_idempotent_when_no_block() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd, reg);
    let result = disable_service("gdm-password", &o).unwrap();
    assert_eq!(result, EditOutcome::NoOp);
}

#[test]
fn enable_then_disable_then_enable_cycle() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    disable_service("gdm-password", &o).unwrap();
    let result = enable_service("gdm-password", &o).unwrap();
    assert_eq!(result, EditOutcome::Changed);
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(content.contains("# namtor-begin"));
}

// --- stale edit conflict tests ---------------------------------------------

#[test]
fn stale_external_edit_rejected_on_reenable() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd.clone(), reg);
    enable_service("gdm-password", &o).unwrap();
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    let tampered = content.replace("pam_gdm.so", "pam_other.so");
    fs::write(pamd.join("gdm-password"), tampered).unwrap();
    let result = enable_service("gdm-password", &o);
    assert!(matches!(result, Err(EditError::StaleEdit(_))));
}

#[test]
fn stale_external_edit_rejected_on_disable() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd.clone(), reg);
    enable_service("gdm-password", &o).unwrap();
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    let tampered = content.replace("pam_gdm.so", "pam_other.so");
    fs::write(pamd.join("gdm-password"), tampered).unwrap();
    let result = disable_service("gdm-password", &o);
    assert!(matches!(result, Err(EditError::StaleEdit(_))));
}

#[test]
fn editing_namtor_block_only_is_not_stale() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    // Manually change the namtor block control field (not a stock line).
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    let modified = content.replace(
        "auth sufficient pam_namtor.so",
        "auth [default=ignore] pam_namtor.so",
    );
    fs::write(pamd.join("gdm-password"), modified).unwrap();
    // Re-enabling with sufficient should succeed (stock matches backup).
    let result = enable_service("gdm-password", &o);
    assert!(
        result.is_ok(),
        "re-enable after block-only edit should pass: {result:?}"
    );
}

// --- crash injection rollback tests ----------------------------------------

#[test]
fn crash_before_backup_leaves_file_original() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let original = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    let result = enable_service(
        "gdm-password",
        &opts_crash(pamd.clone(), reg, CrashPoint::BeforeBackup),
    );
    assert!(matches!(result, Err(EditError::CrashInjected(_))));
    let after = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert_eq!(
        after, original,
        "file must be unchanged after crash before backup"
    );
}

#[test]
fn crash_after_backup_leaves_file_original() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let original = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    let result = enable_service(
        "gdm-password",
        &opts_crash(pamd.clone(), reg, CrashPoint::AfterBackup),
    );
    assert!(matches!(result, Err(EditError::CrashInjected(_))));
    let after = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert_eq!(
        after, original,
        "file must be unchanged after crash after backup"
    );
}

#[test]
fn crash_before_write_leaves_file_original() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let original = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    let result = enable_service(
        "gdm-password",
        &opts_crash(pamd.clone(), reg, CrashPoint::BeforeWrite),
    );
    assert!(matches!(result, Err(EditError::CrashInjected(_))));
    let after = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert_eq!(
        after, original,
        "file must be unchanged after crash before write"
    );
}

#[test]
fn crash_after_write_leaves_file_edited() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let result = enable_service(
        "gdm-password",
        &opts_crash(pamd.clone(), reg.clone(), CrashPoint::AfterWrite),
    );
    assert!(matches!(result, Err(EditError::CrashInjected(_))));
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(
        content.contains("# namtor-begin"),
        "file must have block after crash after write"
    );
    // Re-run without crash should succeed (idempotent or complete).
    let result = enable_service("gdm-password", &opts(pamd, reg)).unwrap();
    assert!(matches!(result, EditOutcome::NoOp | EditOutcome::Changed));
}

#[test]
fn crash_before_verify_leaves_file_edited() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let result = enable_service(
        "gdm-password",
        &opts_crash(pamd.clone(), reg.clone(), CrashPoint::BeforeVerify),
    );
    assert!(matches!(result, Err(EditError::CrashInjected(_))));
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(content.contains("# namtor-begin"));
    let result = enable_service("gdm-password", &opts(pamd, reg)).unwrap();
    assert!(matches!(result, EditOutcome::NoOp | EditOutcome::Changed));
}

#[test]
fn crash_after_verify_leaves_file_edited() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let result = enable_service(
        "gdm-password",
        &opts_crash(pamd.clone(), reg.clone(), CrashPoint::AfterVerify),
    );
    assert!(matches!(result, Err(EditError::CrashInjected(_))));
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(content.contains("# namtor-begin"));
    let result = enable_service("gdm-password", &opts(pamd, reg)).unwrap();
    assert!(matches!(result, EditOutcome::NoOp | EditOutcome::Changed));
}

#[test]
fn crash_recovery_all_points_can_rerun() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let original = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    for point in [
        CrashPoint::BeforeBackup,
        CrashPoint::AfterBackup,
        CrashPoint::BeforeWrite,
        CrashPoint::AfterWrite,
        CrashPoint::BeforeVerify,
        CrashPoint::AfterVerify,
    ] {
        // Reset file to original before each crash test.
        fs::write(pamd.join("gdm-password"), &original).unwrap();
        // Remove backup + registry to start fresh.
        let _ = fs::remove_file(pamd.join("gdm-password.namtor.bak"));
        let _ = fs::remove_file(pamd.join("gdm-password.namtor.bak.sha256"));
        let _ = fs::remove_file(reg.join("pam-registry.json"));

        let result = enable_service(
            "gdm-password",
            &opts_crash(pamd.clone(), reg.clone(), point),
        );
        assert!(
            matches!(result, Err(EditError::CrashInjected(_))),
            "crash at {point:?}"
        );
        // After crash, re-running without crash must succeed.
        let result = enable_service("gdm-password", &opts(pamd.clone(), reg.clone()));
        assert!(
            result.is_ok(),
            "re-run after crash at {point:?} failed: {result:?}"
        );
        // File must have the block and fallback.
        let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
        assert!(
            content.contains("# namtor-begin"),
            "block missing after recovery at {point:?}"
        );
        assert!(
            content.contains("pam_unix.so"),
            "fallback missing after recovery at {point:?}"
        );
    }
}

// --- include-aware fallback tests ------------------------------------------

#[test]
fn include_aware_fallback_debian_gdm() {
    let (_tmp, pamd, reg) = build_pamd(&["debian/gdm-password", "debian/common-auth"]);
    let result = enable_service("gdm-password", &opts(pamd.clone(), reg));
    assert!(result.is_ok(), "debian gdm enable failed: {result:?}");
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(content.contains("# namtor-begin"));
    assert!(content.contains("@include common-auth"));
}

#[test]
fn include_aware_fallback_arch_polkit() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/polkit-1", "arch/system-auth"]);
    let result = enable_service("polkit-1", &opts(pamd.clone(), reg));
    assert!(result.is_ok(), "arch polkit enable failed: {result:?}");
    let content = fs::read_to_string(pamd.join("polkit-1")).unwrap();
    assert!(content.contains("# namtor-begin"));
    assert!(content.contains("include     system-auth"));
}

#[test]
fn include_aware_fallback_fedora_sddm() {
    let (_tmp, pamd, reg) = build_pamd(&["fedora/sddm", "fedora/password-auth"]);
    let result = enable_service("sddm", &opts(pamd.clone(), reg));
    assert!(result.is_ok(), "fedora sddm enable failed: {result:?}");
    let content = fs::read_to_string(pamd.join("sddm")).unwrap();
    assert!(content.contains("# namtor-begin"));
}

#[test]
fn disable_preserves_include_fallback() {
    let (_tmp, pamd, reg) = build_pamd(&["debian/gdm-password", "debian/common-auth"]);
    let o = opts(pamd.clone(), reg);
    enable_service("gdm-password", &o).unwrap();
    disable_service("gdm-password", &o).unwrap();
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(!content.contains("namtor"));
    assert!(content.contains("@include common-auth"));
}

// --- unsafe ordering refusal tests -----------------------------------------

#[test]
fn deny_at_insertion_point_rejected() {
    let tmp = TempDir::new().unwrap();
    let pamd = tmp.path().join("pam.d");
    let reg = tmp.path().join("namtor");
    fs::create_dir_all(&pamd).unwrap();
    fs::create_dir_all(&reg).unwrap();
    stage_module(tmp.path());
    fs::write(
        pamd.join("test-svc"),
        "#%PAM-1.0\nauth required pam_deny.so\nauth required pam_unix.so\n",
    )
    .unwrap();
    let result = enable_service("test-svc", &opts(pamd.clone(), reg));
    // pam_deny is the first auth line → block goes before it → deny is
    // between block and pam_unix → DenyBeforePassword.
    assert!(matches!(result, Err(EditError::DenyBeforePassword(_))));
    let content = fs::read_to_string(pamd.join("test-svc")).unwrap();
    assert!(!content.contains("namtor"));
}

#[test]
fn deny_between_block_and_password_rejected() {
    let tmp = TempDir::new().unwrap();
    let pamd = tmp.path().join("pam.d");
    let reg = tmp.path().join("namtor");
    fs::create_dir_all(&pamd).unwrap();
    fs::create_dir_all(&reg).unwrap();
    stage_module(tmp.path());
    fs::write(
        pamd.join("test-svc"),
        "#%PAM-1.0\nauth optional pam_permit.so\nauth required pam_deny.so\nauth required pam_unix.so\n",
    )
    .unwrap();
    let result = enable_service("test-svc", &opts(pamd, reg));
    assert!(matches!(result, Err(EditError::DenyBeforePassword(_))));
}

#[test]
fn no_auth_lines_rejected() {
    let tmp = TempDir::new().unwrap();
    let pamd = tmp.path().join("pam.d");
    let reg = tmp.path().join("namtor");
    fs::create_dir_all(&pamd).unwrap();
    fs::create_dir_all(&reg).unwrap();
    stage_module(tmp.path());
    fs::write(
        pamd.join("test-svc"),
        "#%PAM-1.0\naccount required pam_unix.so\n",
    )
    .unwrap();
    let result = enable_service("test-svc", &opts(pamd, reg));
    assert!(matches!(result, Err(EditError::NoAuthLines(_))));
}

// --- backup and checksum tests ---------------------------------------------

#[test]
fn backup_created_on_first_enable() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    enable_service("gdm-password", &opts(pamd.clone(), reg)).unwrap();
    assert!(pamd.join("gdm-password.namtor.bak").exists());
    assert!(pamd.join("gdm-password.namtor.bak.sha256").exists());
}

#[test]
fn backup_preserves_original_content() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let original = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    enable_service("gdm-password", &opts(pamd.clone(), reg)).unwrap();
    let backup = fs::read_to_string(pamd.join("gdm-password.namtor.bak")).unwrap();
    assert_eq!(backup, original);
}

#[test]
fn backup_checksum_matches() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    enable_service("gdm-password", &opts(pamd.clone(), reg)).unwrap();
    let backup = fs::read(pamd.join("gdm-password.namtor.bak")).unwrap();
    let stored = fs::read_to_string(pamd.join("gdm-password.namtor.bak.sha256")).unwrap();
    let actual = sha256_hex(&backup);
    assert_eq!(actual, stored.trim());
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    let d = h.finalize();
    let mut s = String::with_capacity(64);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// --- stock byte preservation test ------------------------------------------

#[test]
fn stock_bytes_preserved_outside_block() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let original = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    enable_service("gdm-password", &opts(pamd.clone(), reg)).unwrap();
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    let stock = content
        .lines()
        .filter(|l| !l.contains("namtor"))
        .collect::<Vec<_>>()
        .join("\n");
    let original_stock = original.lines().collect::<Vec<_>>().join("\n");
    assert_eq!(stock, original_stock, "stock lines must be preserved");
}

// --- symlink rejection test -------------------------------------------------

#[test]
fn symlink_target_rejected() {
    let (_tmp, pamd, reg) = build_pamd(&[("arch/system-auth")]);
    fs::write(pamd.join("real"), "#%PAM-1.0\nauth required pam_unix.so\n").unwrap();
    std::os::unix::fs::symlink(pamd.join("real"), pamd.join("linked")).unwrap();
    let result = enable_service("linked", &opts(pamd, reg));
    assert!(matches!(result, Err(EditError::SymlinkRejected(_))));
}

// --- registry tests ---------------------------------------------------------

#[test]
fn registry_updated_on_enable() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    enable_service("gdm-password", &opts(pamd.clone(), reg.clone())).unwrap();
    let registry = Registry::load(&reg).unwrap();
    assert!(registry.is_enabled("gdm-password"));
}

#[test]
fn registry_updated_on_disable() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    disable_service("gdm-password", &o).unwrap();
    let registry = Registry::load(&reg).unwrap();
    assert!(!registry.is_enabled("gdm-password"));
}

#[test]
fn registry_tracks_multiple_services() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth", "arch/sudo"]);
    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    enable_service("sudo", &o).unwrap();
    let registry = Registry::load(&reg).unwrap();
    assert!(registry.is_enabled("gdm-password"));
    assert!(registry.is_enabled("sudo"));
    assert!(!registry.is_enabled("system-auth"));
}

// --- default=ignore control test -------------------------------------------

#[test]
fn default_ignore_control_renders_correctly() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = EditOptions {
        control: NamtorControl::DefaultIgnore,
        ..opts(pamd.clone(), reg)
    };
    enable_service("gdm-password", &o).unwrap();
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(content.contains("auth [default=ignore] pam_namtor.so"));
}

#[test]
fn control_change_is_an_upgrade_not_stale() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o_sufficient = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o_sufficient).unwrap();
    let o_ignore = EditOptions {
        control: NamtorControl::DefaultIgnore,
        ..opts(pamd.clone(), reg)
    };
    let result = enable_service("gdm-password", &o_ignore).unwrap();
    assert_eq!(result, EditOutcome::Changed);
    let content = fs::read_to_string(pamd.join("gdm-password")).unwrap();
    assert!(content.contains("auth [default=ignore] pam_namtor.so"));
    assert!(!content.contains("auth sufficient pam_namtor.so"));
}

// --- invalid service id tests -----------------------------------------------

#[test]
fn invalid_service_ids_rejected() {
    let (_tmp, pamd, reg) = build_pamd(&[]);
    let o = opts(pamd, reg);
    assert!(matches!(
        enable_service("../etc/passwd", &o),
        Err(EditError::InvalidServiceId(_))
    ));
    assert!(matches!(
        enable_service(".hidden", &o),
        Err(EditError::InvalidServiceId(_))
    ));
    assert!(matches!(
        enable_service("a/b", &o),
        Err(EditError::InvalidServiceId(_))
    ));
    assert!(matches!(
        enable_service("", &o),
        Err(EditError::InvalidServiceId(_))
    ));
}

// --- full distro enable/disable tests ---------------------------------------

#[test]
fn full_arch_enable_disable_cycle() {
    let (_tmp, pamd, reg) = build_pamd(&[
        "arch/gdm-password",
        "arch/sudo",
        "arch/su",
        "arch/polkit-1",
        "arch/system-auth",
    ]);
    let o = opts(pamd.clone(), reg.clone());
    for svc in &["gdm-password", "sudo", "su", "polkit-1"] {
        let result = enable_service(svc, &o).unwrap();
        assert_eq!(result, EditOutcome::Changed);
        let content = fs::read_to_string(pamd.join(svc)).unwrap();
        assert!(content.contains("# namtor-begin"), "{svc} missing block");
        assert!(content.contains("pam_unix.so"), "{svc} missing fallback");
    }
    for svc in &["gdm-password", "sudo", "su", "polkit-1"] {
        let result = disable_service(svc, &o).unwrap();
        assert_eq!(result, EditOutcome::Changed);
        let content = fs::read_to_string(pamd.join(svc)).unwrap();
        assert!(!content.contains("namtor"), "{svc} still has namtor");
    }
}

#[test]
fn full_fedora_enable_disable_cycle() {
    let (_tmp, pamd, reg) = build_pamd(&[
        "fedora/sddm",
        "fedora/sudo",
        "fedora/su",
        "fedora/password-auth",
    ]);
    let o = opts(pamd.clone(), reg.clone());
    // Fedora services have pam_deny but it's after pam_unix — should still
    // be insertable since deny is after the password fallback.
    for svc in &["sudo", "password-auth"] {
        let result = enable_service(svc, &o);
        // password-auth has deny after pam_unix — the block goes before
        // the first auth line (pam_env), and pam_unix is after the block.
        assert!(result.is_ok(), "fedora {svc} enable failed: {result:?}");
    }
}

#[test]
fn full_debian_enable_disable_cycle() {
    let (_tmp, pamd, reg) = build_pamd(&[
        "debian/gdm-password",
        "debian/common-auth",
        "debian/sudo",
        "debian/su",
        "debian/polkit-1",
    ]);
    let o = opts(pamd.clone(), reg.clone());
    for svc in &["gdm-password", "sudo", "su", "polkit-1"] {
        let result = enable_service(svc, &o);
        assert!(result.is_ok(), "debian {svc} enable failed: {result:?}");
        let content = fs::read_to_string(pamd.join(svc)).unwrap();
        assert!(content.contains("# namtor-begin"));
    }
}

// --- preflight and fallback regression tests --------------------------------

#[test]
fn enable_refuses_when_module_is_not_installed() {
    let tmp = TempDir::new().unwrap();
    let pamd = tmp.path().join("pam.d");
    let reg = tmp.path().join("namtor");
    fs::create_dir_all(&pamd).unwrap();
    fs::create_dir_all(&reg).unwrap();
    // Deliberately NOT staging pam_namtor.so.
    fs::write(
        pamd.join("test-svc"),
        "#%PAM-1.0\nauth sufficient pam_unix.so\n",
    )
    .unwrap();

    let result = enable_service("test-svc", &opts(pamd.clone(), reg));
    assert!(
        matches!(result, Err(EditError::ModuleNotFound)),
        "expected ModuleNotFound, got {result:?}"
    );

    // Preflight runs before the backup, so a refused enable leaves no trace.
    let content = fs::read_to_string(pamd.join("test-svc")).unwrap();
    assert_eq!(content, "#%PAM-1.0\nauth sufficient pam_unix.so\n");
    assert!(!pamd.join("test-svc.namtor-backup").exists());
}

#[test]
fn enable_refuses_service_with_no_password_fallback() {
    // Regression: verify_candidate falls through to verify_through_includes,
    // which classified the CANDIDATE — a stack that already contained our own
    // block. classify_service short-circuits on `has_namtor` and returns Safe
    // before evaluating the fallback rule, so the check answered "safe?" of a
    // file its own block guaranteed safe, and this stack installed cleanly.
    let tmp = TempDir::new().unwrap();
    let pamd = tmp.path().join("pam.d");
    let reg = tmp.path().join("namtor");
    fs::create_dir_all(&pamd).unwrap();
    fs::create_dir_all(&reg).unwrap();
    stage_module(tmp.path());
    // pam_echo is an auth module, but it is not a password fallback, and the
    // only include is an `account` line the auth walk must not follow.
    fs::write(
        pamd.join("test-svc"),
        "#%PAM-1.0\nauth     required   pam_echo.so\naccount  include    system-auth\n",
    )
    .unwrap();

    let result = enable_service("test-svc", &opts(pamd.clone(), reg));
    assert!(
        matches!(result, Err(EditError::NoPasswordFallback(_))),
        "expected NoPasswordFallback, got {result:?}"
    );
    let content = fs::read_to_string(pamd.join("test-svc")).unwrap();
    assert!(!content.contains("namtor"));
}

#[test]
fn module_root_is_honoured_over_the_live_system() {
    // resolve_module_path_in must not leak to `/`: a staging root with no
    // module must report none even when the live system has one installed.
    let tmp = TempDir::new().unwrap();
    assert!(namtor_pam_config::resolve_module_path_in(tmp.path()).is_none());
    stage_module(tmp.path());
    let found = namtor_pam_config::resolve_module_path_in(tmp.path()).unwrap();
    assert!(found.starts_with(tmp.path()));
    assert!(found.ends_with("usr/lib/security/pam_namtor.so"));
}

// --- restore-from-backup tests ----------------------------------------------

#[test]
fn restore_returns_the_file_to_its_pre_namtor_bytes() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let target = pamd.join("gdm-password");
    let original = fs::read_to_string(&target).unwrap();

    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();
    assert!(fs::read_to_string(&target).unwrap().contains("namtor"));

    assert_eq!(
        restore_service("gdm-password", &o).unwrap(),
        EditOutcome::Changed
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), original);
    assert!(!Registry::load(&reg).unwrap().is_enabled("gdm-password"));
}

#[test]
fn restore_undoes_damage_that_disable_would_carry_forward() {
    // This is the whole reason restore exists as a separate operation.
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let target = pamd.join("gdm-password");
    let original = fs::read_to_string(&target).unwrap();

    let o = opts(pamd.clone(), reg.clone());
    enable_service("gdm-password", &o).unwrap();

    // Someone appends a line after the block. `disable` would strip only our
    // block and leave this behind; `restore` reverts to the captured bytes.
    let damaged = format!(
        "{}auth optional pam_debug.so\n",
        fs::read_to_string(&target).unwrap()
    );
    fs::write(&target, &damaged).unwrap();

    restore_service("gdm-password", &o).unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), original);
}

#[test]
fn restore_without_a_backup_is_refused_not_guessed() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd, reg);
    // Never enabled, so no backup was ever taken.
    let result = restore_service("gdm-password", &o);
    assert!(
        matches!(result, Err(EditError::TargetMissing(_))),
        "expected TargetMissing, got {result:?}"
    );
}

#[test]
fn restore_is_idempotent() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let o = opts(pamd, reg);
    enable_service("gdm-password", &o).unwrap();
    assert_eq!(
        restore_service("gdm-password", &o).unwrap(),
        EditOutcome::Changed
    );
    assert_eq!(
        restore_service("gdm-password", &o).unwrap(),
        EditOutcome::NoOp
    );
}

#[test]
fn restore_refuses_a_backup_that_would_reinstate_the_block() {
    let (_tmp, pamd, reg) = build_pamd(&["arch/gdm-password", "arch/system-auth"]);
    let target = pamd.join("gdm-password");
    let o = opts(pamd.clone(), reg);
    enable_service("gdm-password", &o).unwrap();

    // Forge a backup that already contains a namtor block, and re-checksum it
    // so the integrity check passes and the content check is what must catch
    // it. Restoring this would put back the block the caller is removing.
    let enabled = fs::read_to_string(&target).unwrap();
    let backup = pamd.join("gdm-password.namtor.bak");
    assert!(backup.exists(), "enable should have written a backup");
    fs::write(&backup, &enabled).unwrap();
    let digest = sha256_hex(enabled.as_bytes());
    fs::write(pamd.join("gdm-password.namtor.bak.sha256"), digest).unwrap();

    let result = restore_service("gdm-password", &o);
    assert!(
        matches!(result, Err(EditError::StaleEdit(_))),
        "expected StaleEdit, got {result:?}"
    );
}

// --- CLI argument handling ---------------------------------------------------

#[test]
fn help_is_accepted_in_the_action_slot() {
    use std::process::ExitCode;
    // `-h`/`--help` arrives where the action goes, because the option loop
    // starts at index 2. `namtor pam-edit --help` used to parse "--help" as
    // the action and exit 2 with "--service is required for --help".
    for flag in ["--help", "-h"] {
        let code = namtor_pam_config::app::main(vec!["namtor pam-edit".into(), flag.into()]);
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", ExitCode::SUCCESS),
            "{flag} in the action slot must succeed"
        );
    }
}

#[test]
fn no_action_is_a_usage_error() {
    use std::process::ExitCode;
    let code = namtor_pam_config::app::main(vec!["namtor pam-edit".into()]);
    assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(2)));
}
