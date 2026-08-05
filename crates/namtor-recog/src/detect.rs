// SPDX-License-Identifier: GPL-3.0-only
//! Face detection via YuNet ONNX model.
//!
//! YuNet outputs face boxes + 5-point landmarks at 3 scales (stride 8/16/32).
//! Post-processing: score threshold → NMS → coordinate transform back to
//! original image space.

use namtor_proto::worker::{FrameRef, ModelSpec, Quant};
use namtor_worker::infer::InferenceBackend;
use namtor_worker::WorkerError;

use namtor_capture::frame::{EmitterState, PixFmt};
use namtor_capture::source::Spectrum;

#[derive(Debug, Clone, PartialEq)]
pub struct FaceBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Landmarks {
    pub right_eye: (f32, f32),
    pub left_eye: (f32, f32),
    pub nose: (f32, f32),
    pub right_mouth: (f32, f32),
    pub left_mouth: (f32, f32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub box_: FaceBox,
    pub landmarks: Landmarks,
}

pub struct FaceDetector {
    backend: Box<dyn InferenceBackend>,
    loaded: namtor_proto::worker::Loaded,
    score_threshold: f32,
    nms_threshold: f32,
    input_size: (u32, u32),
}

impl FaceDetector {
    pub fn new(model_path: &std::path::Path, input_size: (u32, u32)) -> Result<Self, WorkerError> {
        let mut backend = namtor_worker::infer_onnx::OnnxCpu::new(1).with_root(
            model_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf(),
        );
        let spec = ModelSpec {
            id: "yunet".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        };
        let loaded = backend.load(&spec)?;
        Ok(Self {
            backend: Box::new(backend),
            loaded,
            score_threshold: 0.6,
            nms_threshold: 0.3,
            input_size,
        })
    }

    pub fn with_thresholds(mut self, score: f32, nms: f32) -> Self {
        self.score_threshold = score;
        self.nms_threshold = nms;
        self
    }

