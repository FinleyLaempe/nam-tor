// SPDX-License-Identifier: GPL-3.0-only
//! `OnnxCpu` backend placeholder.
//!
//! The real `ort`-backed implementation lands in Task 5. This stub exists so
//! `default_backend()` compiles in Task 4 when the `mock-infer` feature is OFF.
//! Task 5 replaces this entire module with the `ort`-based implementation.

use namtor_proto::worker::{FrameRef, Loaded, ModelSpec};

use crate::infer::InferenceBackend;
use crate::WorkerError;

/// Placeholder CPU ONNX backend. Task 5 provides the real `ort`-backed impl.
pub struct OnnxCpu {
    threads: usize,
}

impl OnnxCpu {
    pub fn new(threads: usize) -> Self {
        Self { threads }
    }

    #[allow(dead_code)]
    pub fn threads(&self) -> usize {
        self.threads
    }
}

impl InferenceBackend for OnnxCpu {
    fn load(&mut self, spec: &ModelSpec) -> Result<Loaded, WorkerError> {
        Err(WorkerError::Infer(format!(
            "OnnxCpu not yet implemented (Task 5); cannot load {}",
            spec.id
        )))
    }

    fn run(
        &self,
        _loaded: &Loaded,
        _frame: &[u8],
        _meta: &FrameRef,
    ) -> Result<Vec<f32>, WorkerError> {
        Err(WorkerError::Infer(
            "OnnxCpu not yet implemented (Task 5)".into(),
        ))
    }
}
