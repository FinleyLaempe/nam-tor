// SPDX-License-Identifier: GPL-3.0-only
//! Fault tests for the hardened template store (task 12).
//!
//! Each test names one fault class from the acceptance list:
//! disk-full, partial write, symlink, owner, transplant, concurrency, crash.
//! Run serially: `cargo test -p namtor-store --test store_faults -- --test-threads=1`.

#![cfg(unix)]

use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;

use namtor_store::crypto::BackendSelector;
use namtor_store::template::{EnrollmentMeta, Template, TemplateRecord};
use namtor_store::{StoreError, TemplateStore};

fn test_record(uid: u32) -> TemplateRecord {
    TemplateRecord {
        meta: EnrollmentMeta {
            uid,
            username: "test".into(),
            model_id: "sface".into(),
            model_quant: namtor_proto::worker::Quant::Fp32,
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

fn open_store(dir: &Path) -> TemplateStore {
    let key_path = dir.join("key");
    let mut selector = BackendSelector::file().with_root_secret_path(dir.join("root_secret"));
    TemplateStore::open_with_selector(dir, &key_path, &mut selector).unwrap()
}

fn current_euid() -> u32 {
    unsafe { libc::geteuid() }
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

// --- disk-full / write failure: old record intact ---

#[test]
fn write_failure_preserves_old_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());

    let a = test_record(1000);
    store.store(&a).unwrap();
    let a_raw = std::fs::read(dir.path().join("1000.sealed")).unwrap();

    // Make the dir read-only so temp-file creation fails (stands in for
    // disk-full / quota-exhausted).
    set_mode(dir.path(), 0o500);
    let mut b = test_record(1000);
    b.templates[0].quality = 0.11;
    let err = store.update(&b).unwrap_err();
    assert!(matches!(err, StoreError::Io(_)), "got {err:?}");

    set_mode(dir.path(), 0o755);
    let after = std::fs::read(dir.path().join("1000.sealed")).unwrap();
    assert_eq!(
        after, a_raw,
        "old record must be byte-identical after failed write"
    );
    let loaded = store.load(1000).unwrap();
    assert_eq!(loaded.templates[0].quality, 0.9);
}

// --- partial write: detected, not silently accepted ---

#[test]
fn partial_write_detected() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    store.store(&test_record(1000)).unwrap();

    let path = dir.path().join("1000.sealed");
    let raw = std::fs::read(&path).unwrap();
    // Simulate a torn write (non-atomic) by truncating the file.
    std::fs::write(&path, &raw[..raw.len() / 2]).unwrap();

    let err = store.load(1000).unwrap_err();
    assert!(
        matches!(err, StoreError::Crypto(_) | StoreError::Serde(_)),
        "got {err:?}"
    );
}

// --- symlink: rejected on write and read ---

#[test]
fn symlink_record_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());

    let target = dir.path().join("evil.txt");
    std::fs::write(&target, b"outside").unwrap();
    let link = dir.path().join("1000.sealed");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let err = store.store(&test_record(1000)).unwrap_err();
    assert!(matches!(err, StoreError::Symlink(_)), "got {err:?}");

    let err = store.load(1000).unwrap_err();
    assert!(matches!(err, StoreError::Symlink(_)), "got {err:?}");
}

// --- owner: wrong owner rejected on read; correct owner accepted ---

#[test]
fn wrong_owner_rejected_on_read() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key");
    let wrong_uid = current_euid().wrapping_add(9999);
    let mut selector =
        BackendSelector::file().with_root_secret_path(dir.path().join("root_secret"));
    let store = TemplateStore::open_with_owner_and_selector(
        dir.path(),
        &key_path,
        Some(wrong_uid),
        &mut selector,
    )
    .unwrap();
    store.store(&test_record(1000)).unwrap();

    // Not running as root -> chown skipped -> file owned by current user,
    // which does NOT match `wrong_uid` -> read must refuse.
    let err = store.load(1000).unwrap_err();
    assert!(
        matches!(err, StoreError::WrongOwner { expected, actual, .. }
            if expected == wrong_uid && actual == current_euid()),
        "got {err:?}"
    );
}

#[test]
fn correct_owner_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key");
    let mut selector =
        BackendSelector::file().with_root_secret_path(dir.path().join("root_secret"));
    let store = TemplateStore::open_with_owner_and_selector(
        dir.path(),
        &key_path,
        Some(current_euid()),
        &mut selector,
    )
    .unwrap();
    store.store(&test_record(1000)).unwrap();
    let loaded = store.load(1000).unwrap();
    assert_eq!(loaded.meta.uid, 1000);
}

// --- transplant: template bound to the sealing key ---

