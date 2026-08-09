// SPDX-License-Identifier: GPL-3.0-only
//! Typed model admission policy — replaces blanket licence acceptance.
//!
//! Admission is driven by the model's `RedistributionPolicy` (declared in
//! `packaging/model-manifest.toml`):
//!
//! - **Redistributable** (MIT, Apache-2.0, self-trained): admitted without an
//!   acceptance record. Hash/provenance verification (done by the caller via
//!   the manifest sha256 gate) is sufficient. No env var, no `licence.accepted`
//!   file required.
//! - **Restricted** (research-only weights, dataset-derived): require an
//!   explicit acceptance record on disk. The daemon checks the record but
//!   never writes it — the record is created by [`write_acceptance_record`],
//!   called from the CLI or an explicit operator acceptance flow. The write
//!   is atomic (temp + rename) so a truncated record never masquerades as
//!   valid.
//!
//! Unknown or mismatched models fail closed — the daemon never loads an
//! unadmitted model.

use std::path::Path;

use namtor_proto::worker::ModelSpec;
use serde::{Deserialize, Serialize};

use crate::models::RedistributionPolicy;
use crate::DaemonError;

/// A recorded licence acceptance for one restricted model.
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

    fn tmp_path_for(root: &Path, id: &str) -> std::path::PathBuf {
        root.join(id).join("licence.accepted.tmp")
    }
}

/// Admit a model based on its redistribution policy.
///
/// - `Redistributable` — always `Ok` (hash/provenance already verified by the
///   caller). No acceptance record or env var needed.
/// - `Restricted` — requires an acceptance record whose `licence_sha256`
///   matches. The daemon never writes the record; use
///   [`write_acceptance_record`] for that. Without a record, fails closed.
pub fn ensure_model_admitted(
    root: &Path,
    spec: &ModelSpec,
    policy: RedistributionPolicy,
    licence_url: &str,
    licence_sha256: [u8; 32],
    accepted_by: &str,
) -> Result<(), DaemonError> {
    match policy {
        RedistributionPolicy::Redistributable => Ok(()),
        RedistributionPolicy::Restricted => {
            ensure_restricted_accepted(root, spec, licence_url, licence_sha256, accepted_by)
        }
    }
}

/// Restricted-model acceptance: check the on-disk record. Fails closed if no
/// record exists or the `licence_sha256` mismatches. The daemon never writes
/// the record — [`write_acceptance_record`] is the sole writer.
fn ensure_restricted_accepted(
    root: &Path,
    spec: &ModelSpec,
    licence_url: &str,
    licence_sha256: [u8; 32],
    _accepted_by: &str,
) -> Result<(), DaemonError> {
    let path = LicenceRecord::path_for(root, &spec.id);
    if !path.exists() {
        return Err(DaemonError::State(format!(
            "licence not accepted for model {}; \
             run 'namtor-cli models accept {0}' to accept",
            spec.id
        )));
    }

    let data = std::fs::read_to_string(&path)?;
    let record: LicenceRecord = serde_json::from_str(&data)
        .map_err(|e| DaemonError::State(format!("licence record: {e}")))?;
    if record.licence_sha256 != licence_sha256 {
        return Err(DaemonError::State(format!(
            "licence sha256 mismatch for model {}",
            spec.id
        )));
    }
    let _ = licence_url;
    Ok(())
}

