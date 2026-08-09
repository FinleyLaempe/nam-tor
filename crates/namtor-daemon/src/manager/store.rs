// SPDX-License-Identifier: GPL-3.0-only
//! Profile store bridge: durable TemplateStore transactions for the manager.
//!
//! The [`ProfileStore`] trait abstracts the encrypted template store so
//! [`crate::dbus::ManagerService`] can persist profile lifecycle (commit,
//! rename, enable, delete, merge, replace, last-used) without holding the
//! raw [`namtor_store::TemplateStore`] or any in-memory profile map. The UI
//! has no direct store access — every mutation goes through the manager API.
//!
//! [`TemplateProfileStore`] wraps a real [`namtor_store::TemplateStore`];
//! [`NullProfileStore`] is a no-op used by bus-service tests that exercise
//! D-Bus mechanics, not profile persistence.

use std::sync::Arc;

use namtor_store::store::StoreError;
use namtor_store::template::{ProfileEntry, ProfileId, ProfileRecord, Template};
use namtor_store::{KeySource, TemplateStore};

use crate::dbus::ApiError;
use crate::manager::contract::{ProtectionKeyKind, ProtectionStatus};

/// Durable, encrypted profile storage backing the manager API. All methods
/// are atomic at the container level — a partial mutation (e.g. merge with a
/// missing dst) leaves the store byte-identical.
pub trait ProfileStore: Send + Sync {
    fn list_profiles(&self, uid: u32) -> Result<Vec<ProfileEntry>, StoreError>;
    fn create_profile(
        &self,
        uid: u32,
        profile: ProfileRecord,
        templates: Vec<Template>,
    ) -> Result<(), StoreError>;
    fn get_profile(&self, uid: u32, profile_id: &ProfileId) -> Result<ProfileEntry, StoreError>;
    fn rename_profile(
        &self,
        uid: u32,
        profile_id: &ProfileId,
        new_name: String,
    ) -> Result<(), StoreError>;
    fn enable_profile(
        &self,
        uid: u32,
        profile_id: &ProfileId,
        enabled: bool,
    ) -> Result<(), StoreError>;
    fn delete_profile(&self, uid: u32, profile_id: &ProfileId) -> Result<(), StoreError>;
    fn combine_profiles(
        &self,
        uid: u32,
        src_id: &ProfileId,
        dst_id: &ProfileId,
    ) -> Result<(), StoreError>;
    fn merge_profile(
        &self,
        uid: u32,
        profile: ProfileRecord,
        templates: Vec<Template>,
    ) -> Result<(), StoreError>;
    fn touch_last_used(
        &self,
        uid: u32,
        profile_id: &ProfileId,
        last_used_us: u64,
    ) -> Result<(), StoreError>;
    fn protection_status(&self) -> ProtectionStatus;
}

/// Real store adapter. Wraps an [`Arc<TemplateStore>`] so the manager can
/// share a single store across threads (the daemon + bus service).
pub struct TemplateProfileStore {
    inner: Arc<TemplateStore>,
}

impl TemplateProfileStore {
    pub fn new(store: Arc<TemplateStore>) -> Self {
        Self { inner: store }
    }
}

impl ProfileStore for TemplateProfileStore {
    fn list_profiles(&self, uid: u32) -> Result<Vec<ProfileEntry>, StoreError> {
        self.inner.list_profiles(uid)
    }

    fn create_profile(
        &self,
        uid: u32,
        profile: ProfileRecord,
        templates: Vec<Template>,
    ) -> Result<(), StoreError> {
        self.inner.create_profile(uid, profile, templates)
    }

    fn get_profile(&self, uid: u32, profile_id: &ProfileId) -> Result<ProfileEntry, StoreError> {
        let profiles = self.inner.list_profiles(uid)?;
        profiles
            .into_iter()
            .find(|p| p.profile.profile_id == *profile_id)
            .ok_or_else(|| StoreError::ProfileNotFound(profile_id.0.clone()))
    }

    fn rename_profile(
        &self,
        uid: u32,
        profile_id: &ProfileId,
        new_name: String,
    ) -> Result<(), StoreError> {
        self.inner.rename_profile(uid, profile_id, new_name)
    }

    fn enable_profile(
        &self,
        uid: u32,
        profile_id: &ProfileId,
        enabled: bool,
    ) -> Result<(), StoreError> {
        self.inner.enable_profile(uid, profile_id, enabled)
    }

    fn delete_profile(&self, uid: u32, profile_id: &ProfileId) -> Result<(), StoreError> {
        self.inner.delete_profile(uid, profile_id)
    }

