// SPDX-License-Identifier: GPL-3.0-only
//! Transactional PAM install lifecycle tests.
//!
//! Exercises `packaging/pam-install.sh` and `packaging/pam-uninstall.sh` via
//! `std::process::Command` with `DESTDIR` set to a temp root — no root
//! required. Asserts the task-37 acceptance criteria:
//!   - install adds the namtor block to the PAM stack
//!   - uninstall removes the block
//!   - upgrade (re-install) preserves existing enrollments (templates)
//!   - uninstall does not remove templates
//!   - the atomic rename leaves no temp files in the target directory

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

/// Repo root: `crates/namtor-pam` → two levels up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .to_path_buf()
}

fn packaging_dir() -> PathBuf {
    repo_root().join("packaging")
}

fn pam_install_sh() -> PathBuf {
    packaging_dir().join("pam-install.sh")
}

fn pam_uninstall_sh() -> PathBuf {
    packaging_dir().join("pam-uninstall.sh")
}

const ARCH_GDM_PASSWORD: &str = "\
#%PAM-1.0
auth     include  system-auth
auth     optional  pam_gnome_keyring.so
account  include  system-auth
password include  system-auth
session  include  system-auth
";

/// Build a fake DESTDIR root with an Arch-style PAM file, a stub
/// `pam_namtor.so`, and fake enrollment templates.
struct FakeRoot {
    _tmp: TempDir,
    root: PathBuf,
    pam_file: PathBuf,
    templates_dir: PathBuf,
}

impl FakeRoot {
    fn build() -> Self {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let root = tmp.path().to_path_buf();

        let pam_d = root.join("etc/pam.d");
        fs::create_dir_all(&pam_d).expect("mkdir pam.d");
        let pam_file = pam_d.join("gdm-password");
        fs::write(&pam_file, ARCH_GDM_PASSWORD).expect("write gdm-password");

        let security_dir = root.join("usr/lib/security");
        fs::create_dir_all(&security_dir).expect("mkdir security");
        fs::write(security_dir.join("pam_namtor.so"), b"").expect("write .so stub");

        let templates_dir = root.join("var/lib/namtor/templates");
        fs::create_dir_all(&templates_dir).expect("mkdir templates");
        // Two fake encrypted template containers (per-uid).
        fs::write(
            templates_dir.join("1000.bin"),
            b"ENCRYPTED_TEMPLATE_UID_1000",
        )
        .expect("write t1");
        fs::write(
            templates_dir.join("1001.bin"),
            b"ENCRYPTED_TEMPLATE_UID_1001",
        )
        .expect("write t2");

        Self {
            _tmp: tmp,
            root,
            pam_file,
            templates_dir,
        }
    }

    fn run_install(&self, extra_args: &[&str]) -> std::io::Result<std::process::Output> {
        let mut cmd = Command::new("bash");
        cmd.env("DESTDIR", &self.root)
            .arg(pam_install_sh())
            .arg("--no-preflight")
            .arg("--target")
            .arg("/etc/pam.d/gdm-password")
            .args(extra_args);
        cmd.output()
    }

    fn run_uninstall(&self, extra_args: &[&str]) -> std::io::Result<std::process::Output> {
        let mut cmd = Command::new("bash");
        cmd.env("DESTDIR", &self.root)
            .arg(pam_uninstall_sh())
            .arg("--target")
            .arg("/etc/pam.d/gdm-password")
            .args(extra_args);
        cmd.output()
    }

    fn install_ok(&self, extra_args: &[&str]) {
        let out = self.run_install(extra_args).expect("run install");
        assert!(
            out.status.success(),
            "install failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn uninstall_ok(&self, extra_args: &[&str]) {
        let out = self.run_uninstall(extra_args).expect("run uninstall");
        assert!(
            out.status.success(),
            "uninstall failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn pam_content(&self) -> String {
        fs::read_to_string(&self.pam_file).expect("read pam file")
    }

    fn template_names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.templates_dir)
            .expect("read templates")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn template_content(&self, name: &str) -> Vec<u8> {
        fs::read(self.templates_dir.join(name)).expect("read template")
    }
}

fn assert_block_present(pam: &str) {
    assert!(
        pam.contains("# namtor-begin"),
        "namtor-begin marker missing"
    );
    assert!(
        pam.contains("auth sufficient pam_namtor.so"),
        "namtor auth line missing"
    );
    assert!(pam.contains("# namtor-end"), "namtor-end marker missing");
}

fn assert_block_absent(pam: &str) {
    assert!(
        !pam.contains("namtor"),
        "stray namtor reference after uninstall"
    );
}

fn leaked_temp_files(dir: &std::path::Path) -> Vec<String> {
    fs::read_dir(dir)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".namtor.pam."))
        .collect()
}

