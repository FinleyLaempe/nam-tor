// SPDX-License-Identifier: GPL-3.0-only
//! Face embedding via SFace ONNX model (128-dimensional, L2-normalised).

use namtor_capture::frame::{EmitterState, PixFmt};
use namtor_capture::source::Spectrum;
use namtor_proto::worker::{FrameRef, ModelSpec, Quant};
use namtor_worker::infer::InferenceBackend;
use namtor_worker::WorkerError;

pub struct Embedding {
    pub vector: Vec<f32>,
}

pub struct FaceEmbedder {
    backend: Box<dyn InferenceBackend>,
    loaded: namtor_proto::worker::Loaded,
}

impl FaceEmbedder {
    pub fn new(model_path: &std::path::Path) -> Result<Self, WorkerError> {
        let mut backend = namtor_worker::infer_onnx::OnnxCpu::new(1).with_root(
            model_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf(),
        );
        let spec = ModelSpec {
            id: "sface".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        };
        let loaded = backend.load(&spec)?;
        Ok(Self {
            backend: Box::new(backend),
            loaded,
        })
    }

    pub fn embed(&mut self, crop_112: &[u8]) -> Result<Embedding, WorkerError> {
        let input = preprocess(crop_112);
        let frame_ref = FrameRef {
            offset: 0,
            len: input.len() as u32,
            emitter: EmitterState::On,
            spectrum: Spectrum::Ir,
            pix_fmt: PixFmt::Grey8,
            w: 112,
            h: 112,
            ts_us: 0,
        };
        let raw = self.backend.run(&self.loaded, &input, &frame_ref)?;
        let mut vector = raw;
        l2_normalize(&mut vector);
        Ok(Embedding { vector })
    }
}

fn preprocess(crop: &[u8]) -> Vec<u8> {
    let mut input = vec![0f32; 3 * 112 * 112];
    for i in 0..112 * 112 {
        let gray = if i < crop.len() { crop[i] as f32 } else { 0.0 };
        let normalized = (gray - 127.5) / 127.5;
        for c in 0..3 {
            input[c * 112 * 112 + i] = normalized;
        }
    }
    let mut bytes = Vec::with_capacity(input.len() * 4);
    for v in &input {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-10 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_length() {
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn l2_normalize_zero_vector() {
        let mut v = vec![0.0, 0.0, 0.0];
        l2_normalize(&mut v);
        assert_eq!(v, vec![0.0, 0.0, 0.0]);
    }
}
