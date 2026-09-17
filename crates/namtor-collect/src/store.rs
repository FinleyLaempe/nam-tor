// SPDX-License-Identifier: GPL-3.0-only
//! Collection store: writes labelled frame pairs to disk (spec §10.4).
//!
//! Layout:
//! ```text
//! <dir>/
//!   consent.json          — active consent record
//!   <session_id>/
//!     meta.json           — session metadata (incl. withdrawn_at_us)
//!     pair_0000_d.json    — differential statistics (derived features)
//!     ...                 — product path stores ONLY derived stats
//!     raw/                — present ONLY if ResearchCapture wrote raw pairs
//!       pair_0000_on.raw
//!       pair_0000_off.raw
//! ```
//!
//! Spec §10.4: "Prefer derived features and differential statistics over
//! raw face images wherever they suffice." The product path therefore
//! stores only derived stats via [`CollectionStore::write_derived`]. Raw
//! On/Off pairs are written exclusively by [`crate::research::ResearchCapture`]
//! under a separate `raw/` subtree, gated by `NAMTOR_RESEARCH_CAPTURE=1`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::consent::ConsentRecord;
use crate::manifest::{
    permits_derived_export, permits_raw_export, CollectionManifest, RetentionManifest,
    SessionManifest,
};
use crate::research::raw_pair_count;
use crate::session::{SessionKind, SessionMeta};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("consent not granted for contributor {0}")]
    ConsentNotGranted(String),
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("session {0} is withdrawn and may not be exported")]
    SessionWithdrawn(String),
    #[error("raw research capture is not authorised: set NAMTOR_RESEARCH_CAPTURE=1")]
    ResearchNotAuthorised,
}

/// Derived differential statistics for a single frame pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffStats {
    pub session_id: String,
    pub pair_index: u32,
    pub kind: SessionKind,
    pub energy_mean: f32,
    pub energy_ratio: f32,
    pub gradient_score: f32,
    pub specular_score: f32,
    pub radial_score: f32,
}

pub struct CollectionStore {
    dir: PathBuf,
}

impl CollectionStore {
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Write or update the consent record.
    pub fn write_consent(&self, record: &ConsentRecord) -> Result<(), StoreError> {
        let path = self.dir.join("consent.json");
        let mut file = fs::File::create(&path)?;
        serde_json::to_writer_pretty(&mut file, record)?;
        Ok(())
    }

