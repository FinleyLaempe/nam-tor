// SPDX-License-Identifier: GPL-3.0-only
//! `OnnxCpu`: the production inference backend backed by the `ort` crate
//! (ONNX Runtime, CPU Execution Provider).
//!
//! # Resolved `ort` 2.0.0-rc.13 API (recorded per Task 5 requirement)
//!
//! ```ignore
//! use ort::session::{builder::GraphOptimizationLevel, Session};
//!
//! // Build a CPU-only session. No explicit EP registration is needed — the
//! // CPUExecutionProvider is the default; AVX-VNNI is auto-detected by ORT
//! // at runtime (no API flag to enable it).
//! let session = Session::builder()?
//!     .with_optimization_level(GraphOptimizationLevel::Level3)?
//!     .with_intra_threads(threads)?
//!     .commit_from_memory(&model_bytes)?;
//!
//! // Create an input tensor from owned data: (shape, flat_vec).
//! use ort::value::Tensor;
//! let tensor = Tensor::from_array(([1usize, n], vec![1.0_f32; n]))?;
//!
//! // Run with named inputs via the `ort::inputs!` macro.
//! let outputs = session.run(ort::inputs![session.inputs()[0].name.as_str() => tensor]?)?;
//!
//! // Extract output as ArrayViewD<f32>, then collect to Vec<f32>.
//! let view: ndarray::ArrayViewD<f32> = outputs[0].try_extract_array::<f32>()?;
//! let vec: Vec<f32> = view.iter().cloned().collect();
//! ```
//!
//! # AVX-VNNI
//!
//! The reference CPU (Intel i9-13900H) supports AVX2 + AVX-VNNI. ONNX Runtime's
//! CPU EP auto-detects these at runtime; there is no `ort` API to force-enable
//! AVX-VNNI. INT8 quantized models (Task 16 recipe) pick up VNNI instructions
//! automatically when the CPU EP handles `QLinear*` / quantised ops.
//!
//! # Sandbox interaction
//!
//! `load` reads the model file from disk (`<root>/<id>/<quant>.onnx`). The
//! worker must call `load` **before** applying the seccomp+landlock sandbox
//! (Task 9), since the sandbox denies `open`/`openat`. The daemon supervisor
//! (Task 21) sends `Load` immediately after spawn; the worker handles those
//! loads, then applies the sandbox, then enters the `Infer`/`Shutdown` loop.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use namtor_proto::worker::{FrameRef, Loaded, ModelSpec, Quant};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;

use crate::infer::InferenceBackend;
use crate::WorkerError;

/// Extract the shape from a session I/O [`ValueType`] as `u64` dims. Dynamic
/// dims (-1) become 0 so the wire shape is non-negative.
fn shape_to_u64(dtype: &ort::value::ValueType) -> Vec<u64> {
    match dtype {
        ort::value::ValueType::Tensor { shape, .. } => {
            shape.iter().map(|d| (*d).max(0) as u64).collect()
        }
        _ => Vec::new(),
    }
}

/// CPU ONNX Runtime backend.
///
/// Sessions live behind `RefCell` because `ort::Session::run` takes `&mut
/// self` while the [`InferenceBackend`] trait exposes `run(&self, ...)`; the
/// worker is single-threaded so interior mutability is sound.
pub struct OnnxCpu {
    root: PathBuf,
    threads: usize,
    sessions: HashMap<String, RefCell<Session>>,
}

impl OnnxCpu {
    /// `root` is the model store root (`<root>/<id>/<quant>.onnx`).
    pub fn new(threads: usize) -> Self {
        Self {
            root: PathBuf::new(),
            threads,
            sessions: HashMap::new(),
        }
    }

    /// Set the model store root. Called by the worker binary before the loop.
    pub fn with_root(mut self, root: PathBuf) -> Self {
        self.root = root;
        self
    }