    fn combine_profiles(
        &self,
        uid: u32,
        src_id: &ProfileId,
        dst_id: &ProfileId,
    ) -> Result<(), StoreError> {
        self.inner.combine_profiles(uid, src_id, dst_id)
    }

    fn merge_profile(
        &self,
        uid: u32,
        profile: ProfileRecord,
        templates: Vec<Template>,
    ) -> Result<(), StoreError> {
        self.inner.merge_profile(uid, profile, templates)
    }

    fn touch_last_used(
        &self,
        uid: u32,
        profile_id: &ProfileId,
        last_used_us: u64,
    ) -> Result<(), StoreError> {
        self.inner.touch_last_used(uid, profile_id, last_used_us)
    }

    fn protection_status(&self) -> ProtectionStatus {
        let state = self.inner.protection_state();
        let sealing = match state.sealing {
            KeySource::Tpm2 => ProtectionKeyKind::Tpm2,
            KeySource::File => ProtectionKeyKind::Software,
        };
        ProtectionStatus {
            sealing,
            has_tpm: state.has_tpm,
            warning: state.warning,
        }
    }
}

/// No-op store for bus-service tests that exercise D-Bus mechanics, not
/// profile persistence. Reads return empty; writes return [`StoreError::State`].
pub struct NullProfileStore;

impl ProfileStore for NullProfileStore {
    fn list_profiles(&self, _uid: u32) -> Result<Vec<ProfileEntry>, StoreError> {
        Ok(Vec::new())
    }

    fn create_profile(
        &self,
        _uid: u32,
        _profile: ProfileRecord,
        _templates: Vec<Template>,
    ) -> Result<(), StoreError> {
        Err(StoreError::Lock("null profile store".into()))
    }

    fn get_profile(&self, _uid: u32, id: &ProfileId) -> Result<ProfileEntry, StoreError> {
        Err(StoreError::ProfileNotFound(id.0.clone()))
    }

    fn rename_profile(
        &self,
        _uid: u32,
        id: &ProfileId,
        _new_name: String,
    ) -> Result<(), StoreError> {
        Err(StoreError::ProfileNotFound(id.0.clone()))
    }

    fn enable_profile(&self, _uid: u32, id: &ProfileId, _enabled: bool) -> Result<(), StoreError> {
        Err(StoreError::ProfileNotFound(id.0.clone()))
    }

    fn delete_profile(&self, _uid: u32, id: &ProfileId) -> Result<(), StoreError> {
        Err(StoreError::ProfileNotFound(id.0.clone()))
    }

    fn combine_profiles(
        &self,
        _uid: u32,
        src_id: &ProfileId,
        _dst_id: &ProfileId,
    ) -> Result<(), StoreError> {
        Err(StoreError::ProfileNotFound(src_id.0.clone()))
    }

    fn merge_profile(
        &self,
        _uid: u32,
        _profile: ProfileRecord,
        _templates: Vec<Template>,
    ) -> Result<(), StoreError> {
        Err(StoreError::Lock("null profile store".into()))
    }

    fn touch_last_used(
        &self,
        _uid: u32,
        id: &ProfileId,
        _last_used_us: u64,
    ) -> Result<(), StoreError> {
        Err(StoreError::ProfileNotFound(id.0.clone()))
    }

    fn protection_status(&self) -> ProtectionStatus {
        ProtectionStatus {
            sealing: ProtectionKeyKind::Software,
            has_tpm: false,
            warning: "no store loaded".into(),
        }
    }
}

