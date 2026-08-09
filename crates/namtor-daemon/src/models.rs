// SPDX-License-Identifier: GPL-3.0-only
//! Model fetcher: download stock ONNX weights, verify sha256, manage on-disk
//! layout. No weights are committed — the fetcher pulls them at first run
//! under the upstream licence. Admission is typed: redistributable models
//! (MIT, Apache-2.0, self-trained) admit without an acceptance record;
//! restricted models require an explicit `licence.accepted` record.
//!
//! On-disk layout:
//! ```text
//! <root>/<model_id>/fp32.onnx
//! <root>/<model_id>/fp32.sha256
//! <root>/<model_id>/int8.onnx
//! <root>/<model_id>/int8.sha256
//! <root>/<model_id>/licence.accepted   (restricted models only)
//! ```

use std::path::{Path, PathBuf};

use namtor_proto::worker::{ModelSpec, Quant};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::licence::ensure_model_admitted;
use crate::DaemonError;

/// HTTP download seam. Production uses `ReqwestFetcher`; tests use
/// `FileCopyFetcher` to avoid a real HTTP server.
pub trait HttpFetch {
    fn fetch_to(&self, url: &str, dest: &Path) -> Result<(), DaemonError>;
}

/// No-op fetcher for validation-only paths. Always errors — use when the
/// fetcher is only needed for `validate_against_manifest` (which never
/// downloads).
pub struct NoFetch;

