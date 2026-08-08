// SPDX-License-Identifier: GPL-3.0-only
//! Model fetcher: download stock ONNX weights, verify sha256, manage on-disk
//! layout. No weights are committed — the fetcher pulls them at first run
//! under the upstream licence (Task 15 records acceptance).
//!
//! On-disk layout:
//! ```text
//! <root>/<model_id>/fp32.onnx
//! <root>/<model_id>/fp32.sha256
//! <root>/<model_id>/int8.onnx
//! <root>/<model_id>/int8.sha256
//! <root>/<model_id>/licence.accepted
//! ```

use std::path::{Path, PathBuf};

use namtor_proto::worker::{ModelSpec, Quant};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::licence::ensure_licence_accepted;
use crate::DaemonError;

/// HTTP download seam. Production uses `ReqwestFetcher`; tests use
/// `FileCopyFetcher` to avoid a real HTTP server.
pub trait HttpFetch {
    fn fetch_to(&self, url: &str, dest: &Path) -> Result<(), DaemonError>;
}

/// `reqwest`-backed HTTP downloader. Feature-gated so CI builds that don't
/// need network can skip the `reqwest` dep.
#[cfg(feature = "network")]
pub struct ReqwestFetcher {
    client: reqwest::blocking::Client,
}

#[cfg(feature = "network")]
impl ReqwestFetcher {
    pub fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
        }
    }
}

#[cfg(feature = "network")]
impl HttpFetch for ReqwestFetcher {
    fn fetch_to(&self, url: &str, dest: &Path) -> Result<(), DaemonError> {
        let mut resp = self
            .client
            .get(url)
            .send()
            .map_err(|e| DaemonError::State(format!("HTTP GET {url}: {e}")))?;
        if !resp.status().is_success() {
            return Err(DaemonError::State(format!(
                "HTTP {url} returned {}",
                resp.status()
            )));
        }
        let mut file = std::fs::File::create(dest)?;
        resp.copy_to(&mut file)
            .map_err(|e| DaemonError::State(format!("download body: {e}")))?;
        Ok(())
    }
}

/// Model fetcher: downloads, verifies, and caches ONNX model files.
pub struct ModelFetcher {
    root: PathBuf,
    http: Box<dyn HttpFetch>,
}

impl ModelFetcher {
    pub fn new(root: &Path, http: Box<dyn HttpFetch>) -> Self {
        Self {
            root: root.to_path_buf(),
            http,
        }
    }

    fn model_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn quant_file(&self, id: &str, quant: Quant) -> PathBuf {
        let name = match quant {
            Quant::Fp32 => "fp32.onnx",
            Quant::Int8 => "int8.onnx",
        };
        self.model_dir(id).join(name)
    }

    fn sha256_file(&self, id: &str, quant: Quant) -> PathBuf {
        let name = match quant {
            Quant::Fp32 => "fp32.sha256",
            Quant::Int8 => "int8.sha256",
        };
        self.model_dir(id).join(name)
    }

