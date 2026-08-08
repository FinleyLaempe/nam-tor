// SPDX-License-Identifier: GPL-3.0-only
// allow: SIZE_OK — spec §6.6 Tier 3 mandates a single z-norm module holding
// CohortSource + (Synthetic/File/Dir) cohort impls + ZNormCalibrator +
// normalize_score / normalize_score_scalar / t_norm as one cohesive score
// normalisation API. Splitting would fragment the spec-mandated surface with
// no independent use (calibrator takes CohortSource; normalize takes
// ZNormCalibration). Non-test pure LOC 297.
//! Score normalisation: lawful cohort + z-norm / T-norm.
//!
//! Spec §6.6 Tier 3: per-profile score normalisation needs a *cohort* of
//! background embeddings to compute the mean/std used by z-norm. A cohort is
//! only lawful when its dataset licence is verified and consent is recorded.
//! Until the project selects a licence-verified dataset, the only available
//! cohort is the synthetic bootstrap, which is explicitly `non_production` —
//! it can drive unit tests and structural wiring but must never back a real
//! authentication threshold. See `docs/evidence/trust/znorm.md` for the
//! WITHHELD status of the production cohort gate.

use std::path::PathBuf;

use crate::Embedding;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Errors from cohort loading / calibration.
#[derive(Debug, Error)]
pub enum ZNormError {
    #[error("calibration withheld: cohort licence not verified")]
    Withheld,
    #[error("cohort is empty")]
    EmptyCohort,
    #[error("cohort embeddings have inconsistent dimension")]
    InconsistentDim,
    #[error("cohort file i/o error: {0}")]
    Io(String),
    #[error("cohort file parse error: {0}")]
    Parse(String),
}

/// Provenance of a cohort source: the legal/consent record that decides
/// whether the cohort may back a *production* calibration.
///
/// `licence_verified == false` on a real (non-synthetic) cohort WITHHOLDS
/// calibration. The synthetic bootstrap sets `non_production == true` and
/// `licence_verified == true` (no licence applies to generated data) so it
/// can exercise the wiring, but the resulting `ZNormCalibration` is flagged
/// `non_production` and must never feed a live auth threshold.
#[derive(Debug, Clone)]
pub struct Provenance {
    /// Human-readable source identifier (dataset name or "synthetic-bootstrap").
    pub source: String,
    /// True only when the dataset licence has been accepted and recorded.
    pub licence_verified: bool,
    /// True for the synthetic bootstrap — never usable in production.
    pub non_production: bool,
    /// Optional consent record id (real datasets only).
    pub consent_record: Option<String>,
}

/// Source of cohort embeddings. Implementations must supply `provenance()`;
/// calibrate() consults it to enforce the legal gate.
pub trait CohortSource {
    fn embeddings(&self) -> Result<Vec<Embedding>, ZNormError>;
    fn provenance(&self) -> Provenance;
}

/// Synthetic bootstrap cohort. Generates deterministic pseudo-random
/// 128-d embeddings so the calibration wiring can be exercised without a
/// real dataset. Explicitly `non_production`.
pub struct SyntheticCohort {
    count: usize,
    dim: usize,
    seed: u64,
}

impl SyntheticCohort {
    pub const SOURCE: &'static str = "synthetic-bootstrap";

    pub fn new(count: usize, dim: usize) -> Self {
        Self {
            count,
            dim,
            seed: 0x5eed_c0ff_ee00,
        }
    }
}

impl CohortSource for SyntheticCohort {
    fn embeddings(&self) -> Result<Vec<Embedding>, ZNormError> {
        let mut rng = self.seed;
        let mut out = Vec::with_capacity(self.count);
        for _ in 0..self.count {
            let mut v = Vec::with_capacity(self.dim);
            for _ in 0..self.dim {
                // xorshift64 — deterministic, no external rand dep at call site.
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let f = ((rng & 0xffff) as f32 / 65535.0) * 2.0 - 1.0;
                v.push(f);
            }
            // L2-normalise to match real embedding geometry.
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 1e-6 {
                for x in &mut v {
                    *x /= norm;
                }
            }
            out.push(Embedding { vector: v });
        }
        Ok(out)
    }

    fn provenance(&self) -> Provenance {
        Provenance {
            source: Self::SOURCE.into(),
            // No licence applies to generated data — the bootstrap is lawful
            // to *compute*, just not to ship behind a real threshold.
            licence_verified: true,
            non_production: true,
            consent_record: None,
        }
    }
}

