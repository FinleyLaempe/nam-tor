// SPDX-License-Identifier: GPL-3.0-only
//! Licence acceptance record: refuses to load a model unless the user has
//! accepted its upstream licence. The record is a JSON file on disk, written
//! only when `NAMTOR_ACCEPT_LICENCE=1` is set in the environment.

use std::path::Path;

use namtor_proto::worker::ModelSpec;
use serde::{Deserialize, Serialize};

use crate::DaemonError;

/// A recorded licence acceptance for one model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LicenceRecord {
    pub model_id: String,
    pub licence_url: String,
    pub licence_sha256: [u8; 32],
    pub accepted_at_us: u64,
    pub accepted_by: String,
}

impl LicenceRecord {
    fn path_for(root: &Path, id: &str) -> std::path::PathBuf {
        root.join(id).join("licence.accepted")
    }
}

/// Check that a licence has been accepted for `spec`. If the record file
/// exists, returns Ok (idempotent). If not, requires `NAMTOR_ACCEPT_LICENCE=1`
/// in the environment to write the record; otherwise returns an error
/// (fail-closed — the daemon never loads an unlicenced model).
pub fn ensure_licence_accepted(
    root: &Path,
    spec: &ModelSpec,
    licence_url: &str,
    licence_sha256: [u8; 32],
    accepted_by: &str,
) -> Result<(), DaemonError> {
    let dir = root.join(&spec.id);
    std::fs::create_dir_all(&dir)?;
    let path = LicenceRecord::path_for(root, &spec.id);

    if path.exists() {
        let data = std::fs::read_to_string(&path)?;
        let record: LicenceRecord = serde_json::from_str(&data)
            .map_err(|e| DaemonError::State(format!("licence record: {e}")))?;
        if record.licence_sha256 != licence_sha256 {
            return Err(DaemonError::State(format!(
                "licence sha256 mismatch for model {}",
                spec.id
            )));
        }
        return Ok(());
    }

    if std::env::var("NAMTOR_ACCEPT_LICENCE").as_deref() != Ok("1") {
        return Err(DaemonError::State(format!(
            "licence not accepted for model {}; run with NAMTOR_ACCEPT_LICENCE=1 to accept",
            spec.id
        )));
    }

    let record = LicenceRecord {
        model_id: spec.id.clone(),
        licence_url: licence_url.into(),
        licence_sha256,
        accepted_at_us: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0),
        accepted_by: accepted_by.into(),
    };
    let json = serde_json::to_string_pretty(&record)?;
    std::fs::write(&path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_proto::worker::Quant;

    fn spec() -> ModelSpec {
        ModelSpec {
            id: "test-model".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        }
    }

    #[test]
    fn no_record_no_env_errors() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::remove_var("NAMTOR_ACCEPT_LICENCE");
        let err = ensure_licence_accepted(
            tmp.path(),
            &spec(),
            "https://example.com/licence.txt",
            [0xab; 32],
            "test",
        )
        .unwrap_err();
        assert!(matches!(err, DaemonError::State(_)));
    }

    #[test]
    fn env_set_writes_record() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("NAMTOR_ACCEPT_LICENCE", "1");
        ensure_licence_accepted(
            tmp.path(),
            &spec(),
            "https://example.com/licence.txt",
            [0xab; 32],
            "test",
        )
        .unwrap();
        std::env::remove_var("NAMTOR_ACCEPT_LICENCE");

        let path = LicenceRecord::path_for(tmp.path(), "test-model");
        assert!(path.exists());
        let data = std::fs::read_to_string(&path).unwrap();
        let record: LicenceRecord = serde_json::from_str(&data).unwrap();
        assert_eq!(record.model_id, "test-model");
        assert_eq!(record.licence_url, "https://example.com/licence.txt");
        assert_eq!(record.licence_sha256, [0xab; 32]);
        assert_eq!(record.accepted_by, "test");
    }

    #[test]
    fn existing_record_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("NAMTOR_ACCEPT_LICENCE", "1");
        ensure_licence_accepted(
            tmp.path(),
            &spec(),
            "https://example.com/licence.txt",
            [0xcd; 32],
            "test",
        )
        .unwrap();
        std::env::remove_var("NAMTOR_ACCEPT_LICENCE");

        // Second call without env should still succeed (record exists).
        ensure_licence_accepted(
            tmp.path(),
            &spec(),
            "https://example.com/licence.txt",
            [0xcd; 32],
            "test",
        )
        .unwrap();
    }

    #[test]
    fn mismatched_licence_sha256_errors() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("NAMTOR_ACCEPT_LICENCE", "1");
        ensure_licence_accepted(
            tmp.path(),
            &spec(),
            "https://example.com/licence.txt",
            [0xcd; 32],
            "test",
        )
        .unwrap();
        std::env::remove_var("NAMTOR_ACCEPT_LICENCE");

        let err = ensure_licence_accepted(
            tmp.path(),
            &spec(),
            "https://example.com/licence.txt",
            [0xff; 32],
            "test",
        )
        .unwrap_err();
        assert!(matches!(err, DaemonError::State(_)));
    }
}
