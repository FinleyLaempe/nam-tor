// SPDX-License-Identifier: GPL-3.0-only
//! Separately-authorised raw research capture (spec §10.4).
//!
//! The product collection path stores **only derived differential
//! statistics** — no raw face images. Raw frame capture for research is a
//! distinct, opt-in action gated by the `NAMTOR_RESEARCH_CAPTURE=1`
//! environment variable. This keeps the default store free of biometric
//! imagery and forces anyone recording raw frames to take an explicit,
//! auditable step.
//!
//! When authorised, [`ResearchCapture`] writes raw On/Off pairs to a
//! *separate* `raw/` subtree of the session directory. The product store's
//! [`crate::store::CollectionStore::write_pair`] never touches raw bytes;
//! the research path is the only writer of `*_on.raw` / `*_off.raw`.

use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::store::StoreError;

/// Marker env var. Must be literally `"1"` to authorise raw capture.
pub const RESEARCH_ENV_VAR: &str = "NAMTOR_RESEARCH_CAPTURE";

/// Handle granted only when raw research capture is explicitly authorised.
/// Construct via [`ResearchCapture::authorize`]; there is no public
/// constructor, so the type's existence *is* the proof of authorisation.
#[derive(Debug)]
pub struct ResearchCapture {
    _private: (),
}

impl ResearchCapture {
    /// Returns `Ok` only when `NAMTOR_RESEARCH_CAPTURE=1` is set in the
    /// current process environment. Any other value (or absence) yields
    /// `Err(StoreError::ResearchNotAuthorised)`.
    pub fn authorize() -> Result<Self, StoreError> {
        match env::var_os(RESEARCH_ENV_VAR) {
            Some(v) if v == "1" => Ok(Self { _private: () }),
            _ => Err(StoreError::ResearchNotAuthorised),
        }
    }

    /// Write a single raw On/Off pair under `session_dir/raw/`. Returns the
    /// pair index used in the filenames. The caller is responsible for
    /// recording the derived stats via the product store path.
    pub fn write_raw_pair(
        &self,
        session_dir: &Path,
        pair_index: u32,
        on_frame: &[u8],
        off_frame: &[u8],
    ) -> Result<(), StoreError> {
        let raw_dir = session_dir.join("raw");
        fs::create_dir_all(&raw_dir)?;
        let on_path = raw_dir.join(format!("pair_{pair_index:04}_on.raw"));
        let off_path = raw_dir.join(format!("pair_{pair_index:04}_off.raw"));
        let mut f = fs::File::create(&on_path)?;
        f.write_all(on_frame)?;
        let mut f = fs::File::create(&off_path)?;
        f.write_all(off_frame)?;
        Ok(())
    }
}

/// Convenience check used by the store to decide whether raw pairs exist for
/// a session (the research path writes under `raw/`).
pub fn raw_pair_count(session_dir: &Path) -> u32 {
    let raw_dir = session_dir.join("raw");
    let Ok(entries) = fs::read_dir(&raw_dir) else {
        return 0;
    };
    let mut count = 0u32;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if let Some(s) = name.to_str() {
            if s.ends_with("_on.raw") {
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use crate::raw_pair_count;
    use crate::session::{SessionKind, SessionMeta};
    use crate::CollectionStore;
    use crate::ResearchCapture;
    use crate::RESEARCH_ENV_VAR;
    use std::env;
    use std::sync::Mutex;
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_meta(id: &str, contributor: &str) -> SessionMeta {
        SessionMeta {
            session_id: id.into(),
            contributor_id: contributor.into(),
            kind: SessionKind::Bonafide,
            started_at_us: 0,
            device: "test".into(),
            ambient_notes: None,
            metadata: crate::session::CollectionMetadata::default(),
            withdrawn_at_us: None,
        }
    }

    #[test]
    fn authorize_fails_without_env_var() {
        let _guard = TEST_LOCK.lock().unwrap();
        // SAFETY of test: env var is project-scoped and tests run single-threaded.
        env::remove_var(RESEARCH_ENV_VAR);
        assert!(ResearchCapture::authorize().is_err());
    }

    #[test]
    fn authorize_succeeds_with_env_var() {
        let _guard = TEST_LOCK.lock().unwrap();
        env::set_var(RESEARCH_ENV_VAR, "1");
        assert!(ResearchCapture::authorize().is_ok());
        env::remove_var(RESEARCH_ENV_VAR);
    }

    #[test]
    fn write_raw_pair_writes_files_under_raw_subdir() {
        let _guard = TEST_LOCK.lock().unwrap();
        env::set_var(RESEARCH_ENV_VAR, "1");
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let session_dir = store.create_session(&test_meta("s1", "alice")).unwrap();
        let cap = ResearchCapture::authorize().unwrap();
        cap.write_raw_pair(&session_dir, 0, &[1, 2, 3], &[4, 5, 6])
            .unwrap();
        assert!(session_dir.join("raw/pair_0000_on.raw").exists());
        assert!(session_dir.join("raw/pair_0000_off.raw").exists());
        assert_eq!(raw_pair_count(&session_dir), 1);
        env::remove_var(RESEARCH_ENV_VAR);
    }

    #[test]
    fn product_store_writes_no_raw_files() {
        let _guard = TEST_LOCK.lock().unwrap();
        env::remove_var(RESEARCH_ENV_VAR);
        let dir = tempfile::tempdir().unwrap();
        let store = CollectionStore::open(dir.path()).unwrap();
        let session_dir = store.create_session(&test_meta("s1", "alice")).unwrap();
        let stats = crate::store::DiffStats {
            session_id: "s1".into(),
            pair_index: 0,
            kind: SessionKind::Bonafide,
            energy_mean: 1.0,
            energy_ratio: 0.5,
            gradient_score: 0.0,
            specular_score: 0.0,
            radial_score: 0.0,
        };
        store.write_derived(&session_dir, &stats).unwrap();
        // Derived file present, no raw subtree.
        assert!(session_dir.join("pair_0000_d.json").exists());
        assert!(!session_dir.join("raw").exists());
        assert!(!session_dir.join("pair_0000_on.raw").exists());
        assert!(!session_dir.join("pair_0000_off.raw").exists());
        assert_eq!(raw_pair_count(&session_dir), 0);
    }
}