    /// Read the consent record, if any.
    pub fn read_consent(&self) -> Result<Option<ConsentRecord>, StoreError> {
        let path = self.dir.join("consent.json");
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path)?;
        let record: ConsentRecord = serde_json::from_str(&data)?;
        Ok(Some(record))
    }

    /// Assert consent is granted for `contributor_id`.
    pub fn require_consent(&self, contributor_id: &str) -> Result<(), StoreError> {
        match self.read_consent()? {
            Some(r) if r.is_active() && r.contributor_id == contributor_id => Ok(()),
            _ => Err(StoreError::ConsentNotGranted(contributor_id.into())),
        }
    }

    /// Create a session directory and write its metadata.
    pub fn create_session(&self, meta: &SessionMeta) -> Result<PathBuf, StoreError> {
        let session_dir = self.dir.join(&meta.session_id);
        fs::create_dir_all(&session_dir)?;
        let meta_path = session_dir.join("meta.json");
        let mut file = fs::File::create(&meta_path)?;
        serde_json::to_writer_pretty(&mut file, meta)?;
        Ok(session_dir)
    }

    /// Write the derived differential stats for one pair (product path).
    /// This is the only writer the product collection flow uses; it never
    /// touches raw frame bytes.
    pub fn write_derived(&self, session_dir: &Path, stats: &DiffStats) -> Result<(), StoreError> {
        let stats_path = session_dir.join(format!("pair_{:04}_d.json", stats.pair_index));
        let mut f = fs::File::create(&stats_path)?;
        serde_json::to_writer_pretty(&mut f, stats)?;
        Ok(())
    }

    /// Write a raw frame pair (On + Off) alongside the derived stats.
    ///
    /// This is the **research** path. It writes raw face images and is
    /// therefore gated by [`crate::research::ResearchCapture`]; the caller
    /// must hold an authorised `ResearchCapture` handle. The product
    /// collection flow uses [`Self::write_derived`] instead.
    pub fn write_pair(
        &self,
        session_dir: &Path,
        pair_index: u32,
        on_frame: &[u8],
        off_frame: &[u8],
        stats: &DiffStats,
        research: &crate::research::ResearchCapture,
    ) -> Result<(), StoreError> {
        research.write_raw_pair(session_dir, pair_index, on_frame, off_frame)?;
        self.write_derived(session_dir, stats)
    }

    /// Delete a session and all its data (hard deletion).
    pub fn delete_session(&self, session_id: &str) -> Result<(), StoreError> {
        let session_dir = self.dir.join(session_id);
        if !session_dir.exists() {
            return Err(StoreError::SessionNotFound(session_id.into()));
        }
        fs::remove_dir_all(&session_dir)?;
        Ok(())
    }

    /// Withdraw a session (soft deletion). Marks the session as withdrawn by
    /// stamping `withdrawn_at_us` in `meta.json`. Withdrawn sessions are
    /// excluded from [`Self::export_session`] and
    /// [`Self::retention_manifest`] but are not physically removed until
    /// [`Self::delete_session`] is called. Idempotent: withdrawing an
    /// already-withdrawn session is a no-op.
    pub fn withdraw_session(&self, session_id: &str) -> Result<(), StoreError> {
        let meta = self.read_session_meta(session_id)?;
        if meta.withdrawn_at_us.is_some() {
            return Ok(());
        }
        let mut meta = meta;
        meta.withdrawn_at_us = Some(now_us());
        self.write_session_meta(&meta)
    }

    /// Read the metadata for one session.
    pub fn read_session_meta(&self, session_id: &str) -> Result<SessionMeta, StoreError> {
        let session_dir = self.dir.join(session_id);
        let meta_path = session_dir.join("meta.json");
        if !meta_path.exists() {
            return Err(StoreError::SessionNotFound(session_id.into()));
        }
        let meta_str = fs::read_to_string(&meta_path)?;
        Ok(serde_json::from_str(&meta_str)?)
    }

    fn write_session_meta(&self, meta: &SessionMeta) -> Result<(), StoreError> {
        let session_dir = self.dir.join(&meta.session_id);
        let meta_path = session_dir.join("meta.json");
        let mut f = fs::File::create(&meta_path)?;
        serde_json::to_writer_pretty(&mut f, meta)?;
        Ok(())
    }

    /// Count derived pairs (`pair_NNNN_d.json`) in a session directory.
    fn derived_pair_count(&self, session_dir: &Path) -> Result<u32, StoreError> {
        let mut count = 0u32;
        for entry in fs::read_dir(session_dir)? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with("_d.json") {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Read all derived [`DiffStats`] for a session, ordered by pair index.
    fn read_derived(&self, session_dir: &Path) -> Result<Vec<DiffStats>, StoreError> {
        let mut stats: Vec<DiffStats> = Vec::new();
        for entry in fs::read_dir(session_dir)? {
            let entry = entry?;
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.ends_with("_d.json") {
                    let s = fs::read_to_string(&path)?;
                    stats.push(serde_json::from_str(&s)?);
                }
            }
        }
        stats.sort_by_key(|s| s.pair_index);
        Ok(stats)
    }

    /// Export a single session as a [`CollectionManifest`]. Withdrawn
    /// sessions return [`StoreError::SessionWithdrawn`]; the caller must
    /// delete them to remove them from the store. `raw_eligible` is `true`
    /// only when raw pairs exist on disk *and* the contributor's consent
    /// permits raw export.
    pub fn export_session(&self, session_id: &str) -> Result<CollectionManifest, StoreError> {
        let meta = self.read_session_meta(session_id)?;
        if meta.withdrawn_at_us.is_some() {
            return Err(StoreError::SessionWithdrawn(session_id.into()));
        }
        let session_dir = self.dir.join(session_id);
        let derived = self.read_derived(&session_dir)?;
        let raw_pairs = raw_pair_count(&session_dir);
        let consent = self.read_consent()?;
        let raw_eligible = match consent {
            Some(c) if c.is_active() && c.contributor_id == meta.contributor_id => {
                raw_pairs > 0 && permits_raw_export(c.retention_policy)
            }
            _ => false,
        };
        let session = SessionManifest::from_meta(&meta, derived.len() as u32, raw_pairs);
        Ok(CollectionManifest {
            session,
            derived,
            raw_eligible,
        })
    }

    /// Build a [`RetentionManifest`] over all non-withdrawn sessions. The
    /// manifest is `export_eligible` only when every session's contributor
    /// has active consent with at least [`RetentionPolicy::DerivedExport`].
    pub fn retention_manifest(&self) -> Result<RetentionManifest, StoreError> {
        let mut sessions = Vec::new();
        let mut total_raw_pairs: u64 = 0;
        let mut export_eligible = true;
        for id in self.list_sessions()? {
            let meta = self.read_session_meta(&id)?;
            if meta.withdrawn_at_us.is_some() {
                continue;
            }
            let session_dir = self.dir.join(&id);
            let derived_count = self.derived_pair_count(&session_dir)?;
            let raw_pairs = raw_pair_count(&session_dir);
            total_raw_pairs += u64::from(raw_pairs);
            let consent = self.read_consent()?;
            let eligible = match consent {
                Some(c) if c.is_active() && c.contributor_id == meta.contributor_id => {
                    permits_derived_export(c.retention_policy)
                }
                _ => false,
            };
            if !eligible {
                export_eligible = false;
            }
            sessions.push(SessionManifest::from_meta(&meta, derived_count, raw_pairs));
        }
        Ok(RetentionManifest {
            sessions,
            total_raw_pairs,
            export_eligible,
        })
    }

    /// Delete all sessions for a contributor (revocation). Returns the count
    /// of sessions deleted.
    pub fn delete_contributor_sessions(&self, contributor_id: &str) -> Result<usize, StoreError> {
        let mut count = 0;
        if !self.dir.exists() {
            return Ok(0);
        }
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let meta_path = path.join("meta.json");
            if !meta_path.exists() {
                continue;
            }
            let meta_str = fs::read_to_string(&meta_path)?;
            let meta: SessionMeta = serde_json::from_str(&meta_str)?;
            if meta.contributor_id == contributor_id {
                fs::remove_dir_all(&path)?;
                count += 1;
            }
        }
        Ok(count)
    }

    /// List all session IDs.
    pub fn list_sessions(&self) -> Result<Vec<String>, StoreError> {
        let mut ids = Vec::new();
        if !self.dir.exists() {
            return Ok(ids);
        }
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() && path.join("meta.json").exists() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    ids.push(name.into());
                }
            }
        }
        Ok(ids)
    }
}

fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::{ConsentRecord, RetentionPolicy};
    use crate::research::ResearchCapture;
    use crate::session::{CollectionMetadata, SessionKind};

    fn test_meta(id: &str, contributor: &str, kind: SessionKind) -> SessionMeta {
        SessionMeta {
            session_id: id.into(),
            contributor_id: contributor.into(),
            kind,
            started_at_us: 0,
            device: "test".into(),
            ambient_notes: None,
            metadata: CollectionMetadata::default(),
            withdrawn_at_us: None,
        }
    }

    fn diff_stats(session_id: &str, idx: u32, kind: SessionKind) -> DiffStats {
        DiffStats {
            session_id: session_id.into(),
            pair_index: idx,
            kind,
            energy_mean: 50.0,
            energy_ratio: 0.5,
            gradient_score: 0.8,
            specular_score: 0.7,
            radial_score: 0.9,
        }
    }

    #[test]
    fn consent_write_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let r = ConsentRecord::grant("alice", "pad-training");
        store.write_consent(&r).unwrap();
        let back = store.read_consent().unwrap().unwrap();
        assert!(back.is_active());
        assert_eq!(back.contributor_id, "alice");
    }

    #[test]
    fn require_consent_fails_without_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        assert!(store.require_consent("alice").is_err());
    }

    #[test]
    fn require_consent_passes_with_grant() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let r = ConsentRecord::grant("alice", "pad-training");
        store.write_consent(&r).unwrap();
        assert!(store.require_consent("alice").is_ok());
    }

    #[test]
    fn require_consent_fails_after_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let mut r = ConsentRecord::grant("alice", "pad-training");
        store.write_consent(&r).unwrap();
        r.revoke();
        store.write_consent(&r).unwrap();
        assert!(store.require_consent("alice").is_err());
    }

    #[test]
    fn create_session_writes_meta() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let meta = test_meta("sess1", "alice", SessionKind::Bonafide);
        let session_dir = store.create_session(&meta).unwrap();
        assert!(session_dir.join("meta.json").exists());
    }

    #[test]
    fn write_pair_creates_files_under_raw_subdir() {
        let _env = crate::test_env::ResearchEnv::authorised();
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let meta = test_meta("sess1", "alice", SessionKind::Bonafide);
        let session_dir = store.create_session(&meta).unwrap();

        let on = vec![100u8; 64];
        let off = vec![50u8; 64];
        let stats = diff_stats("sess1", 0, SessionKind::Bonafide);
        let cap = ResearchCapture::authorize().unwrap();
        store
            .write_pair(&session_dir, 0, &on, &off, &stats, &cap)
            .unwrap();
        assert!(session_dir.join("raw/pair_0000_on.raw").exists());
        assert!(session_dir.join("raw/pair_0000_off.raw").exists());
        assert!(session_dir.join("pair_0000_d.json").exists());
    }

    #[test]
    fn write_derived_only_creates_no_raw_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let meta = test_meta("sess1", "alice", SessionKind::Bonafide);
        let session_dir = store.create_session(&meta).unwrap();
        let stats = diff_stats("sess1", 0, SessionKind::Bonafide);
        store.write_derived(&session_dir, &stats).unwrap();
        assert!(session_dir.join("pair_0000_d.json").exists());
        assert!(!session_dir.join("raw").exists());
        assert!(!session_dir.join("pair_0000_on.raw").exists());
    }

    #[test]
    fn delete_session_removes_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let meta = test_meta("sess1", "alice", SessionKind::Bonafide);
        store.create_session(&meta).unwrap();
        store.delete_session("sess1").unwrap();
        assert!(!dir.path().join("sess1").exists());
    }

    #[test]
    fn delete_contributor_sessions_removes_all() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        store
            .create_session(&test_meta("s1", "alice", SessionKind::Bonafide))
            .unwrap();
        store
            .create_session(&test_meta("s2", "alice", SessionKind::Print))
            .unwrap();
        store
            .create_session(&test_meta("s3", "bob", SessionKind::Bonafide))
            .unwrap();

        let deleted = store.delete_contributor_sessions("alice").unwrap();
        assert_eq!(deleted, 2);
        assert!(store.list_sessions().unwrap().contains(&"s3".into()));
    }

    #[test]
    fn list_sessions_returns_all() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        store
            .create_session(&test_meta("s1", "alice", SessionKind::Bonafide))
            .unwrap();
        store
            .create_session(&test_meta("s2", "bob", SessionKind::Print))
            .unwrap();
        let ids = store.list_sessions().unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn grant_withdraw_delete_export_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let consent = ConsentRecord::grant("alice", "pad-training");
        store.write_consent(&consent).unwrap();
        let session_dir = store
            .create_session(&test_meta("s1", "alice", SessionKind::Bonafide))
            .unwrap();
        store
            .write_derived(&session_dir, &diff_stats("s1", 0, SessionKind::Bonafide))
            .unwrap();
        store
            .write_derived(&session_dir, &diff_stats("s1", 1, SessionKind::Bonafide))
            .unwrap();

        let manifest = store.export_session("s1").unwrap();
        assert_eq!(manifest.session.session_id, "s1");
        assert_eq!(manifest.derived.len(), 2);
        assert!(!manifest.raw_eligible);

        store.withdraw_session("s1").unwrap();
        let err = store.export_session("s1").unwrap_err();
        assert!(matches!(err, StoreError::SessionWithdrawn(_)));

        store.delete_session("s1").unwrap();
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn withdrawn_session_excluded_from_retention_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let consent = ConsentRecord::grant("alice", "pad-training");
        store.write_consent(&consent).unwrap();
        let d1 = store
            .create_session(&test_meta("s1", "alice", SessionKind::Bonafide))
            .unwrap();
        store
            .write_derived(&d1, &diff_stats("s1", 0, SessionKind::Bonafide))
            .unwrap();
        let d2 = store
            .create_session(&test_meta("s2", "alice", SessionKind::Print))
            .unwrap();
        store
            .write_derived(&d2, &diff_stats("s2", 0, SessionKind::Print))
            .unwrap();

        store.withdraw_session("s2").unwrap();

        let manifest = store.retention_manifest().unwrap();
        assert_eq!(manifest.sessions.len(), 1);
        assert_eq!(manifest.sessions[0].session_id, "s1");
        assert_eq!(manifest.total_raw_pairs, 0);
    }

    #[test]
    fn revoked_consent_makes_manifest_not_export_eligible() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let mut consent =
            ConsentRecord::grant_versioned("alice", "pad", 1, RetentionPolicy::DerivedExport);
        store.write_consent(&consent).unwrap();
        let session_dir = store
            .create_session(&test_meta("s1", "alice", SessionKind::Bonafide))
            .unwrap();
        store
            .write_derived(&session_dir, &diff_stats("s1", 0, SessionKind::Bonafide))
            .unwrap();

        let m1 = store.retention_manifest().unwrap();
        assert!(m1.export_eligible);

        consent.revoke();
        store.write_consent(&consent).unwrap();
        let m2 = store.retention_manifest().unwrap();
        assert!(!m2.export_eligible);
    }

    #[test]
    fn local_only_consent_is_not_export_eligible() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let consent = ConsentRecord::grant("alice", "pad");
        store.write_consent(&consent).unwrap();
        let session_dir = store
            .create_session(&test_meta("s1", "alice", SessionKind::Bonafide))
            .unwrap();
        store
            .write_derived(&session_dir, &diff_stats("s1", 0, SessionKind::Bonafide))
            .unwrap();
        let manifest = store.retention_manifest().unwrap();
        assert!(!manifest.export_eligible);
        let export = store.export_session("s1").unwrap();
        assert!(!export.raw_eligible);
    }

    #[test]
    fn product_enrollment_stores_no_raw_images() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let consent = ConsentRecord::grant("alice", "pad");
        store.write_consent(&consent).unwrap();
        let session_dir = store
            .create_session(&test_meta("s1", "alice", SessionKind::Bonafide))
            .unwrap();
        store
            .write_derived(&session_dir, &diff_stats("s1", 0, SessionKind::Bonafide))
            .unwrap();
        assert!(!session_dir.join("raw").exists());
        assert!(!session_dir.join("pair_0000_on.raw").exists());
        assert!(!session_dir.join("pair_0000_off.raw").exists());
        let export = store.export_session("s1").unwrap();
        assert_eq!(export.session.raw_pair_count, 0);
        assert!(!export.raw_eligible);
    }

    #[test]
    fn versioned_consent_round_trips_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let r = ConsentRecord::grant_versioned("alice", "research", 4, RetentionPolicy::FullExport);
        store.write_consent(&r).unwrap();
        let back = store.read_consent().unwrap().unwrap();
        assert_eq!(back.purpose_version, 4);
        assert_eq!(back.retention_policy, RetentionPolicy::FullExport);
    }

    #[test]
    fn retention_manifest_aggregates_raw_pairs_and_eligibility() {
        let _env = crate::test_env::ResearchEnv::authorised();
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let consent =
            ConsentRecord::grant_versioned("alice", "pad", 2, RetentionPolicy::DerivedExport);
        store.write_consent(&consent).unwrap();
        let d1 = store
            .create_session(&test_meta("s1", "alice", SessionKind::Bonafide))
            .unwrap();
        store
            .write_derived(&d1, &diff_stats("s1", 0, SessionKind::Bonafide))
            .unwrap();
        let cap = ResearchCapture::authorize().unwrap();
        let d2 = store
            .create_session(&test_meta("s2", "alice", SessionKind::Print))
            .unwrap();
        store
            .write_pair(
                &d2,
                0,
                &[9u8; 4],
                &[0u8; 4],
                &diff_stats("s2", 0, SessionKind::Print),
                &cap,
            )
            .unwrap();
        let manifest = store.retention_manifest().unwrap();
        assert_eq!(manifest.sessions.len(), 2);
        assert_eq!(manifest.total_raw_pairs, 1);
        assert!(manifest.export_eligible);
        let export = store.export_session("s2").unwrap();
        assert!(!export.raw_eligible);
    }

    #[test]
    fn withdraw_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        store
            .create_session(&test_meta("s1", "alice", SessionKind::Bonafide))
            .unwrap();
        store.withdraw_session("s1").unwrap();
        store.withdraw_session("s1").unwrap();
        let meta = store.read_session_meta("s1").unwrap();
        assert!(meta.withdrawn_at_us.is_some());
    }
}
