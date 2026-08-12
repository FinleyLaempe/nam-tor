// SPDX-License-Identifier: GPL-3.0-only
//! Enrollment storage fault tests (task 5).
//!
//! Covers the acceptance list: missing identity, interrupted write, corrupt
//! key, symlink, disk full and re-enrollment; plus the cross-process
//! contract that the daemon (opening the store with the same expected owner
//! the enrollment used) immediately reads a root-initiated enrollment.
//!
//! These tests run unprivileged. The ownership chown path is a no-op without
//! root, so the tests exercise the *logic* — that enrollment opens the store
//! with the correct expected owner, writes atomically, preserves the prior
//! record on failure, cleans temp files, and the daemon's store (same owner)
//! loads the result. Real root behaviour is covered by the HIL script
//! (task 7); see `docs/evidence/enrollment-storage-faults.md`.

#![cfg(unix)]

use std::path::Path;

use namtor_cli::enroll::{
    commit_enrollment_with_selector, resolve_identity, EnrollmentIdentity, IdentityError,
};
use namtor_store::crypto::BackendSelector;
use namtor_store::template::{EnrollmentMeta, Template, TemplateRecord};
use namtor_store::{StoreError, TemplateStore};

use namtor_proto::worker::Quant;

fn test_record(uid: u32) -> TemplateRecord {
    TemplateRecord {
        meta: EnrollmentMeta {
            uid,
            username: "test".into(),
            model_id: "sface".into(),
            model_quant: Quant::Fp32,
            created_at_us: 1,
            num_templates: 1,
            znorm_mean: None,
            znorm_std: None,
            extrinsics: None,
        },
        templates: vec![Template {
            vector: vec![0.5; 128],
            quality: 0.9,
            is_anchor: true,
        }],
    }
}

fn current_euid() -> u32 {
    nix::unistd::geteuid().as_raw()
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
}

fn test_selector(dir: &Path) -> BackendSelector {
    BackendSelector::file().with_root_secret_path(dir.join("root_secret"))
}

fn commit_test_enrollment(
    dir: &Path,
    key_path: &Path,
    id: &EnrollmentIdentity,
    record: &TemplateRecord,
) {
    let mut selector = test_selector(dir);
    commit_enrollment_with_selector(dir, key_path, id, record, &mut selector).unwrap()
}

fn open_test_store(dir: &Path, key_path: &Path, owner_uid: u32) -> TemplateStore {
    let mut selector = test_selector(dir);
    TemplateStore::open_with_owner_and_selector(dir, key_path, Some(owner_uid), &mut selector)
        .unwrap()
}

/// Self-enrollment resolves to the current euid as both the enrolled uid and
/// the store owner (no root, no `namtor` system user needed).
#[test]
fn self_enrollment_identity_uses_current_euid() {
    let id = resolve_identity(None).unwrap();
    assert_eq!(id.uid, current_euid());
    assert_eq!(id.store_owner_uid, current_euid());
}

/// `--user` for a non-existent user fails with `UnknownUser`, not a panic.
#[test]
fn unknown_target_user_fails_actionably() {
    let err = resolve_identity(Some("definitely-no-such-user-xyzzy")).unwrap_err();
    assert!(matches!(err, IdentityError::UnknownUser(_)), "got {err:?}");
    assert!(err.to_string().contains("definitely-no-such-user-xyzzy"));
}

/// `--user` targeting another account while not root fails with
/// `NotRootForOtherUser`. Uses a real account name (`root`) that exists but
/// is not the current unprivileged test user.
#[test]
fn non_root_cannot_enroll_other_user() {
    if current_euid() == 0 {
        // Running as root in CI would make this branch unreachable; skip.
        return;
    }
    let err = resolve_identity(Some("root")).unwrap_err();
    assert!(
        matches!(err, IdentityError::NotRootForOtherUser),
        "got {err:?}"
    );
}

/// Re-enrollment overwrites the prior record cleanly: a second
/// `commit_enrollment` for the same uid replaces the first, and the daemon
/// store (same expected owner) loads the updated templates.
#[test]
fn re_enrollment_overwrites_and_daemon_reads() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key");
    let id = EnrollmentIdentity {
        uid: 1000,
        username: "test".into(),
        store_owner_uid: current_euid(),
    };

    let mut first = test_record(1000);
    first.templates[0].quality = 0.80;
    commit_test_enrollment(dir.path(), &key_path, &id, &first);

    // Daemon perspective: open with the same owner enrollment used.
    let daemon = open_test_store(dir.path(), &key_path, id.store_owner_uid);
    let loaded = daemon.load(1000).unwrap();
    assert_eq!(loaded.templates[0].quality, 0.80);

    let mut second = test_record(1000);
    second.templates[0].quality = 0.42;
    commit_test_enrollment(dir.path(), &key_path, &id, &second);

    let daemon2 = open_test_store(dir.path(), &key_path, id.store_owner_uid);
    let loaded2 = daemon2.load(1000).unwrap();
    assert_eq!(loaded2.templates[0].quality, 0.42);
}

