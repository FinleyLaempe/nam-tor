// SPDX-License-Identifier: GPL-3.0-only
//! FP32 vs INT8 production inference micro-benchmark (Task 30, spec §5.3).
//!
//! Loads both quantisation variants of a model via [`OnnxCpu`] and measures
//! per-inference latency (`std::time::Instant`), output-vector delta (cosine
//! similarity + max absolute error), and model file size. The CPU EP
//! auto-detects AVX-VNNI at runtime; no GPU execution provider is ever
//! selected (spec §5.3).
//!
//! This is the Rust-side counterpart to `ml/recipes/eval_int8_vs_fp32.py`.
//! The Python script runs the full eval on real stock models (SFace, YuNet)
//! with onnxruntime; this module exercises the same `OnnxCpu` code path that
//! the auth path uses, so CI can verify the INT8 loading + inference path
//! against the committed FC fixture without pulling Python or real weights.

use std::path::Path;
use std::time::Instant;

use namtor_proto::worker::{FrameRef, ModelSpec, Quant};

use crate::infer::InferenceBackend;
use crate::infer_onnx::OnnxCpu;
use crate::WorkerError;

/// Per-variant measurement: model size + latency samples.
#[derive(Debug, Clone)]
pub struct VariantMeasurement {
    /// Model file size in bytes.
    pub size_bytes: u64,
    /// Per-inference latencies in microseconds, warmup excluded.
    pub latencies_us: Vec<u64>,
}

impl VariantMeasurement {
    /// Median latency in microseconds.
    pub fn median_us(&self) -> u64 {
        if self.latencies_us.is_empty() {
            return 0;
        }
        let mut sorted = self.latencies_us.clone();
        sorted.sort_unstable();
        let mid = sorted.len() / 2;
        sorted[mid]
    }

    /// p95 latency in microseconds.
    pub fn p95_us(&self) -> u64 {
        if self.latencies_us.is_empty() {
            return 0;
        }
        let mut sorted = self.latencies_us.clone();
        sorted.sort_unstable();
        let idx = sorted.len() * 95 / 100;
        sorted[idx.min(sorted.len() - 1)]
    }

    /// Mean latency in microseconds.
    pub fn mean_us(&self) -> u64 {
        if self.latencies_us.is_empty() {
            return 0;
        }
        let sum: u128 = self.latencies_us.iter().map(|&x| x as u128).sum();
        (sum / self.latencies_us.len() as u128) as u64
    }
}

/// Accuracy comparison between FP32 and INT8 outputs.
#[derive(Debug, Clone)]
pub struct AccuracyDelta {
    /// Mean cosine similarity across all test inputs.
    pub cosine_mean: f64,
    /// Minimum cosine similarity across all test inputs.
    pub cosine_min: f64,
    /// Mean max-absolute-error across all test inputs.
    pub max_abs_error_mean: f64,
    /// Worst max-absolute-error across all test inputs.
    pub max_abs_error_max: f64,
    /// Number of test inputs compared.
    pub n_samples: usize,
}

/// Full FP32-vs-INT8 evaluation report.
#[derive(Debug, Clone)]
pub struct Int8EvalReport {
    pub fp32: VariantMeasurement,
    pub int8: VariantMeasurement,
    pub accuracy: AccuracyDelta,
    /// INT8 size / FP32 size (< 1.0 = INT8 smaller).
    pub size_ratio: f64,
    /// FP32 median / INT8 median (> 1.0 = INT8 faster).
    pub latency_speedup: f64,
}

/// Configuration for [`run_eval`].
pub struct EvalConfig {
    /// Number of warmup runs (excluded from latency measurement).
    pub n_warmup: usize,
    /// Number of measured runs.
    pub n_runs: usize,
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            n_warmup: 10,
            n_runs: 100,
        }
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum();
    let na: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    if na < 1e-12 || nb < 1e-12 {
        return 0.0;
    }
    dot / (na * nb)
}

fn max_abs_error(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as f64 - *y as f64).abs())
        .fold(0.0_f64, f64::max)
}