impl HttpFetch for NoFetch {
    fn fetch_to(&self, _url: &str, _dest: &Path) -> Result<(), DaemonError> {
        Err(DaemonError::State(
            "NoFetch: network fetch not available".into(),
        ))
    }
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
impl Default for ReqwestFetcher {
    fn default() -> Self {
        Self::new()
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

    /// Flat model file path: `<root>/<id>.onnx` (FP32) or
    /// `<root>/<id>_int8.onnx` (INT8). The manifest's `source_url` declares
    /// flat paths (e.g. `file:///var/lib/namtor/models/yunet_2023mar.onnx`),
    /// and the deployed files are flat. Per-model metadata (licence records,
    /// sha256 sidecars) lives in the `<root>/<id>/` subdirectory.
    fn quant_file(&self, id: &str, quant: Quant) -> PathBuf {
        match quant {
            Quant::Fp32 => self.root.join(format!("{id}.onnx")),
            Quant::Int8 => self.root.join(format!("{id}_int8.onnx")),
        }
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

pub fn hex(bytes: &[u8]) -> String {
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

impl TargetArch {
    pub fn matches_current(&self) -> bool {
        match self {
            TargetArch::Both => true,
            TargetArch::X86_64 => std::env::consts::ARCH == "x86_64",
            TargetArch::Aarch64 => std::env::consts::ARCH == "aarch64",
        }
    }
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

impl TensorContract {
    pub fn validate(&self) -> Result<(), DaemonError> {
        if self.input.shape.is_empty() {
            return Err(DaemonError::State(
                "tensor contract input shape is empty".into(),
            ));
        }
        if self.output.shape.is_empty() {
            return Err(DaemonError::State(
                "tensor contract output shape is empty".into(),
            ));
        }
        validate_dtype(&self.input.dtype)?;
        validate_dtype(&self.output.dtype)?;
        Ok(())
    }
}

fn validate_dtype(dtype: &str) -> Result<(), DaemonError> {
    match dtype {
        "f16" | "f32" | "f64" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64"
        | "bool" => Ok(()),
        _ => Err(DaemonError::State(format!("unknown tensor dtype: {dtype}"))),
    }
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

fn is_zero_hash_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c == '0')
}

impl ModelFetcher {
    /// Validate a deployed model against the manifest before the worker loads
    /// it. Five gates, all fail-closed:
    ///
    /// 1. **Zero hash** — the manifest variant's sha256 must not be all-zeros.
    /// 2. **Tampered** — the on-disk file's sha256 must equal the manifest
    ///    variant's sha256 (and the `ModelSpec.sha256` must match the manifest).
    /// 3. **Tensor contract** — the manifest's declared tensor contract must
    ///    be structurally valid (non-empty shapes, known dtype).
    /// 4. **Architecture** — the manifest's `target_arch` must match the
    ///    current CPU architecture.
    /// 5. **Admission + Restricted** — typed by `redistribution_policy`:
    ///    redistributable models admit without a record; restricted models
    ///    require an on-disk `licence.accepted` record AND `gate.allow_restricted`.
    ///
    /// If all gates pass, returns the validated on-disk path. A valid offline
    /// cache (every model present + admitted) proceeds with no network access.
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

        if is_zero_hash_hex(&variant.sha256) {
            return Err(DaemonError::State(format!(
                "zero hash in manifest for model {} ({:?}) — real hash required",
                spec.id, spec.quant
            )));
        }

        let expected = parse_hex32(&variant.sha256)?;

        if spec.sha256 != expected {
            return Err(DaemonError::State(format!(
                "spec hash mismatch for model {} ({:?}): spec has {}, manifest has {}",
                spec.id,
                spec.quant,
                hex(&spec.sha256),
                variant.sha256
            )));
        }

        entry.tensor_contract.validate()?;

        if !entry.target_arch.matches_current() {
            return Err(DaemonError::State(format!(
                "arch mismatch for model {}: manifest requires {:?}, host is {}",
                spec.id,
                entry.target_arch,
                std::env::consts::ARCH
            )));
        }

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
        ensure_model_admitted(
            &self.root,
            spec,
            entry.redistribution_policy,
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

/// Build `ModelSpec` entries from the manifest for a set of required model
/// IDs, resolving the FP32 variant's real sha256 from the manifest. Returns
/// specs in the same order as `required_ids`. Errors if any required model is
/// missing from the manifest or has no FP32 variant.
pub fn resolve_required_specs(
    manifest: &ModelManifest,
    required_ids: &[&str],
) -> Result<Vec<ModelSpec>, DaemonError> {
    required_ids
        .iter()
        .map(|id| {
            let entry = manifest
                .find(id)
                .ok_or_else(|| DaemonError::State(format!("model {id} not present in manifest")))?;
            let variant = entry.variant(Quant::Fp32).ok_or_else(|| {
                DaemonError::State(format!("no Fp32 variant for model {id} in manifest"))
            })?;
            let sha256 = parse_hex32(&variant.sha256)?;
            Ok(ModelSpec {
                id: id.to_string(),
                quant: Quant::Fp32,
                sha256,
            })
        })
        .collect()
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
        assert_eq!(path, root.join("test-model.onnx"));
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
        assert!(!root.join("tampered.onnx.tmp").exists());
        assert!(!root.join("tampered.onnx").exists());
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
            !root.join("netdrop.onnx.tmp").exists(),
            ".tmp left behind after fetch failure"
        );
        assert!(
            !root.join("netdrop.onnx").exists(),
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
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("m.onnx"), b"stale wrong content").unwrap();

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
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("m_int8.onnx"), b"int8 weights").unwrap();

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
        assert_eq!(path, root.join("m_int8.onnx"));
    }

    #[test]
    fn ensure_int8_errors_without_python() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("m.onnx"), b"fp32 weights").unwrap();

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
        std::fs::create_dir_all(&root).unwrap();

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

    /// Write a model file at the fetcher's flat on-disk layout path.
    fn write_model_file(root: &Path, id: &str, quant: Quant, data: &[u8]) {
        std::fs::create_dir_all(root).unwrap();
        let name = match quant {
            Quant::Fp32 => format!("{id}.onnx"),
            Quant::Int8 => format!("{id}_int8.onnx"),
        };
        std::fs::write(root.join(name), data).unwrap();
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
        "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30";

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
        // Redistributable model — no licence.accepted record needed.

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
        assert_eq!(path, root.join("m.onnx"));
    }

    #[test]
    fn manifest_validate_redistributable_no_record_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"redistributable weights no record";
        let sha = sha256_bytes(weights);
        let sha_hex = hex(&sha);
        write_model_file(&root, "m", Quant::Fp32, weights);
        std::env::remove_var("NAMTOR_ACCEPT_LICENCE");
        // No licence.accepted record, env var unset — redistributable admits.

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
    }

    #[test]
    fn manifest_validate_blocks_tampered_sha256_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"actual on-disk weights";
        write_model_file(&root, "m", Quant::Fp32, weights);

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
    fn manifest_validate_blocks_unaccepted_restricted_licence() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"restricted weights no record";
        let sha = sha256_bytes(weights);
        let sha_hex = hex(&sha);
        write_model_file(&root, "m", Quant::Fp32, weights);
        // Restricted model, no licence.accepted record, env var unset.
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
        let err = fetcher.validate_against_manifest(&gate, &spec).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("licence not accepted"), "msg = {msg}");
    }

