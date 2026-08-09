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
use serde::{Deserialize, Serialize};

use crate::crypto::SealedKey;
use crate::format::{aad_v2, decode, encode_v2, Decoded};
use crate::fs_util::{atomic_write, check_owner, ensure_dir_owner, reject_symlink, FileLock};
use crate::template::{
    ProfileEntry, ProfileId, ProfileRecord, Template, TemplateRecord, UserContainer,
};

/// Tagged envelope for the per-uid container file.
///
/// `Profiles` is what current code writes. `Legacy` is kept as an explicit
/// tag for migration records; on-disk legacy files (pre-profile) have no
/// `container_kind` field and fall through to a bare `TemplateRecord` parse.
#[derive(Serialize, Deserialize)]
#[serde(tag = "container_kind", rename_all = "snake_case")]
enum ContainerEnvelope {
    Profiles(UserContainer),
    #[allow(dead_code)]
    Legacy(TemplateRecord),
}

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
    #[error("profile {0} not found")]
    ProfileNotFound(String),
    #[error("profile {0} already exists")]
    ProfileExists(String),
    #[error("profile uid {profile_uid} does not match container uid {container_uid}")]
    ProfileUidMismatch {
        profile_uid: u32,
        container_uid: u32,
    },
    #[error("refusing to downgrade TPM2-bound storage to software fallback: {0}")]
    DowngradeRefused(String),
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
        let owner = expected_owner.unwrap_or_else(|| {
            #[cfg(unix)]
            {
                unsafe { libc::geteuid() }
            }
            #[cfg(not(unix))]
            {
                0
            }
        });
        ensure_dir_owner(dir, owner)?;
        let key = SealedKey::load_or_create_with(key_path, selector)?;
        crate::fs_util::chown_if_root(key_path, owner)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            key,
            expected_owner,
        })
    }

    pub fn key_source(&self) -> crate::crypto::KeySource {
        self.key.source()
    }

    pub fn protection_state(&self) -> crate::crypto::ProtectionState {
        self.key.protection_state()
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
        let _guard = FileLock::acquire_with_owner(&self.lock_path(uid), self.expected_owner_uid())?;
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
        let _guard = FileLock::acquire_with_owner(
            &self.lock_path(record.meta.uid),
            self.expected_owner_uid(),
        )?;

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

    // ---- Named profile API (task 14) ----

    /// List every profile enrolled for `uid`. Legacy single-record files are
    /// migrated on read into a one-element container under the default profile.
    pub fn list_profiles(&self, uid: u32) -> Result<Vec<ProfileEntry>, StoreError> {
        let path = self.record_path(uid);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let container = self.read_container(&path, uid)?;
        Ok(container.profiles)
    }

    /// Append a new profile to `uid`'s container. Fails if the profile id
    /// already exists, or if `profile.uid != uid`.
    pub fn create_profile(
        &self,
        uid: u32,
        profile: ProfileRecord,
        templates: Vec<Template>,
    ) -> Result<(), StoreError> {
        if profile.uid != uid {
            return Err(StoreError::ProfileUidMismatch {
                profile_uid: profile.uid,
                container_uid: uid,
            });
        }
        let path = self.record_path(uid);
        let mut container = self.container_or_new(&path, uid)?;
        if container
            .profiles
            .iter()
            .any(|p| p.profile.profile_id == profile.profile_id)
        {
            return Err(StoreError::ProfileExists(profile.profile_id.0));
        }
        container.profiles.push(ProfileEntry { profile, templates });
        self.write_container(&path, uid, &container)
    }

    /// Rename an existing profile. Fails if the profile id is absent.
    pub fn rename_profile(
        &self,
        uid: u32,
        profile_id: &ProfileId,
        new_name: String,
    ) -> Result<(), StoreError> {
        let path = self.record_path(uid);
        let mut container = self.require_container(&path, uid)?;
        let entry = container
            .profiles
            .iter_mut()
            .find(|p| p.profile.profile_id == *profile_id)
            .ok_or_else(|| StoreError::ProfileNotFound(profile_id.0.clone()))?;
        entry.profile.display_name = new_name;
        self.write_container(&path, uid, &container)
    }

    /// Enable or disable a profile. Disabled profiles are excluded from
    /// [`Self::enabled_gallery`].
    pub fn enable_profile(
        &self,
        uid: u32,
        profile_id: &ProfileId,
        enabled: bool,
    ) -> Result<(), StoreError> {
        let path = self.record_path(uid);
        let mut container = self.require_container(&path, uid)?;
        let entry = container
            .profiles
            .iter_mut()
            .find(|p| p.profile.profile_id == *profile_id)
            .ok_or_else(|| StoreError::ProfileNotFound(profile_id.0.clone()))?;
        entry.profile.enabled = enabled;
        self.write_container(&path, uid, &container)
    }

    /// Remove a profile from `uid`'s container. Fails if the profile id is
    /// absent.
    pub fn delete_profile(&self, uid: u32, profile_id: &ProfileId) -> Result<(), StoreError> {
        let path = self.record_path(uid);
        let mut container = self.require_container(&path, uid)?;
        let before = container.profiles.len();
        container
            .profiles
            .retain(|p| p.profile.profile_id != *profile_id);
        if container.profiles.len() == before {
            return Err(StoreError::ProfileNotFound(profile_id.0.clone()));
        }
        self.write_container(&path, uid, &container)
    }

    /// Return the templates of every enabled profile for `uid`, concatenated.
    pub fn enabled_gallery(&self, uid: u32) -> Result<Vec<Template>, StoreError> {
        let path = self.record_path(uid);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let container = self.read_container(&path, uid)?;
        let mut out = Vec::new();
        for entry in container.profiles {
            if entry.profile.enabled {
                out.extend(entry.templates);
            }
        }
        Ok(out)
    }

    /// Replace the profile if it exists, otherwise append it. Used for
    /// re-enrollment under an existing profile id.
    pub fn merge_profile(
        &self,
        uid: u32,
        profile: ProfileRecord,
        templates: Vec<Template>,
    ) -> Result<(), StoreError> {
        if profile.uid != uid {
            return Err(StoreError::ProfileUidMismatch {
                profile_uid: profile.uid,
                container_uid: uid,
            });
        }
        let path = self.record_path(uid);
        let mut container = self.container_or_new(&path, uid)?;
        if let Some(entry) = container
            .profiles
            .iter_mut()
            .find(|p| p.profile.profile_id == profile.profile_id)
        {
            entry.profile = profile;
            entry.templates = templates;
        } else {
            container.profiles.push(ProfileEntry { profile, templates });
        }
        self.write_container(&path, uid, &container)
    }

    /// Fold `src_id` into `dst_id`: append src's templates to dst's, then
    /// remove src. Atomic at the container level (single [`write_container`])
    /// — a missing dst leaves src intact (no partial mutation).
    pub fn combine_profiles(
        &self,
        uid: u32,
        src_id: &ProfileId,
        dst_id: &ProfileId,
    ) -> Result<(), StoreError> {
        let path = self.record_path(uid);
        let mut container = self.require_container(&path, uid)?;
        let src_idx = container
            .profiles
            .iter()
            .position(|p| p.profile.profile_id == *src_id)
            .ok_or_else(|| StoreError::ProfileNotFound(src_id.0.clone()))?;
        let dst_idx = container
            .profiles
            .iter()
            .position(|p| p.profile.profile_id == *dst_id)
            .ok_or_else(|| StoreError::ProfileNotFound(dst_id.0.clone()))?;
        if src_idx == dst_idx {
            return Err(StoreError::ProfileExists(src_id.0.clone()));
        }
        let src_entry = container.profiles.remove(src_idx);
        let dst_idx = if src_idx < dst_idx {
            dst_idx - 1
        } else {
            dst_idx
        };
        container.profiles[dst_idx]
            .templates
            .extend(src_entry.templates);
        self.write_container(&path, uid, &container)
    }

    /// Update `last_used_us` on a profile. Called by the verify path after a
    /// successful match; zero leaves it unchanged from enrollment.
    pub fn touch_last_used(
        &self,
        uid: u32,
        profile_id: &ProfileId,
        last_used_us: u64,
    ) -> Result<(), StoreError> {
        let path = self.record_path(uid);
        let mut container = self.require_container(&path, uid)?;
        let entry = container
            .profiles
            .iter_mut()
            .find(|p| p.profile.profile_id == *profile_id)
            .ok_or_else(|| StoreError::ProfileNotFound(profile_id.0.clone()))?;
        entry.profile.last_used_us = last_used_us;
        self.write_container(&path, uid, &container)
    }

    fn require_container(&self, path: &Path, uid: u32) -> Result<UserContainer, StoreError> {
        if !path.exists() {
            return Err(StoreError::NotFound(uid));
        }
        self.read_container(path, uid)
    }

    fn container_or_new(&self, path: &Path, uid: u32) -> Result<UserContainer, StoreError> {
        if !path.exists() {
            return Ok(UserContainer {
                schema_version: 1,
                profiles: Vec::new(),
            });
        }
        self.read_container(path, uid)
    }

    fn read_container(&self, path: &Path, uid: u32) -> Result<UserContainer, StoreError> {
        reject_symlink(path)?;
        check_owner(path, self.expected_owner_uid())?;
        let raw = std::fs::read(path)?;
        let frame: Decoded = decode(&raw, uid)?;
        let plaintext = self.decrypt(&frame.nonce, &frame.ciphertext, &frame.aad)?;
        match serde_json::from_slice::<ContainerEnvelope>(&plaintext) {
            Ok(ContainerEnvelope::Profiles(c)) => Ok(c),
            Ok(ContainerEnvelope::Legacy(record)) => Ok(legacy_to_container(record)),
            Err(_) => {
                let record: TemplateRecord = serde_json::from_slice(&plaintext)?;
                Ok(legacy_to_container(record))
            }
        }
    }

    fn write_container(
        &self,
        path: &Path,
        uid: u32,
        container: &UserContainer,
    ) -> Result<(), StoreError> {
        reject_symlink(path)?;
        let _guard = FileLock::acquire_with_owner(&self.lock_path(uid), self.expected_owner_uid())?;
        let envelope = ContainerEnvelope::Profiles(container.clone());
        let json = serde_json::to_vec(&envelope)?;
        let cipher = Aes256Gcm::new_from_slice(self.key.key())
            .map_err(|e| StoreError::Crypto(e.to_string()))?;
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = aes_gcm::Nonce::from_slice(&nonce_bytes);
        let aad = aad_v2(uid, "", "container");
        let payload = Payload {
            msg: &json,
            aad: &aad,
        };
        let ciphertext = cipher
            .encrypt(nonce, payload)
            .map_err(|e| StoreError::Crypto(e.to_string()))?;
        let file = encode_v2(&nonce_bytes, "", "container", &ciphertext);
        atomic_write(path, &file, self.expected_owner_uid())?;
        Ok(())
    }
}

