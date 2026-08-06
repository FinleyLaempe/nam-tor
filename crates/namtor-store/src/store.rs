// SPDX-License-Identifier: GPL-3.0-only
//! Template store: encrypted on-disk storage of face embedding templates.
//!
//! Per-user container at `/var/lib/namtor/templates/<uid>.sealed`:
//! - AES-256-GCM encrypted JSON (the `TemplateRecord`).
//! - AAD binds uid + profile_id + model_id to prevent transplant.
//! - The encryption key is TPM2-sealed (or root-only file fallback).
//!
//! Hardening (task 12):
//! - Atomic writes: encrypt to a unique temp file, `fsync`, atomic `rename`,
//!   `fsync` the directory. A crash mid-write leaves the previous record
//!   intact.
//! - Schema versioning: `NMT2` files carry a `version` field for future
//!   migrations. `NMT1` files (no version, no profile_id) are still readable.
//! - Symlink rejection, owner check, per-uid `flock`, daemon `chown`:
//!   see [`crate::fs_util`].
//! - Sensitive material is `mlock`ed and `MADV_DONTDUMP`ed; see
//!   [`crate::crypto`] and [`crate::template`].

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm;
use rand::RngCore;

use crate::crypto::SealedKey;
use crate::format::{aad_v2, decode, encode_v2, Decoded};
use crate::fs_util::{atomic_write, check_owner, reject_symlink, FileLock};
use crate::template::TemplateRecord;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error("TPM2 error: {0}")]
    Tpm2(String),
    #[error("serialisation error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("template not found for uid {0}")]
    NotFound(u32),
    #[error("template already exists for uid {0} (use update)")]
    AlreadyExists(u32),
    #[error("record path is a symlink (refused): {0}")]
    Symlink(PathBuf),
    #[error("record owner mismatch at {path}: expected {expected}, found {actual}")]
    WrongOwner {
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
    #[error("file lock error: {0}")]
    Lock(String),
    #[error("unsupported store schema version {0}")]
    SchemaVersion(u16),
}

pub struct TemplateStore {
    dir: PathBuf,
    key: SealedKey,
    expected_owner: Option<u32>,
}

impl TemplateStore {
    /// Open the store at `dir`, loading or creating the sealing key at
    /// `key_path`. The expected record owner is the current euid.
    pub fn open(dir: &Path, key_path: &Path) -> Result<Self, StoreError> {
        Self::open_with_owner(dir, key_path, None)
    }

    /// Open with an explicit expected record owner uid.
    ///
    /// `None` means "current euid". The daemon passes the `namtor` system
    /// user's uid; tests pass a specific uid to exercise the owner check.
    pub fn open_with_owner(
        dir: &Path,
        key_path: &Path,
        expected_owner: Option<u32>,
    ) -> Result<Self, StoreError> {
        Self::open_with_owner_and_selector(
            dir,
            key_path,
            expected_owner,
            &mut crate::crypto::BackendSelector::default(),
        )
    }

    /// Open with an explicit backend selector (e.g. a mock TPM seed) for
    /// transplant/rejection tests.
    pub fn open_with_selector(
        dir: &Path,
        key_path: &Path,
        selector: &mut crate::crypto::BackendSelector,
    ) -> Result<Self, StoreError> {
        Self::open_with_owner_and_selector(dir, key_path, None, selector)
    }

