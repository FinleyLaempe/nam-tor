// SPDX-License-Identifier: GPL-3.0-only
//! Production [`EmbedSource`]: the recognition pipeline behind the enrollment
//! session's "frame → embedding + measurements" seam.
//!
//! [`PipelineEmbedSource`] runs detect → align → embed over a captured frame
//! and then measures that same frame against the detection it produced, so
//! the guidance the user sees is derived from the pixels the embedding was
//! actually computed from. Before this existed there was no production
//! `impl EmbedSource` at all — only test doubles — so `Manager3` enrollment
//! could not capture a single frame.
//!
//! Privacy: frames, crops and embeddings never leave this module's caller.
//! [`EnrollmentCapture`](crate::manager::session::EnrollmentCapture) zeroizes
//! every buffer it holds, and the only projection that crosses D-Bus is
//! `GuidanceProgress`.

use namtor_capture::frame::{Frame, PixFmt};
use namtor_recog::pipeline::{PipelineResult, RecognitionPipeline};
use namtor_recog::{quality, FrameMetrics};
use namtor_worker::WorkerError;

use crate::dbus::ApiError;
use crate::manager::session::EmbedSource;

/// The detect → align → embed step, abstracted so [`PipelineEmbedSource`] can
/// be tested without ONNX weights on disk.
///
/// The one production implementor is [`RecognitionPipeline`]; the signature is
/// deliberately identical to
/// [`RecognitionPipeline::process_frame`] so the impl is a straight forward.
pub trait FramePipeline: Send {
    fn process_frame(
        &mut self,
        frame: &[u8],
        w: u32,
        h: u32,
    ) -> Result<Option<PipelineResult>, WorkerError>;
}

impl FramePipeline for RecognitionPipeline {
    fn process_frame(
        &mut self,
        frame: &[u8],
        w: u32,
        h: u32,
    ) -> Result<Option<PipelineResult>, WorkerError> {
        RecognitionPipeline::process_frame(self, frame, w, h)
    }
}

/// [`EmbedSource`] backed by a real recognition pipeline.
pub struct PipelineEmbedSource<P: FramePipeline> {
    pipeline: P,
}

impl<P: FramePipeline> PipelineEmbedSource<P> {
    pub fn new(pipeline: P) -> Self {
        Self { pipeline }
    }
}

impl PipelineEmbedSource<RecognitionPipeline> {
    /// Load the YuNet + SFace weights and wrap them. The 640×640 detector
    /// input matches the auth path (`VerifyEngine::new`), so enrollment and
    /// verification see the same detector geometry.
    pub fn load(
        yunet_path: &std::path::Path,
        sface_path: &std::path::Path,
    ) -> Result<Self, ApiError> {
        let pipeline = RecognitionPipeline::new(yunet_path, sface_path, (640, 640))
            .map_err(|e| ApiError::State(format!("recognition pipeline load: {e}")))?;
        Ok(Self::new(pipeline))
    }
}