    fn quant_file(quant: Quant) -> &'static str {
        match quant {
            Quant::Fp32 => "fp32.onnx",
            Quant::Int8 => "int8.onnx",
        }
    }

    fn model_path(&self, spec: &ModelSpec) -> PathBuf {
        self.root.join(&spec.id).join(Self::quant_file(spec.quant))
    }

    /// Load a model directly from a file path, bypassing the
    /// `<root>/<id>/<quant>.onnx` convention. The `spec.id` is used only
    /// as the session key and metadata label.
    pub fn load_file(&mut self, path: &Path, spec: &ModelSpec) -> Result<Loaded, WorkerError> {
        let bytes = std::fs::read(path)
            .map_err(|e| WorkerError::Infer(format!("read {}: {e}", path.display())))?;
        self.load_bytes(path, &bytes, spec)
    }

    fn load_bytes(
        &mut self,
        path: &Path,
        bytes: &[u8],
        spec: &ModelSpec,
    ) -> Result<Loaded, WorkerError> {
        let session = Session::builder()
            .map_err(|e| WorkerError::Infer(format!("ort builder: {e}")))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| WorkerError::Infer(format!("opt level: {e}")))?
            .with_intra_threads(self.threads)
            .map_err(|e| WorkerError::Infer(format!("intra threads: {e}")))?
            .commit_from_memory(bytes)
            .map_err(|e| WorkerError::Infer(format!("commit {}: {e}", path.display())))?;
        let input_shape = session
            .inputs()
            .first()
            .map(|i| shape_to_u64(i.dtype()))
            .unwrap_or_default();
        let output_shape = session
            .outputs()
            .first()
            .map(|o| shape_to_u64(o.dtype()))
            .unwrap_or_default();
        let key = format!("{}:{:?}", spec.id, spec.quant);
        self.sessions.insert(key.clone(), RefCell::new(session));
        Ok(Loaded {
            model_id: spec.id.clone(),
            quant: spec.quant,
            input_shape,
            output_shape,
        })
    }
}

impl InferenceBackend for OnnxCpu {
    fn load(&mut self, spec: &ModelSpec) -> Result<Loaded, WorkerError> {
        let path = self.model_path(spec);
        let bytes = std::fs::read(&path)
            .map_err(|e| WorkerError::Infer(format!("read {}: {e}", path.display())))?;
        self.load_bytes(&path, &bytes, spec)
    }

    fn run(
        &self,
        loaded: &Loaded,
        frame: &[u8],
        _meta: &FrameRef,
    ) -> Result<Vec<f32>, WorkerError> {
        let key = format!("{}:{:?}", loaded.model_id, loaded.quant);
        let cell = self
            .sessions
            .get(&key)
            .ok_or_else(|| WorkerError::Infer(format!("model not loaded: {key}")))?;
        let mut session = cell.borrow_mut();
        let input_vec: Vec<f32> = frame
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| WorkerError::Infer("model has no inputs".into()))?;
        let output_name = session
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .ok_or_else(|| WorkerError::Infer("model has no outputs".into()))?;
        let shape: Vec<i64> = loaded
            .input_shape
            .iter()
            .map(|&d| {
                if d == 0 {
                    input_vec.len() as i64
                } else {
                    d as i64
                }
            })
            .collect();
        let tensor = Tensor::from_array((shape.as_slice(), input_vec))
            .map_err(|e| WorkerError::Infer(format!("tensor: {e}")))?;
        let outputs = session
            .run(ort::inputs![input_name.as_str() => tensor])
            .map_err(|e| WorkerError::Infer(format!("run: {e}")))?;
        let out = outputs
            .get(output_name.as_str())
            .ok_or_else(|| WorkerError::Infer("model produced no named output".into()))?;
        let view = out
            .try_extract_array::<f32>()
            .map_err(|e| WorkerError::Infer(format!("extract: {e}")))?;
        Ok(view.iter().cloned().collect())
    }
}