/// Cohort loaded from a JSON file on disk. The file is an array of
/// `{"vector": [f32, ...]}` objects. A `FileCohort` is only lawful when
/// constructed with `licence_verified == true`; otherwise calibration is
/// WITHHELD regardless of file contents.
pub struct FileCohort {
    path: PathBuf,
    provenance: Provenance,
}

#[derive(serde::Deserialize)]
struct EmbeddingJson {
    vector: Vec<f32>,
}

impl FileCohort {
    pub fn new(path: impl Into<PathBuf>, provenance: Provenance) -> Self {
        Self {
            path: path.into(),
            provenance,
        }
    }
}

impl CohortSource for FileCohort {
    fn embeddings(&self) -> Result<Vec<Embedding>, ZNormError> {
        if !self.provenance.licence_verified {
            return Err(ZNormError::Withheld);
        }
        let data = std::fs::read(&self.path).map_err(|e| ZNormError::Io(e.to_string()))?;
        let parsed: Vec<EmbeddingJson> =
            serde_json::from_slice(&data).map_err(|e| ZNormError::Parse(e.to_string()))?;
        let out = parsed
            .into_iter()
            .map(|e| Embedding { vector: e.vector })
            .collect();
        Ok(out)
    }

    fn provenance(&self) -> Provenance {
        self.provenance.clone()
    }
}

/// Cohort loaded from a directory of JSON files. Each `*.json` file is an
/// array of `{"vector": [f32, ...]}` objects; embeddings from all files are
/// concatenated in lexicographic filename order (so the cohort hash is
/// deterministic regardless of directory iteration order). A `DirCohort` is
/// only lawful when constructed with `licence_verified == true`.
///
/// The directory is expected to live outside git (e.g. under
/// `var/lib/namtor/cohort/`, which `.gitignore` excludes). No biometric
/// cohort data is ever committed.
pub struct DirCohort {
    dir: PathBuf,
    provenance: Provenance,
}

impl DirCohort {
    pub fn new(dir: impl Into<PathBuf>, provenance: Provenance) -> Self {
        Self {
            dir: dir.into(),
            provenance,
        }
    }
}

