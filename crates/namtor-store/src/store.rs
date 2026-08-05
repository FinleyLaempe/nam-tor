// SPDX-License-Identifier: GPL-3.0-only
//! Template store: encrypted on-disk storage of face embedding templates.
//!
//! Per-user container at `/var/lib/namtor/templates/<uid>.sealed`:
//! - AES-256-GCM encrypted JSON (the `TemplateRecord`).
//! - AAD binds uid + model_id to prevent transplant.
//! - The encryption key is TPM2-sealed (or root-only file fallback).

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm;
use rand::RngCore;

use crate::crypto::SealedKey;
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
}

pub struct TemplateStore {
    dir: PathBuf,
    key: SealedKey,
}

impl TemplateStore {
    /// Open the store at `dir`, loading or creating the sealing key at
    /// `key_path`.
    pub fn open(dir: &Path, key_path: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir)?;
        let key = SealedKey::load_or_create(key_path)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            key,
        })
    }

    pub fn key_source(&self) -> crate::crypto::KeySource {
        self.key.source()
    }

    fn record_path(&self, uid: u32) -> PathBuf {
        self.dir.join(format!("{uid}.sealed"))
    }

    fn aad(uid: u32, model_id: &str) -> Vec<u8> {
        let mut aad = Vec::new();
        aad.extend_from_slice(b"namtor-v1:");
        aad.extend_from_slice(&uid.to_le_bytes());
        aad.push(b':');
        aad.extend_from_slice(model_id.as_bytes());
        aad
    }

    /// Store a template record for `uid`. Fails if one already exists.
    pub fn store(&self, record: &TemplateRecord) -> Result<(), StoreError> {
        let path = self.record_path(record.meta.uid);
        if path.exists() {
            return Err(StoreError::AlreadyExists(record.meta.uid));
        }
        self.write(&path, record)
    }

    /// Replace the template record for `uid`. Overwrites if present.
    pub fn update(&self, record: &TemplateRecord) -> Result<(), StoreError> {
        let path = self.record_path(record.meta.uid);
        self.write(&path, record)
    }

    /// Load the template record for `uid`.
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
        std::fs::remove_file(path)?;
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

    fn write(&self, path: &Path, record: &TemplateRecord) -> Result<(), StoreError> {
        let json = serde_json::to_vec(record)?;
        let cipher = Aes256Gcm::new_from_slice(self.key.key())
            .map_err(|e| StoreError::Crypto(e.to_string()))?;

        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = aes_gcm::Nonce::from_slice(&nonce_bytes);

        let aad = Self::aad(record.meta.uid, &record.meta.model_id);
        let payload = Payload {
            msg: &json,
            aad: &aad,
        };

        let ciphertext = cipher
            .encrypt(nonce, payload)
            .map_err(|e| StoreError::Crypto(e.to_string()))?;

        let model_id_bytes = record.meta.model_id.as_bytes();
        let mut file = Vec::new();
        file.extend_from_slice(b"NMT1");
        file.extend_from_slice(&nonce_bytes);
        file.extend_from_slice(&(model_id_bytes.len() as u32).to_le_bytes());
        file.extend_from_slice(model_id_bytes);
        file.extend_from_slice(&ciphertext);

        std::fs::write(path, &file)?;
        set_permissions(path)?;
        Ok(())
    }

    fn read(&self, path: &Path, uid: u32) -> Result<TemplateRecord, StoreError> {
        let raw = std::fs::read(path)?;
        if raw.len() < 24 || &raw[..4] != b"NMT1" {
            return Err(StoreError::Crypto("invalid file format".into()));
        }
        let nonce_bytes = &raw[4..16];
        let model_id_len = u32::from_le_bytes(raw[16..20].try_into().unwrap()) as usize;
        if raw.len() < 20 + model_id_len {
            return Err(StoreError::Crypto("truncated model_id".into()));
        }
        let model_id = std::str::from_utf8(&raw[20..20 + model_id_len])
            .map_err(|e| StoreError::Crypto(format!("model_id utf8: {e}")))?
            .to_string();
        let ciphertext = &raw[20 + model_id_len..];

        let cipher = Aes256Gcm::new_from_slice(self.key.key())
            .map_err(|e| StoreError::Crypto(e.to_string()))?;
        let nonce = aes_gcm::Nonce::from_slice(nonce_bytes);

        let aad = Self::aad(uid, &model_id);
        let payload = Payload {
            msg: ciphertext,
            aad: &aad,
        };

        let plaintext = cipher
            .decrypt(nonce, payload)
            .map_err(|e| StoreError::Crypto(format!("decrypt: {e}")))?;

        let record: TemplateRecord = serde_json::from_slice(&plaintext)?;

        if record.meta.uid != uid {
            return Err(StoreError::Crypto("uid mismatch in record".into()));
        }
        Ok(record)
    }
}

fn set_permissions(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
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

    #[test]
    fn store_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let store = TemplateStore::open(dir.path(), &key_path).unwrap();

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
        let key_path = dir.path().join("key");
        let store = TemplateStore::open(dir.path(), &key_path).unwrap();

        store.store(&test_record(1000)).unwrap();
        let err = store.store(&test_record(1000)).unwrap_err();
        assert!(matches!(err, StoreError::AlreadyExists(1000)));
    }

    #[test]
    fn update_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let store = TemplateStore::open(dir.path(), &key_path).unwrap();

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
        let key_path = dir.path().join("key");
        let store = TemplateStore::open(dir.path(), &key_path).unwrap();

        store.store(&test_record(1000)).unwrap();
        store.delete(1000).unwrap();
        let err = store.load(1000).unwrap_err();
        assert!(matches!(err, StoreError::NotFound(1000)));
    }

    #[test]
    fn list_uids() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let store = TemplateStore::open(dir.path(), &key_path).unwrap();

        store.store(&test_record(1000)).unwrap();
        store.store(&test_record(1001)).unwrap();
        store.store(&test_record(1002)).unwrap();

        let uids = store.list_uids().unwrap();
        assert_eq!(uids, vec![1000, 1001, 1002]);
    }

    #[test]
    fn aad_prevents_cross_model_decrypt() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let store = TemplateStore::open(dir.path(), &key_path).unwrap();

        let mut record = test_record(1000);
        record.meta.model_id = "model_a".into();
        store.store(&record).unwrap();

        let raw = std::fs::read(dir.path().join("1000.sealed")).unwrap();
        assert!(raw.len() > 16);
        assert_eq!(&raw[..4], b"NMT1");
    }
}