impl OnnxCpu {
    /// Run inference and return ALL named outputs as a map.
    /// Needed for models like YuNet that produce multiple output heads.
    pub fn run_multi(
        &self,
        loaded: &Loaded,
        frame: &[u8],
    ) -> Result<std::collections::HashMap<String, Vec<f32>>, WorkerError> {
        let key = format!("{}:{:?}", loaded.model_id, loaded.quant);
        let cell = self
            .sessions
            .get(&key)
            .ok_or_else(|| WorkerError::Infer(format!("model not loaded: {key}")))?;
        let mut session = cell.borrow_mut();
        let input_vec: Vec<f32> = frame
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| WorkerError::Infer("model has no inputs".into()))?;
        let shape: Vec<i64> = loaded
            .input_shape
            .iter()
            .map(|&d| {
                if d == 0 {
                    input_vec.len() as i64
                } else {
                    d as i64
                }
            })
            .collect();
        let tensor = Tensor::from_array((shape.as_slice(), input_vec))
            .map_err(|e| WorkerError::Infer(format!("tensor: {e}")))?;
        let output_names: Vec<String> = session
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect();
        let outputs = session
            .run(ort::inputs![input_name.as_str() => tensor])
            .map_err(|e| WorkerError::Infer(format!("run: {e}")))?;
        let mut result = std::collections::HashMap::new();
        for name in &output_names {
            if let Some(val) = outputs.get(name.as_str()) {
                let view = val
                    .try_extract_array::<f32>()
                    .map_err(|e| WorkerError::Infer(format!("extract {name}: {e}")))?;
                result.insert(name.clone(), view.iter().cloned().collect());
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_capture::frame::{EmitterState, PixFmt};
    use namtor_capture::source::Spectrum;
    use std::fs;
    use std::path::PathBuf;

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("onnx")
            .join("identity.onnx")
    }

    /// Stage the identity fixture as `<root>/identity/fp32.onnx` so the
    /// `<root>/<id>/<quant>.onnx` path scheme resolves it.
    fn stage_identity(root: &std::path::Path) -> std::path::PathBuf {
        let dir = root.join("identity");
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("fp32.onnx");
        fs::copy(fixture_path(), &dest).unwrap();
        dest
    }

    fn frame_ref(len: u32) -> FrameRef {
        FrameRef {
            offset: 0,
            len,
            emitter: EmitterState::On,
            spectrum: Spectrum::Ir,
            pix_fmt: PixFmt::Grey8,
            w: len,
            h: 1,
            ts_us: 0,
        }
    }

    #[test]
    fn load_identity_and_run_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        stage_identity(tmp.path());
        let mut backend = OnnxCpu::new(1).with_root(tmp.path().to_path_buf());
        let spec = ModelSpec {
            id: "identity".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        };
        let loaded = backend.load(&spec).unwrap();
        assert_eq!(loaded.model_id, "identity");
        // Identity fixture: input [1, N] → output [1, N].
        // Pass f32 values as LE bytes (the contract: frame is preprocessed f32 data).
        let vals = [1.0_f32, 2.0, 3.0];
        let frame: Vec<u8> = vals.iter().flat_map(|v| v.to_le_bytes()).collect();
        let out = backend
            .run(&loaded, &frame, &frame_ref(vals.len() as u32))
            .unwrap();
        assert_eq!(out.len(), vals.len());
        for (i, (got, want)) in out.iter().zip(vals.iter()).enumerate() {
            assert!((got - want).abs() < 1e-5, "out[{i}] = {got}, want {want}");
        }
    }

    #[test]
    fn loaded_shapes_match_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        stage_identity(tmp.path());
        let mut backend = OnnxCpu::new(1).with_root(tmp.path().to_path_buf());
        let spec = ModelSpec {
            id: "identity".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        };
        let loaded = backend.load(&spec).unwrap();
        // Dynamic dim is reported as 0 by ort (unknown at load time).
        assert_eq!(loaded.input_shape[0], 1);
        assert_eq!(loaded.output_shape[0], 1);
    }

    #[test]
    fn load_missing_model_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let mut backend = OnnxCpu::new(1).with_root(tmp.path().to_path_buf());
        let spec = ModelSpec {
            id: "nope".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        };
        let err = backend.load(&spec).unwrap_err();
        assert!(matches!(err, WorkerError::Infer(_)), "got {err:?}");
    }

    #[test]
    fn quant_file_branch() {
        assert_eq!(OnnxCpu::quant_file(Quant::Fp32), "fp32.onnx");
        assert_eq!(OnnxCpu::quant_file(Quant::Int8), "int8.onnx");
    }

    #[test]
    fn run_without_load_errors() {
        let backend = OnnxCpu::new(1);
        let loaded = Loaded {
            model_id: "x".into(),
            quant: Quant::Fp32,
            input_shape: vec![1, 3],
            output_shape: vec![1, 3],
        };
        let err = backend.run(&loaded, &[0u8; 3], &frame_ref(3)).unwrap_err();
        assert!(matches!(err, WorkerError::Infer(_)), "got {err:?}");
    }
}