impl<P: FramePipeline> EmbedSource for PipelineEmbedSource<P> {
    fn embed(&mut self, frame: &Frame) -> Result<Option<(Vec<f32>, FrameMetrics)>, ApiError> {
        // The detector reads the buffer as tightly packed 8-bit greyscale.
        // Handing it anything else would not fail loudly — it would detect
        // faces in reinterpreted chroma — so the format is checked, not
        // assumed.
        if frame.format != PixFmt::Grey8 {
            return Err(ApiError::State(format!(
                "enrollment needs a greyscale frame, got {:?}",
                frame.format
            )));
        }
        let needed = (frame.width as usize).saturating_mul(frame.height as usize);
        if needed == 0 || frame.data.len() < needed {
            return Err(ApiError::State(format!(
                "frame buffer holds {} samples, geometry declares {needed}",
                frame.data.len()
            )));
        }
        let result = self
            .pipeline
            .process_frame(&frame.data, frame.width, frame.height)
            .map_err(|e| ApiError::State(format!("recognition pipeline: {e}")))?;
        // `Ok(None)` is the "no face detected" path, which the session counts
        // as `rejected_no_face` rather than dropping silently.
        let Some(PipelineResult {
            detection,
            embedding,
        }) = result
        else {
            return Ok(None);
        };
        let metrics = quality::measure(&frame.data, frame.width, frame.height, &detection);
        Ok(Some((embedding.vector, metrics)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_capture::frame::{EmitterState, Frame, PixFmt};
    use namtor_recog::detect::{Detection, FaceBox, Landmarks};
    use namtor_recog::embed::Embedding;
    use namtor_recog::pipeline::PipelineResult;
    use namtor_worker::WorkerError;
    use std::time::Duration;

    fn grey_frame(w: u32, h: u32, fill: u8) -> Frame {
        Frame {
            data: vec![fill; (w * h) as usize],
            format: PixFmt::Grey8,
            width: w,
            height: h,
            ts_mono: Duration::from_millis(0),
            sof: None,
            emitter: EmitterState::On,
            quality: None,
        }
    }

    fn centered_detection(w: u32, h: u32) -> Detection {
        let (bw, bh) = (w as f32 * 0.4, h as f32 * 0.5);
        let (x, y) = ((w as f32 - bw) * 0.5, (h as f32 - bh) * 0.5);
        let eye_y = y + bh * 0.35;
        Detection {
            box_: FaceBox {
                x,
                y,
                w: bw,
                h: bh,
                score: 0.93,
            },
            landmarks: Landmarks {
                right_eye: (x + bw * 0.3, eye_y),
                left_eye: (x + bw * 0.7, eye_y),
                nose: (x + bw * 0.5, y + bh * 0.55),
                right_mouth: (x + bw * 0.35, y + bh * 0.75),
                left_mouth: (x + bw * 0.65, y + bh * 0.75),
            },
        }
    }

    /// Pipeline stub: no ONNX weights, no camera.
    enum StubPipeline {
        Nothing,
        Detects(Detection),
        Fails,
    }

    impl FramePipeline for StubPipeline {
        fn process_frame(
            &mut self,
            _frame: &[u8],
            _w: u32,
            _h: u32,
        ) -> Result<Option<PipelineResult>, WorkerError> {
            match self {
                Self::Nothing => Ok(None),
                Self::Detects(d) => Ok(Some(PipelineResult {
                    detection: d.clone(),
                    embedding: Embedding {
                        vector: vec![0.1; 128],
                    },
                })),
                Self::Fails => Err(WorkerError::Infer("inference exploded".into())),
            }
        }
    }

    #[test]
    fn pipeline_embed_source_returns_none_when_no_face_detected() {
        let mut src = PipelineEmbedSource::new(StubPipeline::Nothing);
        let frame = grey_frame(640, 360, 130);
        assert!(src.embed(&frame).expect("embed").is_none());
    }

    #[test]
    fn pipeline_embed_source_returns_embedding_and_metrics_on_detection() {
        let mut src = PipelineEmbedSource::new(StubPipeline::Detects(centered_detection(640, 360)));
        let frame = grey_frame(640, 360, 130);
        let (embedding, metrics) = src.embed(&frame).expect("embed").expect("detection");
        assert_eq!(embedding.len(), 128);
        assert!(metrics.detection_confidence > 0.0);
        // The metrics must describe THIS frame, not a constant: a uniform
        // mid-grey fill is perfectly exposed and perfectly unsharp.
        assert!(metrics.illumination > 0.9, "metrics {metrics:?}");
        assert_eq!(metrics.blur, 0.0, "metrics {metrics:?}");
        assert!(metrics.landmark_spread > 0.0, "metrics {metrics:?}");
        assert!(metrics.face_fraction > 0.0, "metrics {metrics:?}");
    }

    #[test]
    fn pipeline_embed_source_reports_inference_failure_as_an_error() {
        let mut src = PipelineEmbedSource::new(StubPipeline::Fails);
        let frame = grey_frame(640, 360, 130);
        assert!(src.embed(&frame).is_err());
    }

    #[test]
    fn pipeline_embed_source_rejects_a_non_greyscale_frame() {
        let mut src = PipelineEmbedSource::new(StubPipeline::Detects(centered_detection(64, 64)));
        let mut frame = grey_frame(64, 64, 130);
        frame.format = PixFmt::Yuyv;
        frame.data = vec![130; 64 * 64 * 2];
        assert!(
            src.embed(&frame).is_err(),
            "a YUYV buffer must not be fed to a greyscale detector"
        );
    }

    #[test]
    fn pipeline_embed_source_rejects_a_frame_shorter_than_its_geometry() {
        let mut src = PipelineEmbedSource::new(StubPipeline::Detects(centered_detection(64, 64)));
        let mut frame = grey_frame(64, 64, 130);
        frame.data.truncate(100);
        assert!(src.embed(&frame).is_err());
    }
}
