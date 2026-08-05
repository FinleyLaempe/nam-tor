// SPDX-License-Identifier: GPL-3.0-only
//! Face detection via YuNet ONNX model.
//!
//! YuNet outputs face boxes + 5-point landmarks at 3 scales (stride 8/16/32).
//! Post-processing: score threshold → NMS → coordinate transform back to
//! original image space.

use namtor_proto::worker::{ModelSpec, Quant};
use namtor_worker::WorkerError;

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
    backend: namtor_worker::infer_onnx::OnnxCpu,
    loaded: namtor_proto::worker::Loaded,
    score_threshold: f32,
    nms_threshold: f32,
    input_size: (u32, u32),
}

impl FaceDetector {
    pub fn new(model_path: &std::path::Path, input_size: (u32, u32)) -> Result<Self, WorkerError> {
        let mut backend = namtor_worker::infer_onnx::OnnxCpu::new(1);
        let spec = ModelSpec {
            id: "yunet".into(),
            quant: Quant::Fp32,
            sha256: [0; 32],
        };
        let loaded = backend.load_file(model_path, &spec)?;
        Ok(Self {
            backend,
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

        let outputs = self.backend.run_multi(&self.loaded, &input)?;
        let detections = postprocess_multi(
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

pub fn preprocess(frame: &[u8], width: u32, height: u32, iw: usize, ih: usize) -> Vec<u8> {
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

fn postprocess_multi(
    outputs: &std::collections::HashMap<String, Vec<f32>>,
    iw: usize,
    ih: usize,
    orig_w: u32,
    orig_h: u32,
    score_thr: f32,
    nms_thr: f32,
) -> Vec<Detection> {
    let strides = [8usize, 16, 32];
    let mut candidates: Vec<Detection> = Vec::new();

    for &stride in &strides {
        let gw = iw / stride;
        let gh = ih / stride;
        let count = gw * gh;
        let s = stride as f32;

        let cls_key = format!("cls_{stride}");
        let obj_key = format!("obj_{stride}");
        let bbox_key = format!("bbox_{stride}");
        let kps_key = format!("kps_{stride}");

        let cls = outputs.get(&cls_key);
        let obj = outputs.get(&obj_key);
        let bbox = outputs.get(&bbox_key);
        let kps = outputs.get(&kps_key);

        if cls.is_none() || obj.is_none() || bbox.is_none() {
            continue;
        }

        let cls = cls.unwrap();
        let obj = obj.unwrap();
        let bbox = bbox.unwrap();

        for i in 0..count {
            let cls_val = cls[i];
            let obj_val = obj[i];
            let score = cls_val * obj_val;
            if score < score_thr {
                continue;
            }

            let cx = (i % gw) as f32 * s;
            let cy = (i / gw) as f32 * s;

            let l = bbox[i * 4] * s;
            let t = bbox[i * 4 + 1] * s;
            let r = bbox[i * 4 + 2] * s;
            let b = bbox[i * 4 + 3] * s;

            let x1 = cx - l;
            let y1 = cy - t;
            let x2 = cx + r;
            let y2 = cy + b;

            let x = x1 / iw as f32 * orig_w as f32;
            let y = y1 / ih as f32 * orig_h as f32;
            let w = (x2 - x1) / iw as f32 * orig_w as f32;
            let h = (y2 - y1) / ih as f32 * orig_h as f32;

            let mut landmarks = Landmarks {
                right_eye: (0.0, 0.0),
                left_eye: (0.0, 0.0),
                nose: (0.0, 0.0),
                right_mouth: (0.0, 0.0),
                left_mouth: (0.0, 0.0),
            };

            if let Some(kps) = kps {
                let scale_x = orig_w as f32 / iw as f32;
                let scale_y = orig_h as f32 / ih as f32;
                let points: [(f32, f32); 5] = [
                    (
                        (cx + kps[i * 10] * s) * scale_x,
                        (cy + kps[i * 10 + 1] * s) * scale_y,
                    ),
                    (
                        (cx + kps[i * 10 + 2] * s) * scale_x,
                        (cy + kps[i * 10 + 3] * s) * scale_y,
                    ),
                    (
                        (cx + kps[i * 10 + 4] * s) * scale_x,
                        (cy + kps[i * 10 + 5] * s) * scale_y,
                    ),
                    (
                        (cx + kps[i * 10 + 6] * s) * scale_x,
                        (cy + kps[i * 10 + 7] * s) * scale_y,
                    ),
                    (
                        (cx + kps[i * 10 + 8] * s) * scale_x,
                        (cy + kps[i * 10 + 9] * s) * scale_y,
                    ),
                ];
                landmarks = Landmarks {
                    right_eye: points[0],
                    left_eye: points[1],
                    nose: points[2],
                    right_mouth: points[3],
                    left_mouth: points[4],
                };
            }

            candidates.push(Detection {
                box_: FaceBox { x, y, w, h, score },
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