/// Write an acceptance record atomically (temp + rename). Called by the CLI
/// or an explicit operator acceptance flow — never by the daemon's admission
/// check. A truncated `.tmp` never masquerades as a valid record.
pub fn write_acceptance_record(
    root: &Path,
    spec: &ModelSpec,
    licence_url: &str,
    licence_sha256: [u8; 32],
    accepted_by: &str,
) -> Result<(), DaemonError> {
    let dir = root.join(&spec.id);
    std::fs::create_dir_all(&dir)?;

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

    let tmp_path = LicenceRecord::tmp_path_for(root, &spec.id);
    if let Err(e) = std::fs::write(&tmp_path, &json) {
        std::fs::remove_file(&tmp_path).ok();
        return Err(e.into());
    }
    std::fs::rename(&tmp_path, LicenceRecord::path_for(root, &spec.id))?;
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

    fn write_record(root: &Path, id: &str, url: &str, sha: [u8; 32]) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let record = LicenceRecord {
            model_id: id.into(),
            licence_url: url.into(),
            licence_sha256: sha,
            accepted_at_us: 1_700_000_000_000_000,
            accepted_by: "test".into(),
        };
        let json = serde_json::to_string_pretty(&record).unwrap();
        std::fs::write(dir.join("licence.accepted"), json).unwrap();
    }

    // --- redistributable: no record, no env var, admits -----------------

    #[test]
    fn redistributable_admits_without_record() {
        let tmp = tempfile::tempdir().unwrap();
        // No licence.accepted file placed. Redistributable admits purely on
        // hash/provenance (verified by the caller), not on a record.
        ensure_model_admitted(
            tmp.path(),
            &spec(),
            RedistributionPolicy::Redistributable,
            "https://example.com/licence.txt",
            [0xab; 32],
            "test",
        )
        .unwrap();
        // No record should have been written.
        assert!(!LicenceRecord::path_for(tmp.path(), "test-model").exists());
    }

    // --- restricted: no record → denied ---------------------------------

    #[test]
    fn restricted_denied_without_record() {
        let tmp = tempfile::tempdir().unwrap();
        let err = ensure_model_admitted(
            tmp.path(),
            &spec(),
            RedistributionPolicy::Restricted,
            "https://example.com/licence.txt",
            [0xab; 32],
            "test",
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("licence not accepted"), "msg = {msg}");
    }

    // --- restricted: matching record → admitted -------------------------

    #[test]
    fn restricted_admits_with_matching_record() {
        let tmp = tempfile::tempdir().unwrap();
        write_record(
            tmp.path(),
            "test-model",
            "https://example.com/licence.txt",
            [0xcd; 32],
        );
        ensure_model_admitted(
            tmp.path(),
            &spec(),
            RedistributionPolicy::Restricted,
            "https://example.com/licence.txt",
            [0xcd; 32],
            "test",
        )
        .unwrap();
    }

    // --- restricted: mismatched record → denied -------------------------

    #[test]
    fn restricted_denied_with_mismatched_record() {
        let tmp = tempfile::tempdir().unwrap();
        write_record(
            tmp.path(),
            "test-model",
            "https://example.com/licence.txt",
            [0xcd; 32],
        );
        let err = ensure_model_admitted(
            tmp.path(),
            &spec(),
            RedistributionPolicy::Restricted,
            "https://example.com/licence.txt",
            [0xff; 32],
            "test",
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("sha256 mismatch"), "msg = {msg}");
    }

    // --- restricted: atomic record write --------------------------------

    #[test]
    fn restricted_atomic_write_with_explicit_accept() {
        let tmp = tempfile::tempdir().unwrap();
        // No record exists. Write one atomically.
        write_acceptance_record(
            tmp.path(),
            &spec(),
            "https://example.com/licence.txt",
            [0xab; 32],
            "operator",
        )
        .unwrap();

        // Final record exists and parses.
        let path = LicenceRecord::path_for(tmp.path(), "test-model");
        assert!(path.exists(), "licence.accepted not written");
        // Temp file must not survive the atomic rename.
        let tmp_path = LicenceRecord::tmp_path_for(tmp.path(), "test-model");
        assert!(!tmp_path.exists(), "licence.accepted.tmp left behind");

        let data = std::fs::read_to_string(&path).unwrap();
        let record: LicenceRecord = serde_json::from_str(&data).unwrap();
        assert_eq!(record.model_id, "test-model");
        assert_eq!(record.licence_url, "https://example.com/licence.txt");
        assert_eq!(record.licence_sha256, [0xab; 32]);
        assert_eq!(record.accepted_by, "operator");
    }

    // --- restricted: write then admit round-trip ------------------------

    #[test]
    fn restricted_write_then_admit_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        // No record → denied.
        let err = ensure_model_admitted(
            tmp.path(),
            &spec(),
            RedistributionPolicy::Restricted,
            "https://example.com/licence.txt",
            [0xcd; 32],
            "test",
        )
        .unwrap_err();
        assert!(err.to_string().contains("licence not accepted"));

        // Write the record → admitted.
        write_acceptance_record(
            tmp.path(),
            &spec(),
            "https://example.com/licence.txt",
            [0xcd; 32],
            "operator",
        )
        .unwrap();
        ensure_model_admitted(
            tmp.path(),
            &spec(),
            RedistributionPolicy::Restricted,
            "https://example.com/licence.txt",
            [0xcd; 32],
            "test",
        )
        .unwrap();
    }

    // --- restricted: existing record is idempotent ----------------------

    #[test]
    fn restricted_existing_record_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        write_record(
            tmp.path(),
            "test-model",
            "https://example.com/licence.txt",
            [0xcd; 32],
        );
        // Multiple checks succeed (idempotent — no writes, no env var).
        ensure_model_admitted(
            tmp.path(),
            &spec(),
            RedistributionPolicy::Restricted,
            "https://example.com/licence.txt",
            [0xcd; 32],
            "test",
        )
        .unwrap();
        ensure_model_admitted(
            tmp.path(),
            &spec(),
            RedistributionPolicy::Restricted,
            "https://example.com/licence.txt",
            [0xcd; 32],
            "test",
        )
        .unwrap();
    }
}