    /// Open with both an explicit owner and an explicit backend selector.
    pub fn open_with_owner_and_selector(
        dir: &Path,
        key_path: &Path,
        expected_owner: Option<u32>,
        selector: &mut crate::crypto::BackendSelector,
    ) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir)?;
        let key = SealedKey::load_or_create_with(key_path, selector)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            key,
            expected_owner,
        })
    }

    pub fn key_source(&self) -> crate::crypto::KeySource {
        self.key.source()
    }

    fn record_path(&self, uid: u32) -> PathBuf {
        self.dir.join(format!("{uid}.sealed"))
    }

    fn lock_path(&self, uid: u32) -> PathBuf {
        self.dir.join(format!("{uid}.lock"))
    }

    fn expected_owner_uid(&self) -> u32 {
        self.expected_owner.unwrap_or_else(|| {
            #[cfg(unix)]
            {
                unsafe { libc::geteuid() }
            }
            #[cfg(not(unix))]
            {
                0
            }
        })
    }

    /// Store a template record for `uid` under the default (empty) profile.
    /// Fails if one already exists.
    pub fn store(&self, record: &TemplateRecord) -> Result<(), StoreError> {
        self.store_with_profile(record, "")
    }

    /// Store a template record for `uid` under `profile_id`.
    /// Fails if one already exists.
    pub fn store_with_profile(
        &self,
        record: &TemplateRecord,
        profile_id: &str,
    ) -> Result<(), StoreError> {
        let path = self.record_path(record.meta.uid);
        reject_symlink(&path)?;
        if path.exists() {
            return Err(StoreError::AlreadyExists(record.meta.uid));
        }
        self.write(&path, record, profile_id)
    }

    /// Replace the template record for `uid` under the default profile.
    /// Overwrites if present.
    pub fn update(&self, record: &TemplateRecord) -> Result<(), StoreError> {
        self.update_with_profile(record, "")
    }

    /// Replace the template record for `uid` under `profile_id`.
    /// Overwrites if present.
    pub fn update_with_profile(
        &self,
        record: &TemplateRecord,
        profile_id: &str,
    ) -> Result<(), StoreError> {
        let path = self.record_path(record.meta.uid);
        self.write(&path, record, profile_id)
    }

    /// Load the template record for `uid` (any profile).
    pub fn load(&self, uid: u32) -> Result<TemplateRecord, StoreError> {
        let path = self.record_path(uid);
        if !path.exists() {
            return Err(StoreError::NotFound(uid));
        }
        self.read(&path, uid)
    }

    /// Delete the template record for `uid`.
    pub fn delete(&self, uid: u32) -> Result<(), StoreError> {
        let path = self.record_path(uid);
        if !path.exists() {
            return Err(StoreError::NotFound(uid));
        }
        reject_symlink(&path)?;
        let _guard = FileLock::acquire(&self.lock_path(uid))?;
        std::fs::remove_file(&path)?;
        Ok(())
    }

    /// List all enrolled uids.
    pub fn list_uids(&self) -> Result<Vec<u32>, StoreError> {
        let mut uids = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(stem) = name_str.strip_suffix(".sealed") {
                if let Ok(uid) = stem.parse::<u32>() {
                    uids.push(uid);
                }
            }
        }
        uids.sort();
        Ok(uids)
    }

    fn write(
        &self,
        path: &Path,
        record: &TemplateRecord,
        profile_id: &str,
    ) -> Result<(), StoreError> {
        reject_symlink(path)?;
        let _guard = FileLock::acquire(&self.lock_path(record.meta.uid))?;

        let json = serde_json::to_vec(record)?;
        let cipher = Aes256Gcm::new_from_slice(self.key.key())
            .map_err(|e| StoreError::Crypto(e.to_string()))?;

        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = aes_gcm::Nonce::from_slice(&nonce_bytes);

        let aad = aad_v2(record.meta.uid, profile_id, &record.meta.model_id);
        let payload = Payload {
            msg: &json,
            aad: &aad,
        };

        let ciphertext = cipher
            .encrypt(nonce, payload)
            .map_err(|e| StoreError::Crypto(e.to_string()))?;

        let file = encode_v2(&nonce_bytes, profile_id, &record.meta.model_id, &ciphertext);
        atomic_write(path, &file, self.expected_owner_uid())?;
        Ok(())
    }

    fn read(&self, path: &Path, uid: u32) -> Result<TemplateRecord, StoreError> {
        reject_symlink(path)?;
        check_owner(path, self.expected_owner_uid())?;
        let raw = std::fs::read(path)?;
        let frame: Decoded = decode(&raw, uid)?;
        let plaintext = self.decrypt(&frame.nonce, &frame.ciphertext, &frame.aad)?;

        let mut record: TemplateRecord = serde_json::from_slice(&plaintext)?;
        record.lock_memory();

        if record.meta.uid != uid {
            return Err(StoreError::Crypto("uid mismatch in record".into()));
        }
        Ok(record)
    }

    fn decrypt(
        &self,
        nonce_bytes: &[u8],
        ciphertext: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, StoreError> {
        if nonce_bytes.len() != 12 {
            return Err(StoreError::Crypto("nonce not 12 bytes".into()));
        }
        let cipher = Aes256Gcm::new_from_slice(self.key.key())
            .map_err(|e| StoreError::Crypto(e.to_string()))?;
        let nonce = aes_gcm::Nonce::from_slice(nonce_bytes);
        let payload = Payload {
            msg: ciphertext,
            aad,
        };
        cipher
            .decrypt(nonce, payload)
            .map_err(|e| StoreError::Crypto(format!("decrypt: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::{EnrollmentMeta, Template};

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
        TemplateStore::open(dir, &key_path).unwrap()
    }

    #[test]
    fn store_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        let record = test_record(1000);
        store.store(&record).unwrap();

        let loaded = store.load(1000).unwrap();
        assert_eq!(loaded.meta.uid, 1000);
        assert_eq!(loaded.templates.len(), 1);
        assert_eq!(loaded.templates[0].vector, vec![0.5; 128]);
    }

    #[test]
    fn store_fails_on_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store.store(&test_record(1000)).unwrap();
        let err = store.store(&test_record(1000)).unwrap_err();
        assert!(matches!(err, StoreError::AlreadyExists(1000)));
    }

    #[test]
    fn update_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store.store(&test_record(1000)).unwrap();
        let mut updated = test_record(1000);
        updated.templates[0].quality = 0.95;
        store.update(&updated).unwrap();

        let loaded = store.load(1000).unwrap();
        assert_eq!(loaded.templates[0].quality, 0.95);
    }

    #[test]
    fn delete_removes_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store.store(&test_record(1000)).unwrap();
        store.delete(1000).unwrap();
        let err = store.load(1000).unwrap_err();
        assert!(matches!(err, StoreError::NotFound(1000)));
    }

    #[test]
    fn list_uids() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store.store(&test_record(1000)).unwrap();
        store.store(&test_record(1001)).unwrap();
        store.store(&test_record(1002)).unwrap();

        let uids = store.list_uids().unwrap();
        assert_eq!(uids, vec![1000, 1001, 1002]);
    }

    #[test]
    fn new_files_use_nmt2_magic() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        let mut record = test_record(1000);
        record.meta.model_id = "model_a".into();
        store.store(&record).unwrap();

        let raw = std::fs::read(dir.path().join("1000.sealed")).unwrap();
        assert!(raw.len() > 16);
        assert_eq!(&raw[..4], b"NMT2");
    }

    #[test]
    fn profile_id_round_trip_and_aad_binding() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        let record = test_record(1000);
        store.store_with_profile(&record, "profile_work").unwrap();

        let loaded = store.load(1000).unwrap();
        assert_eq!(loaded.meta.uid, 1000);

        // Tampering the stored profile_id breaks AAD -> decrypt fails.
        let path = dir.path().join("1000.sealed");
        let mut raw = std::fs::read(&path).unwrap();
        // profile_id starts at offset 8 (magic 4 + version 2 + len 2).
        raw[8] ^= 0x10;
        std::fs::write(&path, &raw).unwrap();
        let err = store.load(1000).unwrap_err();
        assert!(matches!(err, StoreError::Crypto(_)), "got {err:?}");
    }
}