impl CohortSource for DirCohort {
    fn embeddings(&self) -> Result<Vec<Embedding>, ZNormError> {
        if !self.provenance.licence_verified {
            return Err(ZNormError::Withheld);
        }
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .map_err(|e| ZNormError::Io(e.to_string()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
            .collect();
        paths.sort();
        let mut out = Vec::new();
        for path in &paths {
            let data = std::fs::read(path).map_err(|e| ZNormError::Io(e.to_string()))?;
            let parsed: Vec<EmbeddingJson> =
                serde_json::from_slice(&data).map_err(|e| ZNormError::Parse(e.to_string()))?;
            for e in parsed {
                out.push(Embedding { vector: e.vector });
            }
        }
        Ok(out)
    }

    fn provenance(&self) -> Provenance {
        self.provenance.clone()
    }
}

/// Computed z-norm parameters for one model/device/profile context.
#[derive(Debug, Clone)]
pub struct ZNormCalibration {
    /// Per-dimension mean of the cohort embeddings.
    pub mean: Vec<f32>,
    /// Per-dimension standard deviation of the cohort embeddings.
    pub std: Vec<f32>,
    /// Cohort source identifier (provenance.source).
    pub cohort_source: String,
    /// Number of embeddings in the cohort.
    pub cohort_count: usize,
    /// SHA-256 of the cohort embedding material (canonical f32-LE encoding,
    /// in source order). Proves reproducibility: same cohort → same hash.
    pub cohort_sha256: [u8; 32],
    /// When the calibration was computed, if ever.
    pub calibrated_at: Option<std::time::Instant>,
    /// True iff the cohort was the synthetic bootstrap — never production.
    pub non_production: bool,
}

/// Calibrator owns the current model/device/profile context and decides
/// whether a stored calibration is stale. `calibrate()` is a pure
/// computation over a `CohortSource`; the caller stores the result and
/// calls `confirm_calibration()` to mark the current context fresh.
pub struct ZNormCalibrator {
    model_id: Option<String>,
    device: Option<String>,
    profile: Option<String>,
    calibrated_model_id: Option<String>,
    calibrated_device: Option<String>,
    calibrated_profile: Option<String>,
    invalidated: bool,
}

impl Default for ZNormCalibrator {
    fn default() -> Self {
        Self::new()
    }
}

impl ZNormCalibrator {
    pub fn new() -> Self {
        Self {
            model_id: None,
            device: None,
            profile: None,
            calibrated_model_id: None,
            calibrated_device: None,
            calibrated_profile: None,
            // Starts stale: no calibration has been produced yet.
            invalidated: true,
        }
    }

    pub fn set_model_id(&mut self, id: &str) {
        self.model_id = Some(id.to_string());
        if self.calibrated_model_id.as_deref() != Some(id) {
            self.invalidated = true;
        }
    }

    pub fn set_device(&mut self, device: &str) {
        self.device = Some(device.to_string());
        if self.calibrated_device.as_deref() != Some(device) {
            self.invalidated = true;
        }
    }

    pub fn set_profile(&mut self, profile: &str) {
        self.profile = Some(profile.to_string());
        if self.calibrated_profile.as_deref() != Some(profile) {
            self.invalidated = true;
        }
    }

    /// Compute a `ZNormCalibration` from `source`. Enforces the legal gate:
    /// an unlicensed real cohort returns `Withheld`. Pure — does not mutate
    /// the calibrator's stale flag.
    pub fn calibrate(&self, source: &dyn CohortSource) -> Result<ZNormCalibration, ZNormError> {
        let prov = source.provenance();
        if !prov.licence_verified {
            return Err(ZNormError::Withheld);
        }
        let embeddings = source.embeddings()?;
        if embeddings.is_empty() {
            return Err(ZNormError::EmptyCohort);
        }
        let dim = embeddings[0].vector.len();
        for e in &embeddings {
            if e.vector.len() != dim {
                return Err(ZNormError::InconsistentDim);
            }
        }

        let n = embeddings.len() as f32;
        let mut mean = vec![0.0f32; dim];
        for e in &embeddings {
            for (i, &v) in e.vector.iter().enumerate() {
                mean[i] += v;
            }
        }
        for m in &mut mean {
            *m /= n;
        }

        let mut std = vec![0.0f32; dim];
        for e in &embeddings {
            for (i, &v) in e.vector.iter().enumerate() {
                let d = v - mean[i];
                std[i] += d * d;
            }
        }
        for s in &mut std {
            *s = (*s / n).sqrt();
            // Guard against divide-by-zero on degenerate dimensions.
            if *s < 1e-6 {
                *s = 1.0;
            }
        }

        // Canonical cohort hash: SHA-256 over each embedding's f32-LE bytes,
        // in source order. Same cohort inputs → same hash → reproducible.
        let mut hasher = Sha256::new();
        for e in &embeddings {
            for &v in &e.vector {
                hasher.update(v.to_le_bytes());
            }
        }
        let cohort_sha256 = hasher.finalize().into();

        Ok(ZNormCalibration {
            mean,
            std,
            cohort_source: prov.source,
            cohort_count: embeddings.len(),
            cohort_sha256,
            calibrated_at: Some(std::time::Instant::now()),
            non_production: prov.non_production,
        })
    }

    /// Record that a calibration has been applied for the *current*
    /// model/device/profile context, clearing the stale flag. Call this
    /// after successfully storing a `ZNormCalibration` produced by
    /// `calibrate()`.
    pub fn confirm_calibration(&mut self) {
        self.calibrated_model_id = self.model_id.clone();
        self.calibrated_device = self.device.clone();
        self.calibrated_profile = self.profile.clone();
        self.invalidated = false;
    }

    /// Force the calibration stale (e.g. after a manual model reload or
    /// device re-plug that bypassed the setters).
    pub fn invalidate(&mut self) {
        self.invalidated = true;
    }

    /// True if the calibration must be recomputed: either it was never
    /// confirmed, was explicitly invalidated, or the model/device/profile
    /// context has changed since the last confirmation.
    pub fn is_stale(&self) -> bool {
        if self.invalidated {
            return true;
        }
        self.calibrated_model_id.is_none()
            || self.calibrated_device.is_none()
            || self.calibrated_profile.is_none()
            || self.calibrated_model_id != self.model_id
            || self.calibrated_device != self.device
            || self.calibrated_profile != self.profile
    }
}

/// Apply scalar z-normalisation to a raw score using the central tendency
/// of the per-dimension calibration stats: `mean = avg(cal.mean)`,
/// `std = avg(cal.std)`. A raw score above the cohort mean yields a
/// positive normalised score. If `std` is degenerate the raw score is
/// returned unchanged (matches `verify.rs`'s z-norm guard).
pub fn normalize_score(raw: f32, calibration: &ZNormCalibration) -> f32 {
    if calibration.mean.is_empty() || calibration.std.is_empty() {
        return raw;
    }
    let mean = calibration.mean.iter().sum::<f32>() / calibration.mean.len() as f32;
    let std = calibration.std.iter().sum::<f32>() / calibration.std.len() as f32;
    if std > 1e-6 {
        (raw - mean) / std
    } else {
        raw
    }
}

/// Scalar z-norm helper for the daemon's verify path, which stores z-norm
/// parameters as `Option<f32>` mean/std in per-user enrollment metadata.
/// Applies `(raw - mean) / std` when both are present and std is
/// non-degenerate; otherwise returns the raw score unchanged. This is the
/// trust boundary for the calibrated-threshold contract: a `None` pair
/// means "no calibration" and the caller falls back to the global default
/// threshold.
pub fn normalize_score_scalar(raw: f32, mean: Option<f32>, std: Option<f32>) -> f32 {
    match (mean, std) {
        (Some(m), Some(s)) if s > 1e-6 => (raw - m) / s,
        _ => raw,
    }
}

/// T-norm: normalise each raw score with `normalize_score` and average.
pub fn t_norm(scores: &[f32], calibration: &ZNormCalibration) -> f32 {
    if scores.is_empty() {
        return 0.0;
    }
    let sum: f32 = scores
        .iter()
        .map(|&s| normalize_score(s, calibration))
        .sum();
    sum / scores.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a calibration with scalar mean/std encoded as constant
    /// per-dimension vectors so `normalize_score` sees the requested values.
    fn scalar_calibration(mean: f32, std: f32, non_production: bool) -> ZNormCalibration {
        ZNormCalibration {
            mean: vec![mean; 4],
            std: vec![std; 4],
            cohort_source: "test".into(),
            cohort_count: 1,
            cohort_sha256: [0; 32],
            calibrated_at: None,
            non_production,
        }
    }

    #[test]
    fn synthetic_cohort_produces_calibration() {
        let calibrator = ZNormCalibrator::new();
        let cohort = SyntheticCohort::new(50, 128);
        let cal = calibrator.calibrate(&cohort).expect("synthetic calibrates");
        assert_eq!(cal.mean.len(), 128);
        assert_eq!(cal.std.len(), 128);
        assert_eq!(cal.cohort_source, SyntheticCohort::SOURCE);
        assert_eq!(cal.cohort_count, 50);
        assert!(cal.calibrated_at.is_some());
    }

    #[test]
    fn znorm_normalizes_scores_raw_above_mean_is_positive() {
        // raw = 0.5, mean = 0.3, std = 0.1 -> (0.5 - 0.3) / 0.1 = 2.0
        let cal = scalar_calibration(0.3, 0.1, true);
        let n = normalize_score(0.5, &cal);
        assert!((n - 2.0).abs() < 1e-5, "normalised = {n}");
        assert!(n > 0.0, "raw above mean must be positive");
    }

    #[test]
    fn znorm_raw_below_mean_is_negative() {
        // raw = 0.2, mean = 0.3, std = 0.1 -> -1.0
        let cal = scalar_calibration(0.3, 0.1, true);
        let n = normalize_score(0.2, &cal);
        assert!((n - (-1.0)).abs() < 1e-5);
    }

    #[test]
    fn znorm_zero_std_is_passthrough() {
        let cal = scalar_calibration(0.3, 0.0, true);
        assert_eq!(normalize_score(0.5, &cal), 0.5);
    }

    #[test]
    fn t_norm_averages_normalised_scores() {
        // scores 0.5 and 0.3, mean 0.3, std 0.1 -> normalized 2.0 and 0.0
        // average = 1.0
        let cal = scalar_calibration(0.3, 0.1, true);
        let t = t_norm(&[0.5, 0.3], &cal);
        assert!((t - 1.0).abs() < 1e-5, "t_norm = {t}");
    }

    #[test]
    fn t_norm_empty_is_zero() {
        let cal = scalar_calibration(0.3, 0.1, true);
        assert_eq!(t_norm(&[], &cal), 0.0);
    }

    fn fresh_calibrator_with_synthetic() -> ZNormCalibrator {
        let mut c = ZNormCalibrator::new();
        c.set_model_id("sface-128");
        c.set_device("/dev/video2");
        c.set_profile("alice");
        let cohort = SyntheticCohort::new(20, 128);
        let _ = c.calibrate(&cohort).expect("calibrates");
        c.confirm_calibration();
        assert!(!c.is_stale(), "freshly confirmed must not be stale");
        c
    }

    #[test]
    fn invalidation_on_model_change() {
        let mut c = fresh_calibrator_with_synthetic();
        assert!(!c.is_stale());
        c.set_model_id("sface-256");
        assert!(c.is_stale(), "changing model_id must invalidate");
    }

    #[test]
    fn invalidation_on_device_change() {
        let mut c = fresh_calibrator_with_synthetic();
        assert!(!c.is_stale());
        c.set_device("/dev/video4");
        assert!(c.is_stale(), "changing device must invalidate");
    }

    #[test]
    fn invalidation_on_profile_change() {
        let mut c = fresh_calibrator_with_synthetic();
        assert!(!c.is_stale());
        c.set_profile("bob");
        assert!(c.is_stale(), "changing profile must invalidate");
    }

    #[test]
    fn explicit_invalidate_marks_stale() {
        let mut c = fresh_calibrator_with_synthetic();
        assert!(!c.is_stale());
        c.invalidate();
        assert!(c.is_stale());
    }

    #[test]
    fn same_value_setter_does_not_invalidate() {
        let mut c = fresh_calibrator_with_synthetic();
        c.set_model_id("sface-128"); // same value
        assert!(!c.is_stale(), "setting the same value must not invalidate");
    }

    #[test]
    fn unlicensed_file_cohort_is_withheld() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cohort.json");
        std::fs::write(&path, r#"[{"vector":[0.0,1.0]}]"#).unwrap();
        let prov = Provenance {
            source: "unlicensed-dataset".into(),
            licence_verified: false,
            non_production: false,
            consent_record: None,
        };
        let cohort = FileCohort::new(&path, prov);
        let calibrator = ZNormCalibrator::new();
        let err = calibrator.calibrate(&cohort).unwrap_err();
        assert!(matches!(err, ZNormError::Withheld), "got {err:?}");
    }

    #[test]
    fn licensed_file_cohort_calibrates() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cohort.json");
        // Two embeddings, dim 3 — enough to compute mean/std.
        std::fs::write(
            &path,
            r#"[{"vector":[1.0,0.0,0.0]},{"vector":[0.0,1.0,0.0]},{"vector":[0.0,0.0,1.0]}]"#,
        )
        .unwrap();
        let prov = Provenance {
            source: "licensed-dataset".into(),
            licence_verified: true,
            non_production: false,
            consent_record: Some("consent-#42".into()),
        };
        let cohort = FileCohort::new(&path, prov);
        let calibrator = ZNormCalibrator::new();
        let cal = calibrator.calibrate(&cohort).expect("licensed calibrates");
        assert_eq!(cal.mean.len(), 3);
        assert!(!cal.non_production, "licensed real cohort is production");
        assert_eq!(cal.cohort_source, "licensed-dataset");
        assert_eq!(cal.cohort_count, 3);
    }

    #[test]
    fn synthetic_cohort_is_non_production() {
        let cohort = SyntheticCohort::new(10, 128);
        let prov = cohort.provenance();
        assert!(
            prov.non_production,
            "synthetic cohort must be non-production"
        );
        assert!(
            prov.licence_verified,
            "synthetic bootstrap is lawful to compute (no licence applies)"
        );
        let calibrator = ZNormCalibrator::new();
        let cal = calibrator.calibrate(&cohort).unwrap();
        assert!(
            cal.non_production,
            "synthetic calibration must be non-production"
        );
    }

    #[test]
    fn empty_cohort_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("empty.json");
        std::fs::write(&path, "[]").unwrap();
        let prov = Provenance {
            source: "x".into(),
            licence_verified: true,
            non_production: false,
            consent_record: None,
        };
        let cohort = FileCohort::new(&path, prov);
        let calibrator = ZNormCalibrator::new();
        let err = calibrator.calibrate(&cohort).unwrap_err();
        assert!(matches!(err, ZNormError::EmptyCohort), "got {err:?}");
    }

