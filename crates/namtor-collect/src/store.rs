// SPDX-License-Identifier: GPL-3.0-only
//! Collection store: writes labelled frame pairs to disk (spec §10.4).
//!
//! Layout:
//! ```text
//! <dir>/
//!   consent.json          — active consent record
//!   <session_id>/
//!     meta.json           — session metadata
//!     pair_0000_on.raw    — illuminated frame (Grey8)
//!     pair_0000_off.raw   — dark frame (Grey8)
//!     pair_0000_d.json    — differential statistics (derived features)
//!     ...
//! ```
//!
//! Spec §10.4: "Prefer derived features and differential statistics over
//! raw face images wherever they suffice." Both raw pairs and derived stats
//! are stored, so contributors can choose to share only the latter.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::consent::ConsentRecord;
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

    /// Write a raw frame pair (On + Off) and the derived differential stats.
    pub fn write_pair(
        &self,
        session_dir: &Path,
        pair_index: u32,
        on_frame: &[u8],
        off_frame: &[u8],
        stats: &DiffStats,
    ) -> Result<(), StoreError> {
        let on_path = session_dir.join(format!("pair_{pair_index:04}_on.raw"));
        let off_path = session_dir.join(format!("pair_{pair_index:04}_off.raw"));
        let stats_path = session_dir.join(format!("pair_{pair_index:04}_d.json"));

        let mut f = fs::File::create(&on_path)?;
        f.write_all(on_frame)?;

        let mut f = fs::File::create(&off_path)?;
        f.write_all(off_frame)?;

        let mut f = fs::File::create(&stats_path)?;
        serde_json::to_writer_pretty(&mut f, stats)?;
        Ok(())
    }

    /// Delete a session and all its data (withdrawal).
    pub fn delete_session(&self, session_id: &str) -> Result<(), StoreError> {
        let session_dir = self.dir.join(session_id);
        if !session_dir.exists() {
            return Err(StoreError::SessionNotFound(session_id.into()));
        }
        fs::remove_dir_all(&session_dir)?;
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::ConsentRecord;
    use crate::session::SessionKind;

    fn test_meta(id: &str, contributor: &str, kind: SessionKind) -> SessionMeta {
        SessionMeta {
            session_id: id.into(),
            contributor_id: contributor.into(),
            kind,
            started_at_us: 0,
            device: "test".into(),
            ambient_notes: None,
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
    fn write_pair_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let meta = test_meta("sess1", "alice", SessionKind::Bonafide);
        let session_dir = store.create_session(&meta).unwrap();

        let on = vec![100u8; 64];
        let off = vec![50u8; 64];
        let stats = DiffStats {
            session_id: "sess1".into(),
            pair_index: 0,
            kind: SessionKind::Bonafide,
            energy_mean: 50.0,
            energy_ratio: 0.5,
            gradient_score: 0.8,
            specular_score: 0.7,
            radial_score: 0.9,
        };
        store
            .write_pair(&session_dir, 0, &on, &off, &stats)
            .unwrap();
        assert!(session_dir.join("pair_0000_on.raw").exists());
        assert!(session_dir.join("pair_0000_off.raw").exists());
        assert!(session_dir.join("pair_0000_d.json").exists());
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
}