    /// Compute SHA-256 of a file.
    fn sha256_of_file(path: &Path) -> Result<[u8; 32], DaemonError> {
        let data = std::fs::read(path)?;
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let hash = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&hash);
        Ok(out)
    }

    /// Fetch a model. Idempotent: if the file exists and sha256 matches,
    /// returns the path without downloading.
    pub fn fetch(&self, spec: &ModelSpec, source_url: &str) -> Result<PathBuf, DaemonError> {
        let dir = self.model_dir(&spec.id);
        std::fs::create_dir_all(&dir)?;

        let final_path = self.quant_file(&spec.id, spec.quant);
        let sha_path = self.sha256_file(&spec.id, spec.quant);

        // Idempotent: file exists + sha256 matches → skip download.
        if final_path.exists() {
            let actual = Self::sha256_of_file(&final_path)?;
            if actual == spec.sha256 {
                return Ok(final_path);
            }
            // Hash mismatch — re-download.
            std::fs::remove_file(&final_path)?;
        }

        // Download to .tmp, verify, rename. Clean up .tmp on fetch failure so
        // a truncated download never masquerades as a valid model on the next
        // start.
        let tmp_path = final_path.with_extension("onnx.tmp");
        if let Err(e) = self.http.fetch_to(source_url, &tmp_path) {
            std::fs::remove_file(&tmp_path).ok();
            return Err(e);
        }

        let actual = Self::sha256_of_file(&tmp_path)?;
        if actual != spec.sha256 {
            std::fs::remove_file(&tmp_path).ok();
            return Err(DaemonError::State(format!(
                "sha256 mismatch for model {} ({:?}): expected {}, got {}",
                spec.id,
                spec.quant,
                hex(&spec.sha256),
                hex(&actual)
            )));
        }

        std::fs::rename(&tmp_path, &final_path)?;
        std::fs::write(&sha_path, hex(&spec.sha256).as_bytes())?;
        Ok(final_path)
    }

    /// Ensure an INT8 variant of the model exists. If `int8.onnx` is already
    /// present, returns its path. Otherwise, attempts to run the committed
    /// `ml/recipes/quantize_int8.py` script to generate it from the FP32
    /// source. If Python/onnxruntime is unavailable, returns an error so the
    /// caller can fall back to FP32 (and report degraded mode).
    pub fn ensure_int8(&self, fp32_spec: &ModelSpec) -> Result<PathBuf, DaemonError> {
        let int8_path = self.quant_file(&fp32_spec.id, Quant::Int8);
        if int8_path.exists() {
            return Ok(int8_path);
        }

        let fp32_path = self.quant_file(&fp32_spec.id, Quant::Fp32);
        if !fp32_path.exists() {
            return Err(DaemonError::State(format!(
                "FP32 model not fetched for {}; cannot quantize",
                fp32_spec.id
            )));
        }

        let python = std::env::var("NAMTOR_PYTHON").unwrap_or_else(|_| "python3".into());
        let recipe = std::env::var("NAMTOR_QUANTIZE_SCRIPT")
            .unwrap_or_else(|_| "ml/recipes/quantize_int8.py".into());

        let output = std::process::Command::new(&python)
            .arg(&recipe)
            .arg("--fp32")
            .arg(&fp32_path)
            .arg("--out")
            .arg(&int8_path)
            .output()
            .map_err(|e| {
                DaemonError::State(format!(
                    "failed to run {python} {recipe}: {e}; \
                     install Python + onnxruntime or use FP32"
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DaemonError::State(format!(
                "INT8 quantization failed: {stderr}; \
                 install Python + onnxruntime or use FP32"
            )));
        }

        if !int8_path.exists() {
            return Err(DaemonError::State(
                "quantize_int8.py did not produce int8.onnx".into(),
            ));
        }

        Ok(int8_path)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// Model manifest — `packaging/model-manifest.toml`, enforced at startup.
// allow: SIZE_OK — the manifest subsystem (types + validate_against_manifest)
// is a distinct responsibility from ModelFetcher and belongs in a sibling
// `manifest.rs` module, but extracting it requires `pub mod manifest;` in
// lib.rs, which is outside this task's allowed file set. Follow-up: extract
// once lib.rs is in scope.

/// Redistribution policy declared in the model manifest.
///
/// `Redistributable` models may be shipped in distro packages. `Restricted`
/// models (e.g. weights derived from a research-only dataset) may not be
/// redistributed and require explicit operator authorisation to load even
/// locally — the daemon blocks them unless `ManifestGate::allow_restricted`
/// is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedistributionPolicy {
    Redistributable,
    Restricted,
}

/// Target architecture for a manifest model. Most ONNX models are
/// arch-independent (`Both`); INT8-quantised variants may differ per arch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetArch {
    X86_64,
    Aarch64,
    Both,
}

/// One I/O tensor of a model's interface contract. Lets a consumer verify a
/// model matches the expected interface without loading it (model-substitution
/// detection, INT8/FP32 dispatch — task 19).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TensorIo {
    pub shape: Vec<u64>,
    pub dtype: String,
    /// Short pre-processing tag (e.g. `"bgr_sub_mean_104_177_123"`). Absent
    /// on output tensors.
    pub normalisation: Option<String>,
}

