// SPDX-License-Identifier: GPL-3.0-only
//! Worker wire: daemon↔worker pipe messages.
//!
//! The daemon sends [`WorkerReq`] (load a model, infer on a frame, shut down);
//! the worker replies with [`WorkerRsp`]. Frame *pixels* do not travel this
//! wire — they live in a shared `memfd` region the daemon creates and the
//! worker mmaps read-only before applying its sandbox. [`FrameRef`] indexes
//! into that region with `offset`/`len`.

use namtor_capture::frame::{EmitterState, PixFmt};
use namtor_capture::source::Spectrum;
use serde::{Deserialize, Serialize};

/// Quantisation mode a model is loaded under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quant {
    /// Full-precision FP32.
    Fp32,
    /// Static INT8 (AVX-VNNI on the reference CPU).
    Int8,
}

/// A model to load, identified by a stable id whose on-disk path the worker
/// resolves. `sha256` is the expected content hash; the worker verifies the
/// file before loading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub id: String,
    pub quant: Quant,
    pub sha256: [u8; 32],
}

/// Metadata describing where a frame's bytes live in the shared memfd region
/// and how to interpret them. No pixel data is serialised here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRef {
    /// Byte offset into the shared memfd region.
    pub offset: u64,
    /// Byte length of this frame's pixel data.
    pub len: u32,
    /// Emitter state when the frame was exposed.
    pub emitter: EmitterState,
    /// Which sensor produced the frame.
    pub spectrum: Spectrum,
    /// Pixel format of the frame bytes.
    pub pix_fmt: PixFmt,
    /// Frame width in pixels.
    pub w: u32,
    /// Frame height in pixels.
    pub h: u32,
    /// Frame timestamp in microseconds since capture start
    /// (`Frame.ts_mono.as_micros() as u64` on the daemon side).
    pub ts_us: u64,
}

/// Result of loading a model: the shapes the worker observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loaded {
    pub model_id: String,
    pub quant: Quant,
    pub input_shape: Vec<u64>,
    pub output_shape: Vec<u64>,
}

/// Daemon → worker request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerReq {
    /// Load a model; worker replies [`WorkerRsp::Loaded`] or
    /// [`WorkerRsp::Error`].
    Load(ModelSpec),
    /// Run inference on the frame described by this [`FrameRef`]; worker
    /// replies [`WorkerRsp::Vector`] or [`WorkerRsp::Error`].
    Infer(FrameRef),
    /// Clean shutdown; worker exits after ack (no reply).
    Shutdown,
}

/// Worker → daemon reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerRsp {
    Loaded(Loaded),
    Vector { vec: Vec<f32> },
    Error { msg: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_spec() -> ModelSpec {
        ModelSpec {
            id: "yunet-202608".into(),
            quant: Quant::Fp32,
            sha256: [0xab; 32],
        }
    }

    fn dummy_frame_ref() -> FrameRef {
        FrameRef {
            offset: 4096,
            len: 640 * 360,
            emitter: EmitterState::On,
            spectrum: Spectrum::Ir,
            pix_fmt: PixFmt::Grey8,
            w: 640,
            h: 360,
            ts_us: 1_234_567,
        }
    }

    fn round_trip<T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(
        v: &T,
    ) -> T {
        let mut buf = Vec::new();
        crate::framing::encode(v, &mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let val = crate::framing::decode(&mut cursor).unwrap();
        serde_json::from_value(val).unwrap()
    }

    #[test]
    fn req_load_round_trip() {
        let req = WorkerReq::Load(dummy_spec());
        assert_eq!(round_trip(&req), req);
    }

    #[test]
    fn req_infer_round_trip() {
        let req = WorkerReq::Infer(dummy_frame_ref());
        assert_eq!(round_trip(&req), req);
    }

    #[test]
    fn req_shutdown_round_trip() {
        let req = WorkerReq::Shutdown;
        assert_eq!(round_trip(&req), req);
    }

    #[test]
    fn rsp_loaded_round_trip() {
        let rsp = WorkerRsp::Loaded(Loaded {
            model_id: "mfnet".into(),
            quant: Quant::Int8,
            input_shape: vec![1, 112, 112, 3],
            output_shape: vec![1, 192],
        });
        assert_eq!(round_trip(&rsp), rsp);
    }

    #[test]
    fn rsp_vector_round_trip() {
        let rsp = WorkerRsp::Vector {
            vec: vec![0.1, 0.2, -0.3, 1.0],
        };
        assert_eq!(round_trip(&rsp), rsp);
    }

    #[test]
    fn rsp_error_round_trip() {
        let rsp = WorkerRsp::Error {
            msg: "model not found".into(),
        };
        assert_eq!(round_trip(&rsp), rsp);
    }

    #[test]
    fn frame_ref_preserves_all_fields() {
        let fr = dummy_frame_ref();
        let back = round_trip(&fr);
        assert_eq!(back, fr);
        assert_eq!(back.emitter, EmitterState::On);
        assert_eq!(back.spectrum, Spectrum::Ir);
        assert_eq!(back.pix_fmt, PixFmt::Grey8);
    }

    #[test]
    fn quant_serde_snake_case() {
        assert_eq!(serde_json::to_string(&Quant::Int8).unwrap(), r#""int8""#);
        assert_eq!(serde_json::to_string(&Quant::Fp32).unwrap(), r#""fp32""#);
    }

    #[test]
    fn worker_req_tag_is_kind() {
        let req = WorkerReq::Shutdown;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""kind":"shutdown""#), "{json}");
    }
}
