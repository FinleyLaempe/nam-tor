// SPDX-License-Identifier: GPL-3.0-only
//! The ONNX stack, run against the real shipped models.
//!
//! Every other test in the workspace exercises the pipeline through mocks or
//! synthetic embedders. Nothing loaded `yunet_2023mar.onnx`,
//! `sface_2021dec.onnx` or `minifasnet_pad.onnx` and ran a tensor through
//! them, so a model whose input shape, output arity or opset the runtime
//! refused would have shipped: the failure would land on the first person to
//! press "Scan my face", not in CI.
//!
//! `#[ignore]` by default, following the convention the camera tests use:
//! the weights are licensed artefacts and are deliberately not in the repo,
//! and a test that quietly passes without doing anything reads as coverage
//! it does not have. Run them with
//!
//! ```text
//! NAMTOR_MODELS_DIR=/var/lib/namtor/models \
//!     cargo test -p namtor-recog --test real_models_infer -- --ignored
//! ```
//!
//! The release workflow runs exactly that against the model archive it is
//! about to ship, so weights that do not infer block the release.

use std::path::PathBuf;

/// The models directory, or `None` when this machine has no models staged.
fn models_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("NAMTOR_MODELS_DIR")?);
    dir.join("yunet_2023mar.onnx").exists().then_some(dir)
}

macro_rules! models_or_skip {
    () => {
        match models_dir() {
            Some(d) => d,
            None => {
                eprintln!("SKIP: set NAMTOR_MODELS_DIR to a directory holding the ONNX models");
                return;
            }
        }
    };
}

/// A grey frame with a face-shaped bright region: an oval head, two dark eye
/// sockets and a dark mouth, on a dark background. It is not a person and
/// YuNet is not expected to accept it — what is under test is that a real
/// frame's worth of bytes goes in and well-formed detections come out.
fn face_like_frame(w: u32, h: u32) -> Vec<u8> {
    let (fw, fh) = (w as f32, h as f32);
    let (cx, cy) = (fw / 2.0, fh / 2.0);
    let (rx, ry) = (fw * 0.18, fh * 0.30);
    let mut buf = vec![24u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = ((x as f32 - cx) / rx, (y as f32 - cy) / ry);
            if dx * dx + dy * dy > 1.0 {
                continue;
            }
            let mut v = 190.0 - 40.0 * (dx * dx + dy * dy);
            for (ex, ey, er) in [
                (cx - rx * 0.40, cy - ry * 0.25, rx * 0.16),
                (cx + rx * 0.40, cy - ry * 0.25, rx * 0.16),
                (cx, cy + ry * 0.45, rx * 0.30),
            ] {
                let (ddx, ddy) = (x as f32 - ex, y as f32 - ey);
                if ddx * ddx + ddy * ddy < er * er {
                    v = 45.0;
                }
            }
            buf[(y * w + x) as usize] = v as u8;
        }
    }
    buf
}

#[test]
#[ignore = "requires the ONNX models; run with NAMTOR_MODELS_DIR=<dir> -- --ignored"]
fn yunet_loads_and_runs_on_a_real_frame() {
    let dir = models_or_skip!();
    let mut det =
        namtor_recog::detect::FaceDetector::new(&dir.join("yunet_2023mar.onnx"), (640, 640))
            .expect("YuNet must load: a model the runtime refuses is a dead install");

    let (w, h) = (640u32, 360u32);
    let frame = face_like_frame(w, h);
    let dets = det
        .detect(&frame, w, h)
        .expect("detection must not fail on a well-formed GREY frame");

    // Whatever it finds, every detection must be self-consistent and inside
    // the frame — a coordinate transform that silently produced NaN or
    // off-image boxes would misalign every crop fed to SFace.
    for d in &dets {
        assert!(d.box_.score.is_finite() && d.box_.score > 0.0, "{d:?}");
        assert!(d.box_.w > 0.0 && d.box_.h > 0.0, "{d:?}");
        for (x, y) in [
            d.landmarks.right_eye,
            d.landmarks.left_eye,
            d.landmarks.nose,
            d.landmarks.right_mouth,
            d.landmarks.left_mouth,
        ] {
            assert!(x.is_finite() && y.is_finite(), "{d:?}");
            assert!(
                x >= -(w as f32) && x <= 2.0 * w as f32,
                "landmark far outside the frame: {d:?}"
            );
            assert!(y >= -(h as f32) && y <= 2.0 * h as f32, "{d:?}");
        }
    }
}

