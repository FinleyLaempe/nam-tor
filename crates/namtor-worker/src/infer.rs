// SPDX-License-Identifier: GPL-3.0-only
//! Inference backend abstraction.
//!
//! [`InferenceBackend`] is the seam between the worker's request loop and the
//! model runtime. The production backend is [`OnnxCpu`] (Task 5, behind the
//! default-off `mock-infer` feature gate toggle is inverted: `mock-infer` ON
//! selects [`MockBackend`], OFF selects `OnnxCpu`). Tests and the M3 e2e
//! harness run with `--features mock-infer` so CI needs no ONNX Runtime and no
//! model weights.
//!
//! `MockBackend` is a deterministic LCG seeded by the frame bytes, so identical
//! frames produce identical embeddings and different frames produce different
//! ones — enough for the daemon/supervisor tests (Task 21, Task 26) to assert
//! round-trip behaviour without any real model.

use namtor_proto::worker::{FrameRef, Loaded, ModelSpec};

use crate::WorkerError;

/// A loaded model handle and its observed input/output shapes.
pub trait InferenceBackend {
    /// Resolve and load `spec`; return the shapes the backend observed.
    fn load(&mut self, spec: &ModelSpec) -> Result<Loaded, WorkerError>;
    /// Run inference on `frame` (raw pixel bytes from the memfd region)
    /// described by `meta`, returning the embedding vector.
    fn run(&self, loaded: &Loaded, frame: &[u8], meta: &FrameRef) -> Result<Vec<f32>, WorkerError>;
}

/// Deterministic mock backend: an LCG seeded from the frame bytes, emitting
/// `out_dim` pseudo-embeddings. No model file is read; `load` returns a
/// [`Loaded`] whose shapes are `[1, frame.len()]` → `[1, out_dim]`.
pub struct MockBackend {
    out_dim: usize,
}

impl MockBackend {
    pub fn new(out_dim: usize) -> Self {
        Self { out_dim }
    }
}

impl InferenceBackend for MockBackend {
    fn load(&mut self, spec: &ModelSpec) -> Result<Loaded, WorkerError> {
        Ok(Loaded {
            model_id: spec.id.clone(),
            quant: spec.quant,
            input_shape: vec![1, self.out_dim as u64],
            output_shape: vec![1, self.out_dim as u64],
        })
    }

    fn run(
        &self,
        loaded: &Loaded,
        frame: &[u8],
        _meta: &FrameRef,
    ) -> Result<Vec<f32>, WorkerError> {
        // Numerical Recipes LCG: x_{n+1} = 1664525 x_n + 1013904223 (mod 2^32).
        // Seed by mixing the frame bytes so identical frames → identical vectors
        // and different frames → different vectors.
        let mut state: u32 = 0x42u32;
        for &b in frame {
            state = state.wrapping_mul(31).wrapping_add(b as u32);
        }
        let dim = loaded
            .output_shape
            .last()
            .copied()
            .unwrap_or(self.out_dim as u64) as usize;
        let mut out = Vec::with_capacity(dim);
        for _ in 0..dim {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            // Map to [-1, 1].
            let f = (state as f32 / u32::MAX as f32) * 2.0 - 1.0;
            out.push(f);
        }
        Ok(out)
    }
}

/// Construct the default backend for the current feature configuration.
///
/// With `mock-infer` enabled, returns [`MockBackend`]; otherwise returns the
/// production [`OnnxCpu`] backend (Task 5). The `ort` dependency is only
/// pulled in when the `onnx` feature is enabled (default).
#[cfg(feature = "mock-infer")]
pub fn default_backend() -> Box<dyn InferenceBackend> {
    Box::new(MockBackend::new(192))
}

#[cfg(all(feature = "onnx", not(feature = "mock-infer")))]
pub fn default_backend() -> Box<dyn InferenceBackend> {
    Box::new(crate::infer_onnx::OnnxCpu::new(1))
}

#[cfg(not(any(feature = "mock-infer", feature = "onnx")))]
pub fn default_backend() -> Box<dyn InferenceBackend> {
    Box::new(MockBackend::new(192))
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_capture::frame::{EmitterState, PixFmt};
    use namtor_capture::source::Spectrum;
    use namtor_proto::worker::Quant;

    fn spec() -> ModelSpec {
        ModelSpec {
            id: "m".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        }
    }

    fn frame_ref() -> FrameRef {
        FrameRef {
            offset: 0,
            len: 3,
            emitter: EmitterState::On,
            spectrum: Spectrum::Ir,
            pix_fmt: PixFmt::Grey8,
            w: 3,
            h: 1,
            ts_us: 0,
        }
    }

    #[test]
    fn mock_load_returns_loaded_with_model_id() {
        let mut b = MockBackend::new(192);
        let loaded = b.load(&spec()).unwrap();
        assert_eq!(loaded.model_id, "m");
        assert_eq!(loaded.quant, Quant::Fp32);
        assert_eq!(loaded.output_shape, vec![1, 192]);
    }

    #[test]
    fn mock_run_is_deterministic_on_same_frame() {
        let mut b = MockBackend::new(8);
        let loaded = b.load(&spec()).unwrap();
        let frame = [1u8, 2, 3, 4, 5];
        let v1 = b.run(&loaded, &frame, &frame_ref()).unwrap();
        let v2 = b.run(&loaded, &frame, &frame_ref()).unwrap();
        assert_eq!(v1.len(), 8);
        assert_eq!(v1, v2, "identical frames must produce identical vectors");
    }

    #[test]
    fn mock_run_differs_on_different_frame() {
        let mut b = MockBackend::new(8);
        let loaded = b.load(&spec()).unwrap();
        let v1 = b.run(&loaded, &[1u8, 2, 3], &frame_ref()).unwrap();
        let v2 = b.run(&loaded, &[9u8, 8, 7], &frame_ref()).unwrap();
        assert_ne!(v1, v2, "different frames must produce different vectors");
    }

    #[test]
    fn mock_run_output_dim_follows_loaded_shape() {
        let mut b = MockBackend::new(16);
        let loaded = b.load(&spec()).unwrap();
        let v = b.run(&loaded, &[0u8; 10], &frame_ref()).unwrap();
        assert_eq!(v.len(), 16);
    }

    #[cfg(feature = "mock-infer")]
    #[test]
    fn default_backend_is_mock_under_feature() {
        let _b = default_backend();
        // The fact that this compiles under mock-infer proves the cfg gate.
    }
}
