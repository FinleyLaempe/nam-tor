// SPDX-License-Identifier: GPL-3.0-only
//! Face recognition pipeline: detect → align → embed.
//!
//! Uses YuNet (from OpenCV Zoo, BSD-3-Clause) for face detection with 5-point
//! landmarks, a similarity-transform alignment to 112×112, and SFace (also
//! OpenCV Zoo) for 128-dimensional L2-normalised embeddings.

pub mod align;
pub mod detect;
pub mod embed;
pub mod pipeline;

pub use align::align_face;
pub use detect::{Detection, FaceBox, FaceDetector, Landmarks};
pub use embed::{Embedding, FaceEmbedder};
pub use pipeline::{PipelineResult, RecognitionPipeline};