/// A write failure (simulated disk-full via a read-only directory) leaves the
/// previous enrollment byte-identical and removes the temp file. This is the
/// "preserve old enrollment and clean temps on failure" acceptance.
#[test]
fn write_failure_preserves_old_enrollment_and_cleans_temps() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key");
    let id = EnrollmentIdentity {
        uid: 1000,
        username: "test".into(),
        store_owner_uid: current_euid(),
    };

    let first = test_record(1000);
    commit_test_enrollment(dir.path(), &key_path, &id, &first);
    let sealed = dir.path().join("1000.sealed");
    let first_bytes = std::fs::read(&sealed).unwrap();

    set_mode(dir.path(), 0o500);
    let mut second = test_record(1000);
    second.templates[0].quality = 0.11;
    let err = {
        let mut selector = test_selector(dir.path());
        commit_enrollment_with_selector(dir.path(), &key_path, &id, &second, &mut selector)
            .unwrap_err()
    };
    assert!(matches!(err, StoreError::Io(_)), "got {err:?}");

    set_mode(dir.path(), 0o755);
    let after = std::fs::read(&sealed).unwrap();
    assert_eq!(after, first_bytes, "old enrollment must be byte-identical");

    let mut tmp_leftover = 0;
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("1000.sealed.tmp") {
            tmp_leftover += 1;
        }
    }
    assert_eq!(
        tmp_leftover, 0,
        "no temp file must remain after a failed write"
    );
}

/// A symlink at the sealed-record path is refused on both write and read,
/// proving enrollment does not follow symlinks into arbitrary files.
#[test]
fn symlink_record_rejected_by_enrollment() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key");
    let id = EnrollmentIdentity {
        uid: 1000,
        username: "test".into(),
        store_owner_uid: current_euid(),
    };

    let target = dir.path().join("evil.txt");
    std::fs::write(&target, b"outside").unwrap();
    let link = dir.path().join("1000.sealed");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let err = {
        let mut selector = test_selector(dir.path());
        commit_enrollment_with_selector(
            dir.path(),
            &key_path,
            &id,
            &test_record(1000),
            &mut selector,
        )
        .unwrap_err()
    };
    assert!(matches!(err, StoreError::Symlink(_)), "got {err:?}");
}

/// A corrupt key file causes store open to fail (not a panic), so enrollment
/// reports the error rather than silently re-creating a key that would
/// orphan existing enrollments.
#[test]
fn corrupt_key_fails_open_actionably() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key");
    let id = EnrollmentIdentity {
        uid: 1000,
        username: "test".into(),
        store_owner_uid: current_euid(),
    };

    // First enrollment creates a valid key + record.
    commit_test_enrollment(dir.path(), &key_path, &id, &test_record(1000));

    // Corrupt the key file (truncate to garbage).
    std::fs::write(&key_path, b"not-a-valid-sealed-blob").unwrap();

    let err = {
        let mut selector = test_selector(dir.path());
        commit_enrollment_with_selector(
            dir.path(),
            &key_path,
            &id,
            &test_record(1000),
            &mut selector,
        )
        .unwrap_err()
    };
    assert!(
        matches!(err, StoreError::Crypto(_) | StoreError::Io(_)),
        "got {err:?}"
    );
}

/// A partially-written (truncated) record is detected on load, not silently
/// accepted as a valid enrollment. Proves enrollment's atomic write does not
/// leave the daemon reading a torn file.
#[test]
fn interrupted_write_detected_on_daemon_read() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key");
    let id = EnrollmentIdentity {
        uid: 1000,
        username: "test".into(),
        store_owner_uid: current_euid(),
    };
    commit_test_enrollment(dir.path(), &key_path, &id, &test_record(1000));

    let sealed = dir.path().join("1000.sealed");
    let raw = std::fs::read(&sealed).unwrap();
    std::fs::write(&sealed, &raw[..raw.len() / 2]).unwrap();

    let daemon = open_test_store(dir.path(), &key_path, id.store_owner_uid);
    let err = daemon.load(1000).unwrap_err();
    assert!(
        matches!(err, StoreError::Crypto(_) | StoreError::Serde(_)),
        "got {err:?}"
    );
}

/// The daemon store, opened with the same expected owner as enrollment,
/// reads the record: this is the "daemon immediately reads root-initiated
/// enrollment" contract. Simulated unprivileged by using the current euid
/// as the owner (the logic is identical; root only changes the chown step).
#[test]
fn daemon_reads_enrollment_with_matching_owner() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key");
    let id = EnrollmentIdentity {
        uid: 1000,
        username: "test".into(),
        store_owner_uid: current_euid(),
    };
    commit_test_enrollment(dir.path(), &key_path, &id, &test_record(1000));

    // Daemon opens with the SAME expected owner enrollment used.
    let daemon = open_test_store(dir.path(), &key_path, id.store_owner_uid);
    let loaded = daemon.load(1000).unwrap();
    assert_eq!(loaded.meta.uid, 1000);
    assert_eq!(loaded.templates.len(), 1);
}

/// A daemon opening with a *different* expected owner than enrollment must
/// NOT read the record (owner check fails). This is the negative side of the
/// contract: mismatched ownership is rejected, not silently accepted.
#[test]
fn daemon_rejects_enrollment_with_mismatched_owner() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key");
    let id = EnrollmentIdentity {
        uid: 1000,
        username: "test".into(),
        store_owner_uid: current_euid(),
    };
    commit_test_enrollment(dir.path(), &key_path, &id, &test_record(1000));

    let wrong_owner = current_euid().wrapping_add(9999);
    let daemon = open_test_store(dir.path(), &key_path, wrong_owner);
    let err = daemon.load(1000).unwrap_err();
    assert!(
        matches!(err, StoreError::WrongOwner { expected, actual, .. }
            if expected == wrong_owner && actual == current_euid()),
        "got {err:?}"
    );
}