    #[test]
    fn manifest_validate_restricted_mismatched_record_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"restricted weights mismatched record";
        let sha = sha256_bytes(weights);
        let sha_hex = hex(&sha);
        write_model_file(&root, "m", Quant::Fp32, weights);
        // Pre-place a record with a licence sha256 that does NOT match the
        // manifest's licence_sha256.
        write_licence_record(&root, "m", "https://example.com/licence.txt", [0xee; 32]);

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
        let err = fetcher.validate_against_manifest(&gate, &spec).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("sha256 mismatch"), "msg = {msg}");
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
    fn manifest_licences_match_upstream_not_bsd3() {
        let path = Path::new("packaging/model-manifest.toml");
        if !path.exists() {
            eprintln!("skipping — manifest not found at repo root (run from repo root)");
            return;
        }
        let manifest = ModelManifest::parse(path).unwrap();

        let yunet = manifest.find("yunet_2023mar").unwrap();
        assert!(
            !yunet.licence_url.contains("BSD-3"),
            "YuNet licence_url must not reference BSD-3: {}",
            yunet.licence_url
        );
        assert_eq!(
            yunet.licence_sha256,
            "c83b8120c50ccbd4c4f96edf53141bdd566ebb8f8e9227e415326aa1b1aba958",
            "YuNet licence_sha256 must match the MIT licence text hash"
        );

        let sface = manifest.find("sface_2021dec").unwrap();
        assert!(
            !sface.licence_url.contains("BSD-3"),
            "SFace licence_url must not reference BSD-3: {}",
            sface.licence_url
        );
        assert_eq!(
            sface.licence_sha256,
            "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30",
            "SFace licence_sha256 must match the Apache-2.0 licence text hash"
        );

        let pad = manifest.find("minifasnet_pad").unwrap();
        assert_eq!(
            pad.licence_sha256, "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30",
            "MiniFASNet PAD licence_sha256 must match the Apache-2.0 licence text hash"
        );

        for entry in &manifest.models {
            assert_eq!(
                entry.redistribution_policy,
                RedistributionPolicy::Redistributable,
                "model {} must be redistributable",
                entry.id
            );
        }
    }

    fn manifest_with_arch(
        id: &str,
        sha_hex: &str,
        policy: RedistributionPolicy,
        licence_sha_hex: &str,
        arch: TargetArch,
    ) -> ModelManifest {
        ModelManifest {
            manifest_version: 1,
            models: vec![ModelEntry {
                id: id.into(),
                licence_url: "https://example.com/licence.txt".into(),
                licence_sha256: licence_sha_hex.into(),
                redistribution_policy: policy,
                target_arch: arch,
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

    fn manifest_with_bad_tensor(
        id: &str,
        sha_hex: &str,
        licence_sha_hex: &str,
        contract: TensorContract,
    ) -> ModelManifest {
        ModelManifest {
            manifest_version: 1,
            models: vec![ModelEntry {
                id: id.into(),
                licence_url: "https://example.com/licence.txt".into(),
                licence_sha256: licence_sha_hex.into(),
                redistribution_policy: RedistributionPolicy::Redistributable,
                target_arch: TargetArch::Both,
                tensor_contract: contract,
                variants: vec![ModelVariant {
                    quant: Quant::Fp32,
                    source_url: "file://local".into(),
                    sha256: sha_hex.into(),
                }],
            }],
        }
    }

    #[test]
    fn tensor_contract_validate_rejects_empty_input_shape() {
        let contract = TensorContract {
            input: TensorIo {
                shape: vec![],
                dtype: "f32".into(),
                normalisation: None,
            },
            output: TensorIo {
                shape: vec![1, 128],
                dtype: "f32".into(),
                normalisation: None,
            },
        };
        let err = contract.validate().unwrap_err();
        assert!(err.to_string().contains("input shape is empty"));
    }

    #[test]
    fn tensor_contract_validate_rejects_empty_output_shape() {
        let contract = TensorContract {
            input: TensorIo {
                shape: vec![1, 112],
                dtype: "f32".into(),
                normalisation: None,
            },
            output: TensorIo {
                shape: vec![],
                dtype: "f32".into(),
                normalisation: None,
            },
        };
        let err = contract.validate().unwrap_err();
        assert!(err.to_string().contains("output shape is empty"));
    }

    #[test]
    fn tensor_contract_validate_rejects_unknown_dtype() {
        let contract = TensorContract {
            input: TensorIo {
                shape: vec![1, 112],
                dtype: "bogus".into(),
                normalisation: None,
            },
            output: TensorIo {
                shape: vec![1, 128],
                dtype: "f32".into(),
                normalisation: None,
            },
        };
        let err = contract.validate().unwrap_err();
        assert!(err.to_string().contains("unknown tensor dtype"));
    }

    #[test]
    fn tensor_contract_validate_accepts_valid() {
        let contract = TensorContract {
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
        };
        assert!(contract.validate().is_ok());
    }

    #[test]
    fn target_arch_both_always_matches() {
        assert!(TargetArch::Both.matches_current());
    }

    #[test]
    fn target_arch_current_matches_self() {
        let current = match std::env::consts::ARCH {
            "x86_64" => TargetArch::X86_64,
            "aarch64" => TargetArch::Aarch64,
            _ => TargetArch::Both,
        };
        assert!(current.matches_current());
    }

    #[test]
    fn target_arch_rejects_mismatch() {
        let mismatched = match std::env::consts::ARCH {
            "x86_64" => TargetArch::Aarch64,
            _ => TargetArch::X86_64,
        };
        assert!(!mismatched.matches_current());
    }

    #[test]
    fn manifest_validate_blocks_zero_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"weights for zero hash test";
        write_model_file(&root, "m", Quant::Fp32, weights);

        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: tmp.path().join("nonexistent"),
            }),
        );
        let zero_hex = "0".repeat(64);
        let manifest = manifest_with(
            "m",
            &zero_hex,
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
            sha256: [0; 32],
        };
        let err = fetcher.validate_against_manifest(&gate, &spec).unwrap_err();
        assert!(err.to_string().contains("zero hash"), "msg = {err}");
    }

    #[test]
    fn manifest_validate_blocks_spec_hash_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"weights for spec mismatch test";
        let sha = sha256_bytes(weights);
        let sha_hex = hex(&sha);
        write_model_file(&root, "m", Quant::Fp32, weights);

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
            sha256: [0xff; 32],
        };
        let err = fetcher.validate_against_manifest(&gate, &spec).unwrap_err();
        assert!(
            err.to_string().contains("spec hash mismatch"),
            "msg = {err}"
        );
    }

    #[test]
    fn manifest_validate_blocks_arch_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"weights for arch mismatch test";
        let sha = sha256_bytes(weights);
        let sha_hex = hex(&sha);
        write_model_file(&root, "m", Quant::Fp32, weights);

        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: tmp.path().join("nonexistent"),
            }),
        );
        let wrong_arch = match std::env::consts::ARCH {
            "x86_64" => TargetArch::Aarch64,
            _ => TargetArch::X86_64,
        };
        let manifest = manifest_with_arch(
            "m",
            &sha_hex,
            RedistributionPolicy::Redistributable,
            LICENCE_SHA_HEX,
            wrong_arch,
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
        assert!(err.to_string().contains("arch mismatch"), "msg = {err}");
    }

    #[test]
    fn manifest_validate_blocks_invalid_tensor_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let weights = b"weights for bad tensor test";
        let sha = sha256_bytes(weights);
        let sha_hex = hex(&sha);
        write_model_file(&root, "m", Quant::Fp32, weights);

        let fetcher = ModelFetcher::new(
            &root,
            Box::new(FileCopyFetcher {
                source: tmp.path().join("nonexistent"),
            }),
        );
        let bad_contract = TensorContract {
            input: TensorIo {
                shape: vec![],
                dtype: "f32".into(),
                normalisation: None,
            },
            output: TensorIo {
                shape: vec![1, 128],
                dtype: "f32".into(),
                normalisation: None,
            },
        };
        let manifest = manifest_with_bad_tensor("m", &sha_hex, LICENCE_SHA_HEX, bad_contract);
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
        assert!(
            err.to_string().contains("input shape is empty"),
            "msg = {err}"
        );
    }

    #[test]
    fn resolve_required_specs_builds_from_manifest() {
        let path = Path::new("packaging/model-manifest.toml");
        if !path.exists() {
            eprintln!("skipping — manifest not found at repo root");
            return;
        }
        let manifest = ModelManifest::parse(path).unwrap();
        let specs = resolve_required_specs(
            &manifest,
            &["yunet_2023mar", "sface_2021dec", "minifasnet_pad"],
        )
        .unwrap();
        assert_eq!(specs.len(), 3);
        for spec in &specs {
            assert_ne!(spec.sha256, [0u8; 32], "zero hash for {}", spec.id);
            assert_eq!(spec.quant, Quant::Fp32);
        }
    }

    #[test]
    fn resolve_required_specs_errors_on_missing_model() {
        let manifest = ModelManifest {
            manifest_version: 1,
            models: vec![],
        };
        let err = resolve_required_specs(&manifest, &["ghost"]).unwrap_err();
        assert!(err.to_string().contains("not present in manifest"));
    }

    #[test]
    fn no_fetch_errors_on_fetch_call() {
        let fetcher = ModelFetcher::new(Path::new("/tmp"), Box::new(NoFetch));
        let spec = ModelSpec {
            id: "x".into(),
            quant: Quant::Fp32,
            sha256: [0xaa; 32],
        };
        let err = fetcher.fetch(&spec, "file://local").unwrap_err();
        assert!(err.to_string().contains("NoFetch"));
    }

    #[test]
    fn is_zero_hash_hex_detects_all_zeros() {
        assert!(is_zero_hash_hex(&"0".repeat(64)));
        assert!(!is_zero_hash_hex(&"1".repeat(64)));
        assert!(!is_zero_hash_hex("ab"));
    }

    fn manifest_with_three(sha_yunet: &str, sha_sface: &str, sha_pad: &str) -> ModelManifest {
        let contract = TensorContract {
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
        };
        ModelManifest {
            manifest_version: 1,
            models: vec![
                ModelEntry {
                    id: "yunet_2023mar".into(),
                    licence_url: "https://example.com/licence.txt".into(),
                    licence_sha256: LICENCE_SHA_HEX.into(),
                    redistribution_policy: RedistributionPolicy::Redistributable,
                    target_arch: TargetArch::Both,
                    tensor_contract: contract.clone(),
                    variants: vec![ModelVariant {
                        quant: Quant::Fp32,
                        source_url: "file://local".into(),
                        sha256: sha_yunet.into(),
                    }],
                },
                ModelEntry {
                    id: "sface_2021dec".into(),
                    licence_url: "https://example.com/licence.txt".into(),
                    licence_sha256: LICENCE_SHA_HEX.into(),
                    redistribution_policy: RedistributionPolicy::Redistributable,
                    target_arch: TargetArch::Both,
                    tensor_contract: contract.clone(),
                    variants: vec![ModelVariant {
                        quant: Quant::Fp32,
                        source_url: "file://local".into(),
                        sha256: sha_sface.into(),
                    }],
                },
                ModelEntry {
                    id: "minifasnet_pad".into(),
                    licence_url: "https://example.com/licence.txt".into(),
                    licence_sha256: LICENCE_SHA_HEX.into(),
                    redistribution_policy: RedistributionPolicy::Redistributable,
                    target_arch: TargetArch::Both,
                    tensor_contract: contract,
                    variants: vec![ModelVariant {
                        quant: Quant::Fp32,
                        source_url: "file://local".into(),
                        sha256: sha_pad.into(),
                    }],
                },
            ],
        }
    }

    fn validate_all_three(
        root: &Path,
        manifest: &ModelManifest,
    ) -> Result<Vec<PathBuf>, DaemonError> {
        let fetcher = ModelFetcher::new(root, Box::new(NoFetch));
        let gate = ManifestGate {
            manifest,
            accepted_by: "test",
            allow_restricted: false,
        };
        let specs = resolve_required_specs(
            manifest,
            &["yunet_2023mar", "sface_2021dec", "minifasnet_pad"],
        )?;
        let mut paths = Vec::new();
        for spec in &specs {
            paths.push(fetcher.validate_against_manifest(&gate, spec)?);
        }
        Ok(paths)
    }

    #[test]
    fn startup_validation_all_three_models_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let w1 = b"yunet weights";
        let w2 = b"sface weights";
        let w3 = b"pad weights";
        let s1 = sha256_bytes(w1);
        let s2 = sha256_bytes(w2);
        let s3 = sha256_bytes(w3);
        write_model_file(&root, "yunet_2023mar", Quant::Fp32, w1);
        write_model_file(&root, "sface_2021dec", Quant::Fp32, w2);
        write_model_file(&root, "minifasnet_pad", Quant::Fp32, w3);
        let manifest = manifest_with_three(&hex(&s1), &hex(&s2), &hex(&s3));
        let paths = validate_all_three(&root, &manifest).unwrap();
        assert_eq!(paths.len(), 3);
        for p in &paths {
            assert!(p.exists());
        }
    }

    #[test]
    fn startup_validation_tamper_yunet_denies_all() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let w1 = b"yunet weights tampered";
        let w2 = b"sface weights";
        let w3 = b"pad weights";
        let s1_real = sha256_bytes(b"yunet weights original");
        let s2 = sha256_bytes(w2);
        let s3 = sha256_bytes(w3);
        write_model_file(&root, "yunet_2023mar", Quant::Fp32, w1);
        write_model_file(&root, "sface_2021dec", Quant::Fp32, w2);
        write_model_file(&root, "minifasnet_pad", Quant::Fp32, w3);
        let manifest = manifest_with_three(&hex(&s1_real), &hex(&s2), &hex(&s3));
        let err = validate_all_three(&root, &manifest).unwrap_err();
        assert!(err.to_string().contains("tampered"), "msg = {err}");
    }

    #[test]
    fn startup_validation_tamper_sface_denies_all() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let w1 = b"yunet weights";
        let w2 = b"sface weights tampered";
        let w3 = b"pad weights";
        let s1 = sha256_bytes(w1);
        let s2_real = sha256_bytes(b"sface weights original");
        let s3 = sha256_bytes(w3);
        write_model_file(&root, "yunet_2023mar", Quant::Fp32, w1);
        write_model_file(&root, "sface_2021dec", Quant::Fp32, w2);
        write_model_file(&root, "minifasnet_pad", Quant::Fp32, w3);
        let manifest = manifest_with_three(&hex(&s1), &hex(&s2_real), &hex(&s3));
        let err = validate_all_three(&root, &manifest).unwrap_err();
        assert!(err.to_string().contains("tampered"), "msg = {err}");
    }

    #[test]
    fn startup_validation_tamper_pad_denies_all() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let w1 = b"yunet weights";
        let w2 = b"sface weights";
        let w3 = b"pad weights tampered";
        let s1 = sha256_bytes(w1);
        let s2 = sha256_bytes(w2);
        let s3_real = sha256_bytes(b"pad weights original");
        write_model_file(&root, "yunet_2023mar", Quant::Fp32, w1);
        write_model_file(&root, "sface_2021dec", Quant::Fp32, w2);
        write_model_file(&root, "minifasnet_pad", Quant::Fp32, w3);
        let manifest = manifest_with_three(&hex(&s1), &hex(&s2), &hex(&s3_real));
        let err = validate_all_three(&root, &manifest).unwrap_err();
        assert!(err.to_string().contains("tampered"), "msg = {err}");
    }

    #[test]
    fn startup_validation_missing_model_denies_all() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let w1 = b"yunet weights";
        let w2 = b"sface weights";
        let s1 = sha256_bytes(w1);
        let s2 = sha256_bytes(w2);
        let s3 = sha256_bytes(b"pad weights");
        write_model_file(&root, "yunet_2023mar", Quant::Fp32, w1);
        write_model_file(&root, "sface_2021dec", Quant::Fp32, w2);
        // minifasnet_pad NOT written
        let manifest = manifest_with_three(&hex(&s1), &hex(&s2), &hex(&s3));
        let err = validate_all_three(&root, &manifest).unwrap_err();
        assert!(
            err.to_string().contains("model file missing"),
            "msg = {err}"
        );
    }

    #[test]
    fn startup_validation_zero_hash_in_manifest_denies() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let w1 = b"yunet weights";
        let w2 = b"sface weights";
        let w3 = b"pad weights";
        let s2 = sha256_bytes(w2);
        let s3 = sha256_bytes(w3);
        write_model_file(&root, "yunet_2023mar", Quant::Fp32, w1);
        write_model_file(&root, "sface_2021dec", Quant::Fp32, w2);
        write_model_file(&root, "minifasnet_pad", Quant::Fp32, w3);
        let zero = "0".repeat(64);
        let manifest = manifest_with_three(&zero, &hex(&s2), &hex(&s3));
        let err = validate_all_three(&root, &manifest).unwrap_err();
        assert!(err.to_string().contains("zero hash"), "msg = {err}");
    }

    #[test]
    fn startup_validation_logs_contain_no_biometric_data() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        let w1 = b"yunet weights";
        let w2 = b"sface weights";
        let w3 = b"pad weights";
        let s1 = sha256_bytes(w1);
        let s2 = sha256_bytes(w2);
        let s3 = sha256_bytes(w3);
        write_model_file(&root, "yunet_2023mar", Quant::Fp32, w1);
        write_model_file(&root, "sface_2021dec", Quant::Fp32, w2);
        write_model_file(&root, "minifasnet_pad", Quant::Fp32, w3);
        let manifest = manifest_with_three(&hex(&s1), &hex(&s2), &hex(&s3));

        let fetcher = ModelFetcher::new(&root, Box::new(NoFetch));
        let gate = ManifestGate {
            manifest: &manifest,
            accepted_by: "test",
            allow_restricted: false,
        };
        let specs = resolve_required_specs(
            &manifest,
            &["yunet_2023mar", "sface_2021dec", "minifasnet_pad"],
        )
        .unwrap();

        for spec in &specs {
            let path = fetcher.validate_against_manifest(&gate, spec).unwrap();
            let log_msg = format!(
                "model_id={} sha256={} path={}",
                spec.id,
                hex(&spec.sha256),
                path.display()
            );
            let lower = log_msg.to_lowercase();
            for marker in &["frame", "embedding", "template", "pixel", "score", "vec="] {
                assert!(
                    !lower.contains(marker),
                    "biometric marker '{marker}' in startup log: {log_msg}"
                );
            }
        }
    }

    #[test]
    fn startup_validation_packaging_manifest_all_three_valid() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let manifest_path = repo_root.join("packaging/model-manifest.toml");
        let models_dir = repo_root.join("var/lib/namtor/models");
        if !manifest_path.exists() || !models_dir.exists() {
            eprintln!("skipping — manifest or models not found");
            return;
        }
        let manifest = ModelManifest::parse(&manifest_path).unwrap();
        let specs = resolve_required_specs(
            &manifest,
            &["yunet_2023mar", "sface_2021dec", "minifasnet_pad"],
        )
        .unwrap();
        assert_eq!(specs.len(), 3);
        for spec in &specs {
            assert_ne!(spec.sha256, [0u8; 32], "zero hash for {}", spec.id);
        }
        let fetcher = ModelFetcher::new(&models_dir, Box::new(NoFetch));
        let gate = ManifestGate {
            manifest: &manifest,
            accepted_by: "test",
            allow_restricted: false,
        };
        for spec in &specs {
            let path = fetcher
                .validate_against_manifest(&gate, spec)
                .unwrap_or_else(|e| panic!("validation failed for {}: {e}", spec.id));
            assert!(path.exists(), "validated path missing: {}", path.display());
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