    pub fn detect(
        &mut self,
        frame: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Vec<Detection>, WorkerError> {
        let (iw, ih) = (self.input_size.0 as usize, self.input_size.1 as usize);
        let input = preprocess(frame, width, height, iw, ih);

        let frame_ref = FrameRef {
            offset: 0,
            len: input.len() as u32,
            emitter: EmitterState::On,
            spectrum: Spectrum::Ir,
            pix_fmt: PixFmt::Grey8,
            w: iw as u32,
            h: ih as u32,
            ts_us: 0,
        };

        let outputs = self.backend.run(&self.loaded, &input, &frame_ref)?;
        let detections = postprocess(
            &outputs,
            iw,
            ih,
            width,
            height,
            self.score_threshold,
            self.nms_threshold,
        );
        Ok(detections)
    }
}

fn preprocess(frame: &[u8], width: u32, height: u32, iw: usize, ih: usize) -> Vec<u8> {
    let mut input = vec![0f32; 3 * iw * ih];
    let sx = width as f32 / iw as f32;
    let sy = height as f32 / ih as f32;
    for y in 0..ih {
        for x in 0..iw {
            let src_x = (x as f32 * sx) as usize;
            let src_y = (y as f32 * sy) as usize;
            let src_idx = src_y * width as usize + src_x;
            let gray = if src_idx < frame.len() {
                frame[src_idx] as f32
            } else {
                0.0
            };
            let normalized = (gray - 127.5) / 128.0;
            for c in 0..3 {
                input[c * iw * ih + y * iw + x] = normalized;
            }
        }
    }
    let mut bytes = Vec::with_capacity(input.len() * 4);
    for v in &input {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

fn postprocess(
    outputs: &[f32],
    iw: usize,
    ih: usize,
    orig_w: u32,
    orig_h: u32,
    score_thr: f32,
    nms_thr: f32,
) -> Vec<Detection> {
    let strides = [8, 16, 32];
    let mut candidates: Vec<Detection> = Vec::new();

    let mut offset = 0;
    for &stride in &strides {
        let gw = iw / stride;
        let gh = ih / stride;
        let count = gw * gh;

        let cls_off = offset;
        let bbox_off = cls_off + count * 2;
        let kps_off = bbox_off + count * 4 * 2;
        offset = kps_off + count * 10 * 2;

        for i in 0..count {
            let cls_score = outputs[cls_off + i * 2 + 1];
            if cls_score < score_thr {
                continue;
            }
            let cx = (i % gw) as f32 * stride as f32;
            let cy = (i / gw) as f32 * stride as f32;

            let bx = (outputs[bbox_off + i * 8] * stride as f32) + cx;
            let by = (outputs[bbox_off + i * 8 + 1] * stride as f32) + cy;
            let bw = outputs[bbox_off + i * 8 + 2] * stride as f32;
            let bh = outputs[bbox_off + i * 8 + 3] * stride as f32;

            let x = (bx - bw / 2.0) / iw as f32 * orig_w as f32;
            let y = (by - bh / 2.0) / ih as f32 * orig_h as f32;
            let w = bw / iw as f32 * orig_w as f32;
            let h = bh / ih as f32 * orig_h as f32;

            let kps = [
                (
                    outputs[kps_off + i * 10] * stride as f32 + cx,
                    outputs[kps_off + i * 10 + 1] * stride as f32 + cy,
                ),
                (
                    outputs[kps_off + i * 10 + 2] * stride as f32 + cx,
                    outputs[kps_off + i * 10 + 3] * stride as f32 + cy,
                ),
                (
                    outputs[kps_off + i * 10 + 4] * stride as f32 + cx,
                    outputs[kps_off + i * 10 + 5] * stride as f32 + cy,
                ),
                (
                    outputs[kps_off + i * 10 + 6] * stride as f32 + cx,
                    outputs[kps_off + i * 10 + 7] * stride as f32 + cy,
                ),
                (
                    outputs[kps_off + i * 10 + 8] * stride as f32 + cx,
                    outputs[kps_off + i * 10 + 9] * stride as f32 + cy,
                ),
            ];
            let scale_x = orig_w as f32 / iw as f32;
            let scale_y = orig_h as f32 / ih as f32;
            let landmarks = Landmarks {
                right_eye: (kps[0].0 * scale_x, kps[0].1 * scale_y),
                left_eye: (kps[1].0 * scale_x, kps[1].1 * scale_y),
                nose: (kps[2].0 * scale_x, kps[2].1 * scale_y),
                right_mouth: (kps[3].0 * scale_x, kps[3].1 * scale_y),
                left_mouth: (kps[4].0 * scale_x, kps[4].1 * scale_y),
            };

            candidates.push(Detection {
                box_: FaceBox {
                    x,
                    y,
                    w,
                    h,
                    score: cls_score,
                },
                landmarks,
            });
        }
    }

    nms(&mut candidates, nms_thr);
    candidates
}

fn nms(dets: &mut Vec<Detection>, threshold: f32) {
    dets.sort_by(|a, b| {
        b.box_
            .score
            .partial_cmp(&a.box_.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut keep = Vec::new();
    while !dets.is_empty() {
        let best = dets.remove(0);
        keep.push(best.clone());
        dets.retain(|d| iou(&best.box_, &d.box_) < threshold);
    }
    *dets = keep;
}

fn iou(a: &FaceBox, b: &FaceBox) -> f32 {
    let ax2 = a.x + a.w;
    let ay2 = a.y + a.h;
    let bx2 = b.x + b.w;
    let by2 = b.y + b.h;
    let ix1 = a.x.max(b.x);
    let iy1 = a.y.max(b.y);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    let intersection = iw * ih;
    let union = a.w * a.h + b.w * b.h - intersection;
    if union > 0.0 {
        intersection / union
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iou_identical_boxes() {
        let a = FaceBox {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
            score: 1.0,
        };
        let b = FaceBox {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
            score: 1.0,
        };
        assert!((iou(&a, &b) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn iou_non_overlapping() {
        let a = FaceBox {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
            score: 1.0,
        };
        let b = FaceBox {
            x: 20.0,
            y: 20.0,
            w: 10.0,
            h: 10.0,
            score: 1.0,
        };
        assert!((iou(&a, &b)).abs() < 1e-5);
    }

    #[test]
    fn nms_removes_overlapping() {
        let mut dets = vec![
            Detection {
                box_: FaceBox {
                    x: 0.0,
                    y: 0.0,
                    w: 10.0,
                    h: 10.0,
                    score: 0.9,
                },
                landmarks: Landmarks {
                    right_eye: (0.0, 0.0),
                    left_eye: (0.0, 0.0),
                    nose: (0.0, 0.0),
                    right_mouth: (0.0, 0.0),
                    left_mouth: (0.0, 0.0),
                },
            },
            Detection {
                box_: FaceBox {
                    x: 1.0,
                    y: 1.0,
                    w: 10.0,
                    h: 10.0,
                    score: 0.8,
                },
                landmarks: Landmarks {
                    right_eye: (0.0, 0.0),
                    left_eye: (0.0, 0.0),
                    nose: (0.0, 0.0),
                    right_mouth: (0.0, 0.0),
                    left_mouth: (0.0, 0.0),
                },
            },
        ];
        nms(&mut dets, 0.3);
        assert_eq!(dets.len(), 1);
        assert!((dets[0].box_.score - 0.9).abs() < 1e-5);
    }
}