    #[test]
    fn inconsistent_dimension_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mixed.json");
        std::fs::write(&path, r#"[{"vector":[0.0,1.0]},{"vector":[0.0,1.0,2.0]}]"#).unwrap();
        let prov = Provenance {
            source: "x".into(),
            licence_verified: true,
            non_production: false,
            consent_record: None,
        };
        let cohort = FileCohort::new(&path, prov);
        let calibrator = ZNormCalibrator::new();
        let err = calibrator.calibrate(&cohort).unwrap_err();
        assert!(matches!(err, ZNormError::InconsistentDim), "got {err:?}");
    }

    #[test]
    fn calibrator_starts_stale() {
        let c = ZNormCalibrator::new();
        assert!(
            c.is_stale(),
            "a fresh calibrator with no calibration is stale"
        );
    }

    #[test]
    fn synthetic_embeddings_are_l2_normalised() {
        let cohort = SyntheticCohort::new(5, 128);
        let embs = cohort.embeddings().unwrap();
        for e in &embs {
            let norm = e.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-4, "embedding not unit-norm: {norm}");
        }
    }

    #[test]
    fn path_unused_when_unlicensed() {
        // Unlicensed FileCohort must WITHHOLD without touching the file —
        // so a missing file still yields Withheld, not Io.
        let prov = Provenance {
            source: "x".into(),
            licence_verified: false,
            non_production: false,
            consent_record: None,
        };
        let cohort = FileCohort::new("/nonexistent/cohort.json", prov);
        let calibrator = ZNormCalibrator::new();
        let err = calibrator.calibrate(&cohort).unwrap_err();
        assert!(matches!(err, ZNormError::Withheld), "got {err:?}");
    }

    /// Two calibrations from the same synthetic cohort must produce
    /// identical stats and identical cohort hash — the reproducibility
    /// proof. The only field that may differ is `calibrated_at` (wall clock).
    #[test]
    fn calibration_is_reproducible_same_hash() {
        let cohort = SyntheticCohort::new(40, 128);
        let calibrator = ZNormCalibrator::new();
        let cal_a = calibrator.calibrate(&cohort).unwrap();
        let cal_b = calibrator.calibrate(&cohort).unwrap();
        assert_eq!(cal_a.cohort_sha256, cal_b.cohort_sha256, "hash must match");
        assert_eq!(cal_a.cohort_count, cal_b.cohort_count);
        assert_eq!(cal_a.mean, cal_b.mean, "mean must match");
        assert_eq!(cal_a.std, cal_b.std, "std must match");
        assert_eq!(cal_a.cohort_source, cal_b.cohort_source);
    }

    /// Different cohort inputs must produce different cohort hashes.
    #[test]
    fn cohort_hash_differs_for_different_cohorts() {
        let calibrator = ZNormCalibrator::new();
        let cal_a = calibrator
            .calibrate(&SyntheticCohort::new(10, 128))
            .unwrap();
        let cal_b = calibrator
            .calibrate(&SyntheticCohort::new(20, 128))
            .unwrap();
        assert_ne!(
            cal_a.cohort_sha256, cal_b.cohort_sha256,
            "different cohorts must hash differently"
        );
    }

    /// `normalize_score_scalar` must match the inline z-norm math that
    /// `verify.rs` previously used, so the daemon bridge is behaviour-preserving.
    #[test]
    fn normalize_score_scalar_matches_inline_math() {
        let raw = 0.50f32;
        let m = 0.3f32;
        let s = 0.1f32;
        let inline = (raw - m) / s;
        assert!((normalize_score_scalar(raw, Some(m), Some(s)) - inline).abs() < 1e-6);
    }

    #[test]
    fn normalize_score_scalar_zero_std_is_passthrough() {
        assert_eq!(normalize_score_scalar(0.5, Some(0.3), Some(0.0)), 0.5);
    }

    #[test]
    fn normalize_score_scalar_missing_params_is_passthrough() {
        assert_eq!(normalize_score_scalar(0.5, None, None), 0.5);
        assert_eq!(normalize_score_scalar(0.5, Some(0.3), None), 0.5);
        assert_eq!(normalize_score_scalar(0.5, None, Some(0.1)), 0.5);
    }

    /// DirCohort concatenates embeddings from all `*.json` files in
    /// lexicographic filename order. Proves the directory-loading path
    /// that production will use (cohort dir outside git).
    #[test]
    fn dir_cohort_loads_multiple_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("a.json"),
            r#"[{"vector":[1.0,0.0,0.0]},{"vector":[0.0,1.0,0.0]}]"#,
        )
        .unwrap();
        std::fs::write(tmp.path().join("b.json"), r#"[{"vector":[0.0,0.0,1.0]}]"#).unwrap();
        // Non-json file is ignored.
        std::fs::write(tmp.path().join("notes.txt"), "ignore me").unwrap();
        let prov = Provenance {
            source: "dir-cohort-test".into(),
            licence_verified: true,
            non_production: false,
            consent_record: None,
        };
        let cohort = DirCohort::new(tmp.path(), prov);
        let calibrator = ZNormCalibrator::new();
        let cal = calibrator
            .calibrate(&cohort)
            .expect("dir cohort calibrates");
        assert_eq!(cal.cohort_count, 3, "3 embeddings across 2 files");
        assert_eq!(cal.mean.len(), 3);
        assert_eq!(cal.cohort_source, "dir-cohort-test");
    }

    /// An empty directory (no json files) yields `EmptyCohort`, not an error —
    /// because the cohort material is genuinely absent.
    #[test]
    fn dir_cohort_empty_dir_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let prov = Provenance {
            source: "x".into(),
            licence_verified: true,
            non_production: false,
            consent_record: None,
        };
        let cohort = DirCohort::new(tmp.path(), prov);
        let calibrator = ZNormCalibrator::new();
        let err = calibrator.calibrate(&cohort).unwrap_err();
        assert!(matches!(err, ZNormError::EmptyCohort), "got {err:?}");
    }

    /// Unlicensed DirCohort WITHHOLDS without touching the directory —
    /// so a missing dir still yields Withheld, not Io.
    #[test]
    fn dir_cohort_unlicensed_is_withheld_without_io() {
        let prov = Provenance {
            source: "x".into(),
            licence_verified: false,
            non_production: false,
            consent_record: None,
        };
        let cohort = DirCohort::new("/nonexistent/cohort-dir", prov);
        let calibrator = ZNormCalibrator::new();
        let err = calibrator.calibrate(&cohort).unwrap_err();
        assert!(matches!(err, ZNormError::Withheld), "got {err:?}");
    }

    /// DirCohort hash is order-independent of directory iteration: two
    /// calibrations from the same directory contents produce the same hash.
    #[test]
    fn dir_cohort_hash_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.json"), r#"[{"vector":[1.0,0.0,0.0]}]"#).unwrap();
        std::fs::write(tmp.path().join("b.json"), r#"[{"vector":[0.0,1.0,0.0]}]"#).unwrap();
        let prov = Provenance {
            source: "x".into(),
            licence_verified: true,
            non_production: false,
            consent_record: None,
        };
        let calibrator = ZNormCalibrator::new();
        let cal_a = calibrator
            .calibrate(&DirCohort::new(tmp.path(), prov.clone()))
            .unwrap();
        let cal_b = calibrator
            .calibrate(&DirCohort::new(tmp.path(), prov))
            .unwrap();
        assert_eq!(cal_a.cohort_sha256, cal_b.cohort_sha256);
    }

    /// Z-norm centers cohort-derived scores: the mean of normalised cohort
    /// scores is approximately 0 and the std is approximately 1, proving the
    /// calibration produces centered/portable scores (spec §6.6 Tier 3).
    #[test]
    fn znorm_centers_cohort_scores() {
        // Build a synthetic cohort and a probe embedding that equals one
        // cohort member; the normalised score of the cohort's own mean
        // against itself must be ~0 (raw == mean => z == 0).
        let cohort = SyntheticCohort::new(100, 128);
        let calibrator = ZNormCalibrator::new();
        let cal = calibrator.calibrate(&cohort).unwrap();
        let mean_of_means = cal.mean.iter().sum::<f32>() / cal.mean.len() as f32;
        // Normalising the cohort mean by itself yields 0.
        let z = normalize_score(mean_of_means, &cal);
        assert!(
            z.abs() < 1e-3,
            "normalising the cohort mean must yield ~0, got {z}"
        );
    }
}