// --- helpers ---

fn vec_into(originals: &[(String, Vec<u8>)]) -> Vec<String> {
    let mut names: Vec<String> = originals.iter().map(|(n, _)| n.clone()).collect();
    names.sort();
    names
}

fn assert_templates_unchanged(root: &FakeRoot, originals: &[(String, Vec<u8>)]) {
    for (name, content) in originals {
        assert_eq!(
            root.template_content(name),
            *content,
            "template {name} content changed"
        );
    }
}

/// Given: a fake root with templates and an Arch-style PAM file.
/// When:  pam-install.sh runs.
/// Then:  the namtor block is inserted before the first auth line.
#[test]
fn install_adds_namtor_block() {
    let root = FakeRoot::build();
    root.install_ok(&["--keep-backup"]);

    let pam = root.pam_content();
    assert_block_present(&pam);

    let begin = pam.find("# namtor-begin").expect("begin marker position");
    let first_auth = pam
        .find("auth     include  system-auth")
        .expect("system-auth line position");
    assert!(
        begin < first_auth,
        "namtor block must precede the first stock auth line"
    );
}

/// Given: a fake root with the namtor block installed.
/// When:  pam-uninstall.sh runs.
/// Then:  the block is removed and the PAM file matches the original.
#[test]
fn uninstall_removes_namtor_block() {
    let root = FakeRoot::build();
    let original = root.pam_content();

    root.install_ok(&["--keep-backup"]);
    assert_block_present(&root.pam_content());

    root.uninstall_ok(&[]);

    let after = root.pam_content();
    assert_block_absent(&after);
    assert_eq!(
        after, original,
        "PAM file must match the pre-install original after uninstall"
    );
}

/// Given: a fake root with enrolled templates.
/// When:  install → upgrade (re-install) → uninstall runs.
/// Then:  templates are byte-identical throughout — the PAM lifecycle
///        scripts never touch `/var/lib/namtor/templates/`.
#[test]
fn upgrade_preserves_templates() {
    let root = FakeRoot::build();

    let originals: Vec<(String, Vec<u8>)> = root
        .template_names()
        .into_iter()
        .map(|n| {
            let content = root.template_content(&n);
            (n, content)
        })
        .collect();
    assert_eq!(originals.len(), 2, "expected two template files");

    // Install.
    root.install_ok(&["--keep-backup"]);
    assert_eq!(
        root.template_names(),
        vec_into(&originals),
        "template set changed after install"
    );
    assert_templates_unchanged(&root, &originals);

    // Upgrade (re-install).
    root.install_ok(&["--keep-backup"]);
    assert_eq!(
        root.template_names(),
        vec_into(&originals),
        "template set changed after upgrade"
    );
    assert_templates_unchanged(&root, &originals);

    // Uninstall.
    root.uninstall_ok(&[]);
    assert_eq!(
        root.template_names(),
        vec_into(&originals),
        "template set changed after uninstall"
    );
    assert_templates_unchanged(&root, &originals);
}

/// Given: an install that completed successfully.
/// When:  we scan the target PAM directory.
/// Then:  no `.namtor.pam.*` temp files leak — the atomic rename consumed
///        the temp file.
#[test]
fn install_leaves_no_temp_files() {
    let root = FakeRoot::build();
    root.install_ok(&["--keep-backup"]);

    let pam_dir = root.pam_file.parent().expect("pam.d dir");
    assert!(
        leaked_temp_files(pam_dir).is_empty(),
        "temp files leaked in target directory"
    );
}

/// Given: an install that was interrupted (simulated by a denied control
/// field that fails before the rename).
/// When:  the rollback trap fires.
/// Then:  no temp files leak in the target directory — the trap cleans up.
#[test]
fn failed_install_leaves_no_temp_files() {
    let root = FakeRoot::build();

    // `--control required` is rejected by check_control() before the temp
    // is created, so it can't test the temp leak path. Instead, we test the
    // rollback path: install into a deny-first file fails post-insertion
    // verification, the trap fires, and the temp should be cleaned up.
    let deny_first = "\
#%PAM-1.0
auth     required   pam_deny.so
auth     sufficient pam_unix.so nullok try_first_pass
account  include    system-auth
";
    fs::write(&root.pam_file, deny_first).expect("write deny-first");

    let out = root.run_install(&["--keep-backup"]).expect("run install");
    assert!(!out.status.success(), "install into deny-first should fail");

    let pam_dir = root.pam_file.parent().expect("pam.d dir");
    assert!(
        leaked_temp_files(pam_dir).is_empty(),
        "temp files leaked after failed install + rollback"
    );
}