fn legacy_to_container(record: TemplateRecord) -> UserContainer {
    UserContainer {
        schema_version: 1,
        profiles: vec![ProfileEntry {
            profile: ProfileRecord {
                profile_id: ProfileId::default(),
                display_name: record.meta.username.clone(),
                enabled: true,
                uid: record.meta.uid,
                model_id: record.meta.model_id.clone(),
                created_at_us: record.meta.created_at_us,
                schema_version: 1,
                last_used_us: 0,
                extrinsics: record.meta.extrinsics.clone(),
            },
            templates: record.templates,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::{EnrollmentMeta, ProfileId, ProfileRecord, Template};

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

    fn test_profile(uid: u32, id: &str, enabled: bool) -> ProfileRecord {
        ProfileRecord {
            profile_id: ProfileId(id.into()),
            display_name: id.into(),
            enabled,
            uid,
            model_id: "sface".into(),
            created_at_us: 1,
            schema_version: 1,
            last_used_us: 0,
            extrinsics: None,
        }
    }

    fn test_template(scale: f32) -> Template {
        Template {
            vector: vec![scale; 128],
            quality: 0.9,
            is_anchor: true,
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

    #[test]
    fn legacy_enrollment_survives_migration() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store.store(&test_record(1000)).unwrap();

        let profiles = store.list_profiles(1000).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].profile.profile_id, ProfileId::default());
        assert!(profiles[0].profile.enabled);
        assert_eq!(profiles[0].templates.len(), 1);
    }

    #[test]
    fn two_profiles_both_match() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store
            .create_profile(
                1000,
                test_profile(1000, "work", true),
                vec![test_template(0.1)],
            )
            .unwrap();
        store
            .create_profile(
                1000,
                test_profile(1000, "home", true),
                vec![test_template(0.2)],
            )
            .unwrap();

        let gallery = store.enabled_gallery(1000).unwrap();
        assert_eq!(gallery.len(), 2);
        assert_eq!(gallery[0].vector, vec![0.1; 128]);
        assert_eq!(gallery[1].vector, vec![0.2; 128]);
    }

    #[test]
    fn disabled_profile_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store
            .create_profile(
                1000,
                test_profile(1000, "work", true),
                vec![test_template(0.1)],
            )
            .unwrap();
        store
            .create_profile(
                1000,
                test_profile(1000, "home", true),
                vec![test_template(0.2)],
            )
            .unwrap();
        store
            .enable_profile(1000, &ProfileId("home".into()), false)
            .unwrap();

        let gallery = store.enabled_gallery(1000).unwrap();
        assert_eq!(gallery.len(), 1);
        assert_eq!(gallery[0].vector, vec![0.1; 128]);
    }

    #[test]
    fn wrong_uid_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        let err = store
            .create_profile(
                1000,
                test_profile(2000, "work", true),
                vec![test_template(0.1)],
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::ProfileUidMismatch {
                    profile_uid: 2000,
                    container_uid: 1000
                }
            ),
            "got {err:?}"
        );
        assert!(store.list_profiles(1000).unwrap().is_empty());
    }

    #[test]
    fn rename_preserves_templates() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store
            .create_profile(
                1000,
                test_profile(1000, "work", true),
                vec![test_template(0.7)],
            )
            .unwrap();
        store
            .rename_profile(1000, &ProfileId("work".into()), "Work Face".into())
            .unwrap();

        let profiles = store.list_profiles(1000).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].profile.display_name, "Work Face");
        assert_eq!(profiles[0].profile.profile_id, ProfileId("work".into()));
        assert_eq!(profiles[0].templates.len(), 1);
        assert_eq!(profiles[0].templates[0].vector, vec![0.7; 128]);
    }

    #[test]
    fn delete_removes_profile() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store
            .create_profile(
                1000,
                test_profile(1000, "work", true),
                vec![test_template(0.1)],
            )
            .unwrap();
        store
            .create_profile(
                1000,
                test_profile(1000, "home", true),
                vec![test_template(0.2)],
            )
            .unwrap();
        store
            .delete_profile(1000, &ProfileId("work".into()))
            .unwrap();

        let profiles = store.list_profiles(1000).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].profile.profile_id, ProfileId("home".into()));
    }

    #[test]
    fn merge_replaces_existing() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store
            .create_profile(
                1000,
                test_profile(1000, "work", true),
                vec![test_template(0.1)],
            )
            .unwrap();
        let mut replaced = test_profile(1000, "work", false);
        replaced.display_name = "Work v2".into();
        store
            .merge_profile(1000, replaced, vec![test_template(0.9), test_template(0.8)])
            .unwrap();

        let profiles = store.list_profiles(1000).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].profile.display_name, "Work v2");
        assert!(!profiles[0].profile.enabled);
        assert_eq!(profiles[0].templates.len(), 2);
        assert_eq!(profiles[0].templates[0].vector, vec![0.9; 128]);
    }

    #[test]
    fn corrupt_profile_isolated() {
        let container = UserContainer {
            schema_version: 1,
            profiles: vec![
                ProfileEntry {
                    profile: test_profile(1000, "work", true),
                    templates: vec![test_template(0.1)],
                },
                ProfileEntry {
                    profile: test_profile(1000, "home", true),
                    templates: vec![test_template(0.2)],
                },
            ],
        };
        let json = serde_json::to_string(&container).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value["profiles"][0]["templates"] = serde_json::json!([]);
        let corrupted = serde_json::to_string(&value).unwrap();
        let parsed: UserContainer = serde_json::from_str(&corrupted).unwrap();

        assert_eq!(parsed.profiles.len(), 2);
        assert!(parsed.profiles[0].templates.is_empty());
        assert_eq!(parsed.profiles[1].templates.len(), 1);
        assert_eq!(parsed.profiles[1].templates[0].vector, vec![0.2; 128]);
        assert_eq!(
            parsed.profiles[1].profile.profile_id,
            ProfileId("home".into())
        );
    }

    #[test]
    fn combine_folds_src_into_dst() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store
            .create_profile(
                1000,
                test_profile(1000, "work", true),
                vec![test_template(0.1), test_template(0.2)],
            )
            .unwrap();
        store
            .create_profile(
                1000,
                test_profile(1000, "home", true),
                vec![test_template(0.3)],
            )
            .unwrap();
        store
            .combine_profiles(1000, &ProfileId("work".into()), &ProfileId("home".into()))
            .unwrap();

        let profiles = store.list_profiles(1000).unwrap();
        assert_eq!(profiles.len(), 1, "src removed, dst remains");
        assert_eq!(profiles[0].profile.profile_id, ProfileId("home".into()));
        assert_eq!(
            profiles[0].templates.len(),
            3,
            "dst has src + dst templates"
        );
        assert_eq!(profiles[0].templates[0].vector, vec![0.3; 128]);
        assert_eq!(profiles[0].templates[1].vector, vec![0.1; 128]);
        assert_eq!(profiles[0].templates[2].vector, vec![0.2; 128]);
    }

    #[test]
    fn combine_missing_dst_preserves_src() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store
            .create_profile(
                1000,
                test_profile(1000, "work", true),
                vec![test_template(0.1)],
            )
            .unwrap();
        let err = store
            .combine_profiles(
                1000,
                &ProfileId("work".into()),
                &ProfileId("nonexistent".into()),
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::ProfileNotFound(_)));

        let profiles = store.list_profiles(1000).unwrap();
        assert_eq!(profiles.len(), 1, "src preserved on missing dst");
        assert_eq!(profiles[0].profile.profile_id, ProfileId("work".into()));
        assert_eq!(profiles[0].templates.len(), 1);
    }

    #[test]
    fn combine_missing_src_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store
            .create_profile(
                1000,
                test_profile(1000, "home", true),
                vec![test_template(0.3)],
            )
            .unwrap();
        let err = store
            .combine_profiles(
                1000,
                &ProfileId("nonexistent".into()),
                &ProfileId("home".into()),
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::ProfileNotFound(_)));
        assert_eq!(store.list_profiles(1000).unwrap().len(), 1);
    }

    #[test]
    fn combine_same_id_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store
            .create_profile(
                1000,
                test_profile(1000, "work", true),
                vec![test_template(0.1)],
            )
            .unwrap();
        let err = store
            .combine_profiles(1000, &ProfileId("work".into()), &ProfileId("work".into()))
            .unwrap_err();
        assert!(matches!(err, StoreError::ProfileExists(_)));
        assert_eq!(store.list_profiles(1000).unwrap().len(), 1);
    }

    #[test]
    fn touch_last_used_updates_field() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store
            .create_profile(
                1000,
                test_profile(1000, "work", true),
                vec![test_template(0.1)],
            )
            .unwrap();
        store
            .touch_last_used(1000, &ProfileId("work".into()), 9_000_000_000)
            .unwrap();

        let profiles = store.list_profiles(1000).unwrap();
        assert_eq!(profiles[0].profile.last_used_us, 9_000_000_000);
    }

    #[test]
    fn touch_last_used_missing_profile_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        store
            .create_profile(
                1000,
                test_profile(1000, "work", true),
                vec![test_template(0.1)],
            )
            .unwrap();
        let err = store
            .touch_last_used(1000, &ProfileId("nope".into()), 1)
            .unwrap_err();
        assert!(matches!(err, StoreError::ProfileNotFound(_)));
    }

    #[test]
    fn last_used_defaults_to_zero_on_deserialize() {
        let json = r#"{"profile_id":"x","display_name":"x","enabled":true,"uid":1,"model_id":"m","created_at_us":0,"schema_version":1}"#;
        let rec: ProfileRecord = serde_json::from_str(json).unwrap();
        assert_eq!(rec.last_used_us, 0);
    }
}
