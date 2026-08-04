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
use sha2::{Digest, Sha256};

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

        // Download to .tmp, verify, rename.
        let tmp_path = final_path.with_extension("onnx.tmp");
        self.http.fetch_to(source_url, &tmp_path)?;

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
}