#[test]
fn transplant_to_different_key_rejected() {
    let dir_a = tempfile::tempdir().unwrap();
    let key_path_a = dir_a.path().join("key");
    let seed_a = [0xA1u8; 32];
    let mut sel_a = BackendSelector::mock(seed_a);
    let store_a = TemplateStore::open_with_selector(dir_a.path(), &key_path_a, &mut sel_a).unwrap();
    store_a.store(&test_record(1000)).unwrap();

    // Copy the sealed template into a store backed by a DIFFERENT key.
    let dir_b = tempfile::tempdir().unwrap();
    let key_path_b = dir_b.path().join("key");
    let seed_b = [0xB2u8; 32];
    let mut sel_b = BackendSelector::mock(seed_b);
    let store_b = TemplateStore::open_with_selector(dir_b.path(), &key_path_b, &mut sel_b).unwrap();
    std::fs::copy(
        dir_a.path().join("1000.sealed"),
        dir_b.path().join("1000.sealed"),
    )
    .unwrap();

    let err = store_b.load(1000).unwrap_err();
    assert!(
        matches!(err, StoreError::Crypto(_)),
        "transplant must be rejected, got {err:?}"
    );
}

// --- concurrency: per-uid flock serialises concurrent writers ---

#[test]
fn concurrent_writers_serialise_without_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    // Seed with an initial record so both updates hit the same uid.
    store.store(&test_record(1000)).unwrap();

    let store = Arc::new(store);
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for q in [0.11_f32, 0.22_f32] {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let mut rec = test_record(1000);
            rec.templates[0].quality = q;
            barrier.wait();
            store.update(&rec).unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let loaded = store.load(1000).unwrap();
    let q = loaded.templates[0].quality;
    assert!(
        (q - 0.11).abs() < f32::EPSILON || (q - 0.22).abs() < f32::EPSILON,
        "final quality must be one of the two writers, got {q}"
    );

    // The lock file must exist after a write.
    assert!(dir.path().join("1000.lock").exists());
}

// --- crash mid-write: stale temp file does not corrupt the committed record ---

#[test]
fn crash_mid_write_leaves_old_record_intact() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());

    let a = test_record(1000);
    store.store(&a).unwrap();
    let a_raw = std::fs::read(dir.path().join("1000.sealed")).unwrap();

    // Simulate a crash DURING a write of B: drop a partial temp file in the
    // dir, but never rename. The committed record A must be untouched.
    let stale_tmp = dir.path().join("1000.sealed.tmp.crashleftover");
    std::fs::write(&stale_tmp, b"partial garbage from a crashed write").unwrap();

    let loaded = store.load(1000).unwrap();
    assert_eq!(loaded.templates[0].quality, 0.9);
    let still = std::fs::read(dir.path().join("1000.sealed")).unwrap();
    assert_eq!(still, a_raw, "committed record unchanged by crash leftover");

    // A subsequent update must still succeed and overwrite A cleanly.
    let mut b = test_record(1000);
    b.templates[0].quality = 0.77;
    store.update(&b).unwrap();
    let loaded = store.load(1000).unwrap();
    assert_eq!(loaded.templates[0].quality, 0.77);
}

// --- backward compat: NMT1 files (pre-task-12) are still readable ---

#[test]
fn reads_legacy_nmt1_file() {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce};
    use namtor_store::crypto::BackendSelector;

    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key");
    let mut sel = BackendSelector::file().with_root_secret_path(dir.path().join("root_secret"));
    let store = TemplateStore::open_with_selector(dir.path(), &key_path, &mut sel).unwrap();

    // Hand-craft an NMT1 file using the v1 AAD layout that the pre-task-12
    // writer produced. We re-derive the file key by re-loading it.
    let sealed_key =
        namtor_store::crypto::SealedKey::load_or_create_with(&key_path, &mut sel).unwrap();
    let cipher = Aes256Gcm::new_from_slice(sealed_key.key()).unwrap();
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let record = test_record(1000);
    let json = serde_json::to_vec(&record).unwrap();
    let mut aad = Vec::new();
    aad.extend_from_slice(b"namtor-v1:");
    aad.extend_from_slice(&1000u32.to_le_bytes());
    aad.push(b':');
    aad.extend_from_slice(b"sface");
    let payload = Payload {
        msg: &json,
        aad: &aad,
    };
    let ct = cipher.encrypt(nonce, payload).unwrap();

    let mut file = Vec::new();
    file.extend_from_slice(b"NMT1");
    file.extend_from_slice(&nonce_bytes);
    file.extend_from_slice(&(b"sface".len() as u32).to_le_bytes());
    file.extend_from_slice(b"sface");
    file.extend_from_slice(&ct);
    std::fs::write(dir.path().join("1000.sealed"), &file).unwrap();

    let loaded = store.load(1000).unwrap();
    assert_eq!(loaded.meta.uid, 1000);
    assert_eq!(loaded.templates[0].vector, vec![0.5; 128]);
}

use rand::RngCore;