/// Run the FP32-vs-INT8 evaluation using [`OnnxCpu`].
///
/// `fp32_path` and `int8_path` are direct file paths to the ONNX models.
/// `test_inputs` is a list of pre-serialized f32-LE byte buffers (the frame
/// contract: raw f32 values in little-endian bytes).
///
/// Returns an [`Int8EvalReport`] with latency, accuracy, and size comparison.
pub fn run_eval(
    fp32_path: &Path,
    int8_path: &Path,
    test_inputs: &[Vec<u8>],
    config: &EvalConfig,
) -> Result<Int8EvalReport, WorkerError> {
    let mut backend = OnnxCpu::new(1);

    let fp32_spec = ModelSpec {
        id: "eval".into(),
        quant: Quant::Fp32,
        sha256: [0; 32],
    };
    let int8_spec = ModelSpec {
        id: "eval".into(),
        quant: Quant::Int8,
        sha256: [0; 32],
    };

    let fp32_loaded = backend.load_file(fp32_path, &fp32_spec)?;
    let int8_loaded = backend.load_file(int8_path, &int8_spec)?;

    let dummy_meta = FrameRef {
        offset: 0,
        len: 0,
        emitter: namtor_capture::frame::EmitterState::On,
        spectrum: namtor_capture::source::Spectrum::Ir,
        pix_fmt: namtor_capture::frame::PixFmt::Grey8,
        w: 0,
        h: 0,
        ts_us: 0,
    };

    // Warmup
    for input in test_inputs.iter().take(config.n_warmup) {
        let _ = backend.run(&fp32_loaded, input, &dummy_meta)?;
        let _ = backend.run(&int8_loaded, input, &dummy_meta)?;
    }

    // Measure latency
    let mut fp32_lats = Vec::with_capacity(config.n_runs);
    let mut int8_lats = Vec::with_capacity(config.n_runs);
    for input in test_inputs.iter().take(config.n_runs) {
        let t0 = Instant::now();
        let _ = backend.run(&fp32_loaded, input, &dummy_meta)?;
        fp32_lats.push(t0.elapsed().as_micros() as u64);

        let t0 = Instant::now();
        let _ = backend.run(&int8_loaded, input, &dummy_meta)?;
        int8_lats.push(t0.elapsed().as_micros() as u64);
    }

    // Accuracy: compare outputs on all test inputs
    let mut cosines = Vec::new();
    let mut errors = Vec::new();
    for input in test_inputs {
        let fp32_out = backend.run(&fp32_loaded, input, &dummy_meta)?;
        let int8_out = backend.run(&int8_loaded, input, &dummy_meta)?;
        cosines.push(cosine_similarity(&fp32_out, &int8_out));
        errors.push(max_abs_error(&fp32_out, &int8_out));
    }
    let n_samples = cosines.len();
    let cosine_mean = cosines.iter().sum::<f64>() / n_samples as f64;
    let cosine_min = cosines.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_abs_error_mean = errors.iter().sum::<f64>() / n_samples as f64;
    let max_abs_error_max = errors.iter().cloned().fold(0.0_f64, f64::max);

    let fp32_size = std::fs::metadata(fp32_path)?.len();
    let int8_size = std::fs::metadata(int8_path)?.len();

    let fp32_meas = VariantMeasurement {
        size_bytes: fp32_size,
        latencies_us: fp32_lats,
    };
    let int8_meas = VariantMeasurement {
        size_bytes: int8_size,
        latencies_us: int8_lats,
    };

    let size_ratio = int8_size as f64 / fp32_size as f64;
    let latency_speedup = if int8_meas.median_us() > 0 {
        fp32_meas.median_us() as f64 / int8_meas.median_us() as f64
    } else {
        0.0
    };

    Ok(Int8EvalReport {
        fp32: fp32_meas,
        int8: int8_meas,
        accuracy: AccuracyDelta {
            cosine_mean,
            cosine_min,
            max_abs_error_mean,
            max_abs_error_max,
            n_samples,
        },
        size_ratio,
        latency_speedup,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locate a fixture under `fixtures/onnx/` relative to the crate manifest dir.
    fn fixture_path(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("onnx")
            .join(name)
    }

    /// Generate f32-LE byte buffers from f32 slices (the frame contract).
    fn to_frame(vals: &[f32]) -> Vec<u8> {
        vals.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn fc_fixture_eval_compares_fp32_and_int8() {
        let fp32 = fixture_path("fc_fp32.onnx");
        let int8 = fixture_path("fc_int8.onnx");
        if !fp32.exists() || !int8.exists() {
            eprintln!("skipping — FC fixtures not found at {fp32:?} / {int8:?}");
            return;
        }

        // 20 test inputs: 8 f32 values each (matching [1, 8] input shape).
        let mut seed: u32 = 0x42;
        let inputs: Vec<Vec<u8>> = (0..20)
            .map(|_| {
                let vals: Vec<f32> = (0..8)
                    .map(|_| {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
                    })
                    .collect();
                to_frame(&vals)
            })
            .collect();

        let report = run_eval(
            &fp32,
            &int8,
            &inputs,
            &EvalConfig {
                n_warmup: 5,
                n_runs: 20,
            },
        )
        .expect("eval");

        // INT8 must produce finite, non-empty output (the inference path works).
        assert!(report.accuracy.n_samples > 0);
        assert!(report.accuracy.cosine_mean.is_finite());
        // Cosine similarity should be high (same model, quantized).
        assert!(
            report.accuracy.cosine_mean > 0.8,
            "cosine_mean = {}",
            report.accuracy.cosine_mean
        );
        // Size ratio: INT8 may be larger for tiny models (QDQ overhead).
        assert!(report.size_ratio > 0.0);
        // Latency speedup: for a tiny model INT8 may be slower (overhead).
        // We just assert it's finite and positive.
        assert!(report.latency_speedup >= 0.0);
        // Both variants should have measured latencies.
        assert!(!report.fp32.latencies_us.is_empty());
        assert!(!report.int8.latencies_us.is_empty());
    }

    #[test]
    fn cosine_similarity_identical_vectors() {
        let a = vec![1.0_f32, 2.0, 3.0];
        assert!((cosine_similarity(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn max_abs_error_basic() {
        let a = vec![1.0_f32, 2.0, 3.0];
        let b = vec![1.1_f32, 2.5, 2.0];
        assert!((max_abs_error(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn median_and_p95_of_sorted_samples() {
        let m = VariantMeasurement {
            size_bytes: 0,
            latencies_us: vec![10, 20, 30, 40, 100],
        };
        assert_eq!(m.median_us(), 30);
        // p95 of 5 samples: idx = 5*95/100 = 4 → 100
        assert_eq!(m.p95_us(), 100);
        assert_eq!(m.mean_us(), 40);
    }

    #[test]
    fn median_of_empty_is_zero() {
        let m = VariantMeasurement {
            size_bytes: 0,
            latencies_us: vec![],
        };
        assert_eq!(m.median_us(), 0);
        assert_eq!(m.p95_us(), 0);
        assert_eq!(m.mean_us(), 0);
    }
}