/// Map a [`StoreError`] to an [`ApiError`] for the D-Bus surface. Profile-
/// level errors (`ProfileNotFound`, `ProfileExists`) map to `NotFound` /
/// `Exists`; uid-level `NotFound` / `AlreadyExists` map similarly; everything
/// else (crypto, TPM2, lock, owner mismatch, schema) collapses to `State`.
pub fn map_store_error(e: StoreError) -> ApiError {
    match e {
        StoreError::ProfileNotFound(id) => ApiError::NotFound(id),
        StoreError::ProfileExists(id) => ApiError::Exists(id),
        StoreError::NotFound(uid) => ApiError::NotFound(uid.to_string()),
        StoreError::AlreadyExists(uid) => ApiError::Exists(uid.to_string()),
        StoreError::ProfileUidMismatch {
            profile_uid,
            container_uid,
        } => ApiError::State(format!("uid mismatch {profile_uid} != {container_uid}")),
        StoreError::Io(e) => ApiError::Io(e),
        StoreError::Crypto(s) => ApiError::State(format!("crypto: {s}")),
        StoreError::Tpm2(s) => ApiError::State(format!("tpm2: {s}")),
        StoreError::Serde(e) => ApiError::State(format!("serde: {e}")),
        StoreError::SchemaVersion(v) => ApiError::State(format!("schema version {v}")),
        StoreError::Symlink(p) => ApiError::State(format!("symlink refused: {}", p.display())),
        StoreError::WrongOwner {
            expected, actual, ..
        } => ApiError::State(format!("owner mismatch {expected} != {actual}")),
        StoreError::Lock(s) => ApiError::State(format!("lock: {s}")),
        StoreError::DowngradeRefused(s) => ApiError::State(format!("downgrade refused: {s}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_store(dir: &std::path::Path) -> Arc<TemplateStore> {
        Arc::new(TemplateStore::open(dir, &dir.join("key")).unwrap())
    }

    fn sample_profile(uid: u32, id: &str) -> ProfileRecord {
        ProfileRecord {
            profile_id: ProfileId(id.into()),
            display_name: id.into(),
            enabled: true,
            uid,
            model_id: "sface".into(),
            created_at_us: 1,
            schema_version: 1,
            last_used_us: 0,
            extrinsics: None,
        }
    }

    #[test]
    fn template_store_adapter_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        let adapter = TemplateProfileStore::new(store);

        adapter
            .create_profile(1000, sample_profile(1000, "work"), vec![])
            .unwrap();
        let profiles = adapter.list_profiles(1000).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].profile.display_name, "work");
    }

    #[test]
    fn template_store_adapter_combine() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        let adapter = TemplateProfileStore::new(store);

        adapter
            .create_profile(1000, sample_profile(1000, "a"), vec![])
            .unwrap();
        adapter
            .create_profile(1000, sample_profile(1000, "b"), vec![])
            .unwrap();
        adapter
            .combine_profiles(1000, &ProfileId("a".into()), &ProfileId("b".into()))
            .unwrap();
        let profiles = adapter.list_profiles(1000).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].profile.profile_id, ProfileId("b".into()));
    }

    #[test]
    fn null_store_returns_empty_and_errors() {
        let null = NullProfileStore;
        assert!(null.list_profiles(1000).unwrap().is_empty());
        assert!(null
            .create_profile(1000, sample_profile(1000, "x"), vec![])
            .is_err());
    }

    #[test]
    fn map_store_error_variants() {
        assert!(matches!(
            map_store_error(StoreError::ProfileNotFound("p".into())),
            ApiError::NotFound(_)
        ));
        assert!(matches!(
            map_store_error(StoreError::ProfileExists("p".into())),
            ApiError::Exists(_)
        ));
        assert!(matches!(
            map_store_error(StoreError::NotFound(1000)),
            ApiError::NotFound(_)
        ));
        assert!(matches!(
            map_store_error(StoreError::AlreadyExists(1000)),
            ApiError::Exists(_)
        ));
        assert!(matches!(
            map_store_error(StoreError::Crypto("boom".into())),
            ApiError::State(_)
        ));
        assert!(matches!(
            map_store_error(StoreError::Io(std::io::Error::other("x"))),
            ApiError::Io(_)
        ));
    }

    #[test]
    fn template_store_protection_status_software_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        let adapter = TemplateProfileStore::new(store);
        let status = adapter.protection_status();
        assert_eq!(status.sealing, ProtectionKeyKind::Software);
        assert!(!status.warning.is_empty());
    }

    #[test]
    fn null_store_protection_status_reports_no_store() {
        let null = NullProfileStore;
        let status = null.protection_status();
        assert_eq!(status.sealing, ProtectionKeyKind::Software);
        assert!(!status.warning.is_empty());
    }

    #[test]
    fn protection_status_contains_no_key_material() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        let adapter = TemplateProfileStore::new(store);
        let status = adapter.protection_status();
        let debug_str = format!("{status:?}");
        assert!(
            !debug_str.contains(dir.path().to_str().unwrap()),
            "protection status must not leak the store path"
        );
        assert!(
            !debug_str.contains("/var/lib/namtor"),
            "protection status must not leak runtime paths"
        );
    }

    #[test]
    fn map_store_error_downgrade_refused() {
        let err = map_store_error(StoreError::DowngradeRefused("tpm gone".into()));
        assert!(matches!(err, ApiError::State(ref s) if s.contains("downgrade refused")));
    }
}