/// Input + output tensor contract for a model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TensorContract {
    pub input: TensorIo,
    pub output: TensorIo,
}

/// A quantisation variant of a model: the file actually deployed on disk.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelVariant {
    pub quant: Quant,
    pub source_url: String,
    /// Hex sha256 of the variant's ONNX file.
    pub sha256: String,
}

/// One model entry in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub licence_url: String,
    /// Hex sha256 of the licence text the operator reviews at acceptance.
    pub licence_sha256: String,
    pub redistribution_policy: RedistributionPolicy,
    pub target_arch: TargetArch,
    pub tensor_contract: TensorContract,
    pub variants: Vec<ModelVariant>,
}

impl ModelEntry {
    /// Find the variant for a given quantisation mode.
    pub fn variant(&self, quant: Quant) -> Option<&ModelVariant> {
        self.variants.iter().find(|v| v.quant == quant)
    }
}

/// Parsed `packaging/model-manifest.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelManifest {
    pub manifest_version: u32,
    pub models: Vec<ModelEntry>,
}

impl ModelManifest {
    /// Parse a model manifest from a TOML file on disk.
    pub fn parse(path: &Path) -> Result<Self, DaemonError> {
        let data = std::fs::read_to_string(path)?;
        toml::from_str(&data).map_err(|e| {
            DaemonError::State(format!("model manifest parse {}: {e}", path.display()))
        })
    }

    /// Find the entry for a model id.
    pub fn find(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }
}

/// Authorisation context for manifest validation. Bundled so the validation
/// method stays under the parameter budget and so the daemon can construct
/// one gate at startup (e.g. deriving `allow_restricted` from env/config once)
/// and reuse it across every model.
#[derive(Debug, Clone)]
pub struct ManifestGate<'a> {
    pub manifest: &'a ModelManifest,
    /// User/account recording the licence acceptance (stamped into
    /// `licence.accepted`).
    pub accepted_by: &'a str,
    /// If false, `redistribution_policy = "restricted"` models block startup.
    pub allow_restricted: bool,
}

/// Parse a 64-char hex string into a 32-byte array.
fn parse_hex32(s: &str) -> Result<[u8; 32], DaemonError> {
    if s.len() != 64 {
        return Err(DaemonError::State(format!(
            "expected 64-char hex sha256, got {len} chars",
            len = s.len()
        )));
    }
    let mut out = [0u8; 32];
    let bytes = s.as_bytes();
    for (i, chunk) in bytes.chunks(2).enumerate() {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8, DaemonError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(DaemonError::State(format!(
            "invalid hex digit {b:?} in sha256"
        ))),
    }
}

impl ModelFetcher {
    /// Validate a deployed model against the manifest before the worker loads
    /// it. Three gates, all fail-closed:
    ///
    /// 1. **Tampered** — the on-disk file's sha256 must equal the manifest
    ///    variant's sha256.
    /// 2. **Unaccepted** — the upstream licence must be accepted (on-disk
    ///    `licence.accepted` record whose sha256 matches the manifest).
    /// 3. **Restricted** — `redistribution_policy = "restricted"` models
    ///    require `gate.allow_restricted = true`.
    ///
    /// If all gates pass, returns the validated on-disk path. A valid offline
    /// cache (every model present + accepted) proceeds with no network access.
    pub fn validate_against_manifest(
        &self,
        gate: &ManifestGate,
        spec: &ModelSpec,
    ) -> Result<PathBuf, DaemonError> {
        let entry = gate.manifest.find(&spec.id).ok_or_else(|| {
            DaemonError::State(format!("model {} not present in manifest", spec.id))
        })?;

        let variant = entry.variant(spec.quant).ok_or_else(|| {
            DaemonError::State(format!(
                "no {:?} variant for model {} in manifest",
                spec.quant, spec.id
            ))
        })?;

        let expected = parse_hex32(&variant.sha256)?;
        let path = self.quant_file(&spec.id, spec.quant);
        if !path.exists() {
            return Err(DaemonError::State(format!(
                "model file missing: {}",
                path.display()
            )));
        }
        let actual = Self::sha256_of_file(&path)?;
        if actual != expected {
            return Err(DaemonError::State(format!(
                "tampered model {} ({:?}): sha256 mismatch — expected {}, got {}",
                spec.id,
                spec.quant,
                variant.sha256,
                hex(&actual)
            )));
        }

        let licence_sha = parse_hex32(&entry.licence_sha256)?;
        ensure_licence_accepted(
            &self.root,
            spec,
            &entry.licence_url,
            licence_sha,
            gate.accepted_by,
        )?;

        if entry.redistribution_policy == RedistributionPolicy::Restricted && !gate.allow_restricted
        {
            return Err(DaemonError::State(format!(
                "restricted model {} requires explicit authorisation \
                 (ManifestGate.allow_restricted)",
                spec.id
            )));
        }

        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Copies a local file instead of downloading over HTTP.
    struct FileCopyFetcher {
        source: PathBuf,
    }

    impl HttpFetch for FileCopyFetcher {
        fn fetch_to(&self, _url: &str, dest: &Path) -> Result<(), DaemonError> {
            std::fs::copy(&self.source, dest)?;
            Ok(())
        }
    }

    fn make_blob(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(data).unwrap();
        path
    }

    fn sha256_bytes(data: &[u8]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(data);
        let hash = h.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&hash);
        out
    }