/// A detector built for a size the model does not accept used to load
/// happily and then fail on every frame with a tensor-length message that
/// named neither the model nor the setting. It is refused at load now.
#[test]
#[ignore = "requires the ONNX models; run with NAMTOR_MODELS_DIR=<dir> -- --ignored"]
fn a_detector_built_for_the_wrong_input_size_is_refused_at_load() {
    let dir = models_or_skip!();
    let path = dir.join("yunet_2023mar.onnx");
    let err = match namtor_recog::detect::FaceDetector::new(&path, (320, 320)) {
        Ok(_) => panic!("a 320x320 detector for a 640x640 model must not load"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(msg.contains("640x640"), "{msg}");
    assert!(msg.contains("320x320"), "{msg}");
    assert!(
        msg.contains("yunet"),
        "the message must name the model: {msg}"
    );
}

#[test]
#[ignore = "requires the ONNX models; run with NAMTOR_MODELS_DIR=<dir> -- --ignored"]
fn a_blank_frame_yields_no_face() {
    let dir = models_or_skip!();
    let mut det =
        namtor_recog::detect::FaceDetector::new(&dir.join("yunet_2023mar.onnx"), (640, 640))
            .expect("YuNet must load");
    let dets = det
        .detect(&vec![0u8; 640 * 360], 640, 360)
        .expect("detection must not fail on a black frame");
    assert!(
        dets.is_empty(),
        "a black frame produced {} detections; enrolling on noise would \
         store templates of nothing",
        dets.len()
    );
}

#[test]
#[ignore = "requires the ONNX models; run with NAMTOR_MODELS_DIR=<dir> -- --ignored"]
fn sface_produces_a_normalised_embedding() {
    let dir = models_or_skip!();
    let mut emb = namtor_recog::embed::FaceEmbedder::new(&dir.join("sface_2021dec.onnx"))
        .expect("SFace must load");

    // SFace takes the 112x112 aligned crop the aligner produces.
    let crop = face_like_frame(112, 112);
    let v = emb.embed(&crop).expect("embedding must not fail");
    assert_eq!(v.vector.len(), 128, "SFace embeds to 128 dimensions");
    assert!(
        v.vector.iter().all(|x| x.is_finite()),
        "embedding has NaN/inf"
    );
    let norm: f32 = v.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() < 1e-3,
        "embeddings must be L2-normalised or every cosine score is wrong: |v| = {norm}"
    );
}

/// The same crop twice must embed identically. A backend that carried state
/// between runs would make a template depend on what was matched before it.
#[test]
#[ignore = "requires the ONNX models; run with NAMTOR_MODELS_DIR=<dir> -- --ignored"]
fn embedding_is_deterministic() {
    let dir = models_or_skip!();
    let mut emb = namtor_recog::embed::FaceEmbedder::new(&dir.join("sface_2021dec.onnx"))
        .expect("SFace must load");
    let crop = face_like_frame(112, 112);
    let a = emb.embed(&crop).expect("first embed");
    let b = emb.embed(&crop).expect("second embed");
    assert_eq!(
        a.vector, b.vector,
        "the same crop embedded to two different vectors"
    );
}

/// Different faces must embed to different vectors. If the model collapsed
/// every input onto one point, cosine similarity would be 1.0 for everyone
/// and face unlock would accept anybody.
#[test]
#[ignore = "requires the ONNX models; run with NAMTOR_MODELS_DIR=<dir> -- --ignored"]
fn different_crops_embed_differently() {
    let dir = models_or_skip!();
    let mut emb = namtor_recog::embed::FaceEmbedder::new(&dir.join("sface_2021dec.onnx"))
        .expect("SFace must load");
    let a = emb.embed(&face_like_frame(112, 112)).expect("a");
    let mut other = face_like_frame(112, 112);
    other.reverse();
    let b = emb.embed(&other).expect("b");
    let cos: f32 = a
        .vector
        .iter()
        .zip(b.vector.iter())
        .map(|(x, y)| x * y)
        .sum();
    assert!(
        cos < 0.999,
        "two different crops embedded to the same point (cos = {cos})"
    );
}

/// The whole path in one call: frame → detect → align → embed.
#[test]
#[ignore = "requires the ONNX models; run with NAMTOR_MODELS_DIR=<dir> -- --ignored"]
fn the_pipeline_runs_end_to_end_on_a_real_frame() {
    let dir = models_or_skip!();
    let mut pipe = namtor_recog::pipeline::RecognitionPipeline::new(
        &dir.join("yunet_2023mar.onnx"),
        &dir.join("sface_2021dec.onnx"),
        (640, 640),
    )
    .expect("both models must load together");

    let (w, h) = (640u32, 360u32);
    // `Ok(None)` (no face confident enough) is a valid answer here; an `Err`
    // is not — that would mean the stack itself is broken.
    let out = pipe
        .process_frame(&face_like_frame(w, h), w, h)
        .expect("the pipeline must not error on a well-formed frame");
    if let Some(r) = out {
        assert_eq!(r.embedding.vector.len(), 128);
        assert!(r.detection.box_.score > 0.5);
    }
}
