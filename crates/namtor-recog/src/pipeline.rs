// SPDX-License-Identifier: GPL-3.0-only
//! Full recognition pipeline: detect → align → embed.
//!
//! Wraps [`FaceDetector`] and [`FaceEmbedder`] into a single call that takes
//! a raw frame and returns an L2-normalised embedding vector.

use crate::align::align_face;
use crate::detect::{Detection, FaceDetector};
use crate::embed::{Embedding, FaceEmbedder};
use namtor_worker::WorkerError;

pub struct PipelineResult {
    pub detection: Detection,
    pub embedding: Embedding,
}

pub struct RecognitionPipeline {
    pub detector: FaceDetector,
    embedder: FaceEmbedder,
}

impl RecognitionPipeline {
    pub fn new(
        yunet_path: &std::path::Path,
        sface_path: &std::path::Path,
        input_size: (u32, u32),
    ) -> Result<Self, WorkerError> {
        let detector = FaceDetector::new(yunet_path, input_size)?;
        let embedder = FaceEmbedder::new(sface_path)?;
        Ok(Self { detector, embedder })
    }

    pub fn detect(&mut self, frame: &[u8], w: u32, h: u32) -> Result<Vec<Detection>, WorkerError> {
        self.detector.detect(frame, w, h)
    }

    pub fn embed_crop(&mut self, crop_112: &[u8]) -> Result<Embedding, WorkerError> {
        self.embedder.embed(crop_112)
    }

    pub fn process_frame(
        &mut self,
        frame: &[u8],
        w: u32,
        h: u32,
    ) -> Result<Option<PipelineResult>, WorkerError> {
        let detections = self.detector.detect(frame, w, h)?;
        let best = match detections.into_iter().max_by(|a, b| {
            a.box_
                .score
                .partial_cmp(&b.box_.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        }) {
            Some(d) if d.box_.score > 0.5 => d,
            _ => return Ok(None),
        };
        let crop = align_face(frame, w, h, &best.landmarks);
        let embedding = self.embedder.embed(&crop)?;
        Ok(Some(PipelineResult {
            detection: best,
            embedding,
        }))
    }

    pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na > 1e-10 && nb > 1e-10 {
            dot / (na * nb)
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_identical() {
        let v = vec![1.0, 0.0, 0.0];
        let s = RecognitionPipeline::cosine_similarity(&v, &v);
        assert!((s - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let s = RecognitionPipeline::cosine_similarity(&a, &b);
        assert!(s.abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_negative() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let s = RecognitionPipeline::cosine_similarity(&a, &b);
        assert!((s + 1.0).abs() < 1e-5);
    }
}