    #[test]
    fn fetch_downloads_and_verifies() {
        let tmp = tempfile::tempdir().unwrap();
        let blob_data = b"hello world model weights";
        let blob = make_blob(tmp.path(), "model.bin", blob_data);
        let expected = sha256_bytes(blob_data);

        let root = tmp.path().join("models");
        let fetcher = ModelFetcher::new(&root, Box::new(FileCopyFetcher { source: blob }));

        let spec = ModelSpec {
            id: "test-model".into(),
            quant: Quant::Fp32,
            sha256: expected,
        };
        let path = fetcher.fetch(&spec, "file://local").unwrap();
        assert!(path.exists());
        assert_eq!(path, root.join("test-model/fp32.onnx"));
        assert!(root.join("test-model/fp32.sha256").exists());
    }

    #[test]
    fn fetch_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let blob_data = b"idempotent model";
        let blob = make_blob(tmp.path(), "model.bin", blob_data);
        let expected = sha256_bytes(blob_data);

        let root = tmp.path().join("models");
        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: blob.clone(),
            }),
        );

        let spec = ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: expected,
        };
        let p1 = fetcher.fetch(&spec, "file://local").unwrap();
        let p2 = fetcher.fetch(&spec, "file://local").unwrap();
        assert_eq!(p1, p2);

        // The source blob should only have been read once (copy count = 1).
        // Since we don't track call count, verify the file is unchanged.
        let data = std::fs::read(&p1).unwrap();
        assert_eq!(data, blob_data);
    }

    #[test]
    fn fetch_rejects_tampered_sha256() {
        let tmp = tempfile::tempdir().unwrap();
        let blob_data = b"actual weights";
        let blob = make_blob(tmp.path(), "model.bin", blob_data);
        let wrong_hash = [0xff; 32];

        let root = tmp.path().join("models");
        let fetcher = ModelFetcher::new(&root, Box::new(FileCopyFetcher { source: blob }));

        let spec = ModelSpec {
            id: "tampered".into(),
            quant: Quant::Fp32,
            sha256: wrong_hash,
        };
        let err = fetcher.fetch(&spec, "file://local").unwrap_err();
        assert!(matches!(err, DaemonError::State(_)));
        // .tmp file should be cleaned up.
        assert!(!root.join("tampered/fp32.onnx.tmp").exists());
        assert!(!root.join("tampered/fp32.onnx").exists());
    }

    /// `HttpFetch` impl that writes a partial file then fails — simulates a
    /// network drop mid-download leaving a truncated `.tmp` on disk.
    struct FailingFetcher {
        partial: Vec<u8>,
    }

    impl HttpFetch for FailingFetcher {
        fn fetch_to(&self, _url: &str, dest: &Path) -> Result<(), DaemonError> {
            std::fs::write(dest, &self.partial)?;
            Err(DaemonError::State("simulated network drop".into()))
        }
    }

    #[test]
    fn fetch_failure_cleans_partial_tmp() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FailingFetcher {
                partial: b"partial download body".to_vec(),
            }),
        );
        let spec = ModelSpec {
            id: "netdrop".into(),
            quant: Quant::Fp32,
            sha256: [0xaa; 32],
        };
        let err = fetcher.fetch(&spec, "file://local").unwrap_err();
        assert!(matches!(err, DaemonError::State(_)));
        // No partial .tmp must survive a failed fetch — a truncated file
        // masquerading as a valid model is a production-safety violation.
        assert!(
            !root.join("netdrop/fp32.onnx.tmp").exists(),
            ".tmp left behind after fetch failure"
        );
        assert!(
            !root.join("netdrop/fp32.onnx").exists(),
            ".onnx created despite fetch failure"
        );
    }

    #[test]
    fn fetch_redownloads_on_hash_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let blob_data = b"correct weights v2";
        let blob = make_blob(tmp.path(), "model.bin", blob_data);
        let expected = sha256_bytes(blob_data);

        let root = tmp.path().join("models");
        let fetcher = ModelFetcher::new(&root, Box::new(FileCopyFetcher { source: blob }));

        // Pre-place a stale file with wrong content.
        let dir = root.join("m");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fp32.onnx"), b"stale wrong content").unwrap();

        let spec = ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: expected,
        };
        let path = fetcher.fetch(&spec, "file://local").unwrap();
        let data = std::fs::read(&path).unwrap();
        assert_eq!(data, blob_data);
    }

    #[test]
    fn ensure_int8_returns_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let dir = root.join("m");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("int8.onnx"), b"int8 weights").unwrap();

        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: tmp.path().join("nonexistent"),
            }),
        );
        let spec = ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        };
        let path = fetcher.ensure_int8(&spec).unwrap();
        assert_eq!(path, dir.join("int8.onnx"));
    }

    #[test]
    fn ensure_int8_errors_without_python() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let dir = root.join("m");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fp32.onnx"), b"fp32 weights").unwrap();

        std::env::set_var("NAMTOR_PYTHON", "/nonexistent/python3");
        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: tmp.path().join("nonexistent"),
            }),
        );
        let spec = ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        };
        let err = fetcher.ensure_int8(&spec).unwrap_err();
        assert!(matches!(err, DaemonError::State(_)));
        std::env::remove_var("NAMTOR_PYTHON");
    }

    #[test]
    fn ensure_int8_errors_without_fp32_source() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        std::fs::create_dir_all(root.join("m")).unwrap();

        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: tmp.path().join("nonexistent"),
            }),
        );
        let spec = ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        };
        let err = fetcher.ensure_int8(&spec).unwrap_err();
        assert!(matches!(err, DaemonError::State(_)));
    }

    // --- model manifest validation ----------------------------------------

    use crate::licence::LicenceRecord;

    /// Write a model file at the fetcher's on-disk layout path.
    fn write_model_file(root: &Path, id: &str, quant: Quant, data: &[u8]) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let name = match quant {
            Quant::Fp32 => "fp32.onnx",
            Quant::Int8 => "int8.onnx",
        };
        std::fs::write(dir.join(name), data).unwrap();
    }

    /// Pre-place a valid licence.accepted record (no env var needed).
    fn write_licence_record(root: &Path, id: &str, licence_url: &str, licence_sha: [u8; 32]) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let record = LicenceRecord {
            model_id: id.into(),
            licence_url: licence_url.into(),
            licence_sha256: licence_sha,
            accepted_at_us: 1_700_000_000_000_000,
            accepted_by: "test".into(),
        };
        let json = serde_json::to_string_pretty(&record).unwrap();
        std::fs::write(dir.join("licence.accepted"), json).unwrap();
    }

    fn manifest_with(
        id: &str,
        sha_hex: &str,
        policy: RedistributionPolicy,
        licence_sha_hex: &str,
    ) -> ModelManifest {
        ModelManifest {
            manifest_version: 1,
            models: vec![ModelEntry {
                id: id.into(),
                licence_url: "https://example.com/licence.txt".into(),
                licence_sha256: licence_sha_hex.into(),
                redistribution_policy: policy,
                target_arch: TargetArch::Both,
                tensor_contract: TensorContract {
                    input: TensorIo {
                        shape: vec![1, 112, 112, 3],
                        dtype: "f32".into(),
                        normalisation: Some("none".into()),
                    },
                    output: TensorIo {
                        shape: vec![1, 128],
                        dtype: "f32".into(),
                        normalisation: None,
                    },
                },
                variants: vec![ModelVariant {
                    quant: Quant::Fp32,
                    source_url: "file://local".into(),
                    sha256: sha_hex.into(),
                }],
            }],
        }
    }

    const LICENCE_SHA_HEX: &str =
        "91f6f2043c607e9db392ca6b4fafd56825a2d5ca741cb3a56d8f817d8d9a9753";

    fn licence_sha_bytes() -> [u8; 32] {
        parse_hex32(LICENCE_SHA_HEX).unwrap()
    }

    #[test]
    fn manifest_validate_ok_offline_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"real fp32 weights for offline use";
        let sha = sha256_bytes(weights);
        let sha_hex = hex(&sha);
        write_model_file(&root, "m", Quant::Fp32, weights);
        write_licence_record(
            &root,
            "m",
            "https://example.com/licence.txt",
            licence_sha_bytes(),
        );

        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: tmp.path().join("nonexistent"),
            }),
        );
        let manifest = manifest_with(
            "m",
            &sha_hex,
            RedistributionPolicy::Redistributable,
            LICENCE_SHA_HEX,
        );
        let gate = ManifestGate {
            manifest: &manifest,
            accepted_by: "test",
            allow_restricted: false,
        };
        let spec = ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: sha,
        };
        let path = fetcher.validate_against_manifest(&gate, &spec).unwrap();
        assert!(path.exists());
        assert_eq!(path, root.join("m/fp32.onnx"));
    }

    #[test]
    fn manifest_validate_blocks_tampered_sha256_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"actual on-disk weights";
        write_model_file(&root, "m", Quant::Fp32, weights);
        write_licence_record(
            &root,
            "m",
            "https://example.com/licence.txt",
            licence_sha_bytes(),
        );

        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: tmp.path().join("nonexistent"),
            }),
        );
        // Manifest declares a sha256 that does NOT match the on-disk file.
        let wrong_hex = hex(&[0xff; 32]);
        let manifest = manifest_with(
            "m",
            &wrong_hex,
            RedistributionPolicy::Redistributable,
            LICENCE_SHA_HEX,
        );
        let gate = ManifestGate {
            manifest: &manifest,
            accepted_by: "test",
            allow_restricted: false,
        };
        let spec = ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: [0xff; 32],
        };
        let err = fetcher.validate_against_manifest(&gate, &spec).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("tampered"), "msg = {msg}");
        assert!(msg.contains("sha256 mismatch"), "msg = {msg}");
    }

    #[test]
    fn manifest_validate_blocks_unaccepted_licence() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"weights whose sha matches the manifest";
        let sha = sha256_bytes(weights);
        let sha_hex = hex(&sha);
        write_model_file(&root, "m", Quant::Fp32, weights);
        // No licence.accepted record placed.
        std::env::remove_var("NAMTOR_ACCEPT_LICENCE");

        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: tmp.path().join("nonexistent"),
            }),
        );
        let manifest = manifest_with(
            "m",
            &sha_hex,
            RedistributionPolicy::Redistributable,
            LICENCE_SHA_HEX,
        );
        let gate = ManifestGate {
            manifest: &manifest,
            accepted_by: "test",
            allow_restricted: false,
        };
        let spec = ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: sha,
        };
        let err = fetcher.validate_against_manifest(&gate, &spec).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("licence not accepted"), "msg = {msg}");
    }

    #[test]
    fn manifest_validate_blocks_restricted_without_authorisation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"restricted weights";
        let sha = sha256_bytes(weights);
        let sha_hex = hex(&sha);
        write_model_file(&root, "m", Quant::Fp32, weights);
        write_licence_record(
            &root,
            "m",
            "https://example.com/licence.txt",
            licence_sha_bytes(),
        );

        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: tmp.path().join("nonexistent"),
            }),
        );
        let manifest = manifest_with(
            "m",
            &sha_hex,
            RedistributionPolicy::Restricted,
            LICENCE_SHA_HEX,
        );
        let gate = ManifestGate {
            manifest: &manifest,
            accepted_by: "test",
            allow_restricted: false,
        };
        let spec = ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: sha,
        };
        let err = fetcher.validate_against_manifest(&gate, &spec).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("restricted"), "msg = {msg}");
        assert!(msg.contains("authorisation"), "msg = {msg}");
    }

    #[test]
    fn manifest_validate_allows_restricted_with_authorisation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"restricted weights ok";
        let sha = sha256_bytes(weights);
        let sha_hex = hex(&sha);
        write_model_file(&root, "m", Quant::Fp32, weights);
        write_licence_record(
            &root,
            "m",
            "https://example.com/licence.txt",
            licence_sha_bytes(),
        );

        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: tmp.path().join("nonexistent"),
            }),
        );
        let manifest = manifest_with(
            "m",
            &sha_hex,
            RedistributionPolicy::Restricted,
            LICENCE_SHA_HEX,
        );
        let gate = ManifestGate {
            manifest: &manifest,
            accepted_by: "test",
            allow_restricted: true,
        };
        let spec = ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: sha,
        };
        let path = fetcher.validate_against_manifest(&gate, &spec).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn manifest_validate_blocks_unknown_model() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: tmp.path().join("nonexistent"),
            }),
        );
        let manifest = ModelManifest {
            manifest_version: 1,
            models: vec![],
        };
        let gate = ManifestGate {
            manifest: &manifest,
            accepted_by: "test",
            allow_restricted: false,
        };
        let spec = ModelSpec {
            id: "ghost".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        };
        let err = fetcher.validate_against_manifest(&gate, &spec).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not present in manifest"), "msg = {msg}");
    }

    #[test]
    fn manifest_validate_blocks_missing_variant() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"fp32 only weights";
        let sha = sha256_bytes(weights);
        let sha_hex = hex(&sha);
        write_model_file(&root, "m", Quant::Fp32, weights);
        write_licence_record(
            &root,
            "m",
            "https://example.com/licence.txt",
            licence_sha_bytes(),
        );

        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: tmp.path().join("nonexistent"),
            }),
        );
        // Manifest declares only an fp32 variant; ask for int8.
        let manifest = manifest_with(
            "m",
            &sha_hex,
            RedistributionPolicy::Redistributable,
            LICENCE_SHA_HEX,
        );
        let gate = ManifestGate {
            manifest: &manifest,
            accepted_by: "test",
            allow_restricted: false,
        };
        let spec = ModelSpec {
            id: "m".into(),
            quant: Quant::Int8,
            sha256: [0; 32],
        };
        let err = fetcher.validate_against_manifest(&gate, &spec).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no Int8 variant"), "msg = {msg}");
    }

    #[test]
    fn manifest_parse_packaging_file_loads_three_models() {
        let path = Path::new("packaging/model-manifest.toml");
        if !path.exists() {
            eprintln!("skipping — manifest not found at repo root (run from repo root)");
            return;
        }
        let manifest = ModelManifest::parse(path).unwrap();
        assert_eq!(manifest.manifest_version, 1);
        let ids: Vec<&str> = manifest.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["yunet_2023mar", "sface_2021dec", "minifasnet_pad"]
        );
        for entry in &manifest.models {
            assert!(
                entry.variant(Quant::Fp32).is_some(),
                "model {} missing fp32 variant",
                entry.id
            );
            assert!(!entry.licence_sha256.is_empty());
            assert!(!entry.variants.is_empty());
        }
    }

    #[test]
    fn parse_hex32_roundtrips_and_rejects_bad_input() {
        let bytes = [0xab; 32];
        let s = hex(&bytes);
        assert_eq!(parse_hex32(&s).unwrap(), bytes);
        assert!(parse_hex32("ab").is_err());
        assert!(parse_hex32(&"z".repeat(64)).is_err());
        assert!(parse_hex32(&"ab".repeat(31)).is_err());
    }
}
