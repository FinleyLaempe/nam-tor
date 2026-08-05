// SPDX-License-Identifier: GPL-3.0-only
//! R4 INT8 vs FP32 eval driver (Task 25).
//!
//! `#[ignore]` — requires real ONNX model weights, a working `onnxruntime`
//! binding, and a labelled comparison set under `fixtures/r4/`. Run with:
//!
//! ```bash
//! cargo test -p namtor-daemon --test r4 -- --ignored
//! ```
//!
//! Produces two JSONL score files (`r4_fp32.jsonl`, `r4_int8.jsonl`) in the
//! workspace root, then the Python script `ml/recipes/r4_int8_vs_fp32.py`
//! renders `docs/m3-r4-report.md` from them.
//!
//! Blocked on the human: Python `onnxruntime` binding fix + model selection +
//! licence acceptance (`NAMTOR_ACCEPT_LICENCE=1`) + R5 cohort dataset. See
//! `AGENTS.md` "What M3 needs from the human".

use std::path::PathBuf;
use std::time::Instant;

use namtor_eval::score::{write_jsonl, LatencyBreakdown, ScoreRecord};
use namtor_proto::worker::{ModelSpec, Quant};
use namtor_worker::infer::InferenceBackend;
use namtor_worker::infer_onnx::OnnxCpu;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("r4")
}

fn model_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("var")
        .join("lib")
        .join("namtor")
}

fn run_backend(quant: Quant, fixture_dir: &std::path::Path) -> Vec<ScoreRecord> {
    let spec = ModelSpec {
        id: "r4-embed".into(),
        quant,
        sha256: [0; 32],
    };
    let root = model_root();
    let mut backend = OnnxCpu::new(1).with_root(root);
    let loaded = backend
        .load(&spec)
        .unwrap_or_else(|e| panic!("load {quant:?}: {e}"));

    let probes = std::fs::read_dir(fixture_dir.join("probes"))
        .unwrap_or_else(|e| panic!("read probes dir: {e}"));
    let gallery = std::fs::read_dir(fixture_dir.join("gallery"))
        .unwrap_or_else(|e| panic!("read gallery dir: {e}"));

    let gallery_items: Vec<_> = gallery
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            let id = path.file_stem()?.to_string_lossy().into_owned();
            Some((id, path))
        })
        .collect();

    let frame_ref = namtor_proto::worker::FrameRef {
        offset: 0,
        len: 0,
        emitter: namtor_capture::frame::EmitterState::On,
        spectrum: namtor_capture::source::Spectrum::Ir,
        pix_fmt: namtor_capture::frame::PixFmt::Grey8,
        w: 112,
        h: 112,
        ts_us: 0,
    };

    let mut records = Vec::new();
    for probe in probes.filter_map(|e| e.ok()) {
        let probe_path = probe.path();
        let probe_id = probe_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let probe_bytes = std::fs::read(&probe_path)
            .unwrap_or_else(|e| panic!("read probe {}: {e}", probe_path.display()));

        let t0 = Instant::now();
        let probe_vec = backend
            .run(&loaded, &probe_bytes, &frame_ref)
            .unwrap_or_else(|e| panic!("infer probe: {e}"));
        let embed_ms = t0.elapsed().as_secs_f64() * 1000.0;

        for (gallery_id, gallery_path) in &gallery_items {
            let gallery_bytes = std::fs::read(gallery_path)
                .unwrap_or_else(|e| panic!("read gallery {}: {e}", gallery_path.display()));
            let t1 = Instant::now();
            let gallery_vec = backend
                .run(&loaded, &gallery_bytes, &frame_ref)
                .unwrap_or_else(|e| panic!("infer gallery: {e}"));
            let gallery_embed_ms = t1.elapsed().as_secs_f64() * 1000.0;

            let score = cosine(&probe_vec, &gallery_vec);
            let genuine = probe_id.split('-').next() == gallery_id.split('-').next();
            let total_ms = embed_ms + gallery_embed_ms;

            records.push(ScoreRecord {
                probe_id: probe_id.clone(),
                gallery_id: gallery_id.clone(),
                genuine,
                score,
                pad_score: 1.0,
                attack_type: None,
                latency: LatencyBreakdown {
                    capture_ms: 0.0,
                    detect_ms: 0.0,
                    embed_ms: total_ms,
                    pad_ms: 0.0,
                    total_ms,
                },
            });
        }
    }
    records
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na > 0.0 && nb > 0.0 {
        dot / (na * nb)
    } else {
        0.0
    }
}

#[test]
#[ignore = "requires real ONNX weights, onnxruntime binding, and fixtures/r4/; run with NAMTOR_HW_TEST=1"]
fn r4_int8_vs_fp32() {
    let fixture_dir = fixture_dir();
    assert!(
        fixture_dir.exists(),
        "fixtures/r4/ not found at {}",
        fixture_dir.display()
    );

    let fp32_records = run_backend(Quant::Fp32, &fixture_dir);
    let int8_records = run_backend(Quant::Int8, &fixture_dir);

    assert!(!fp32_records.is_empty(), "FP32 produced no records");
    assert!(!int8_records.is_empty(), "INT8 produced no records");

    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    write_jsonl(&workspace.join("r4_fp32.jsonl"), &fp32_records).expect("write fp32 jsonl");
    write_jsonl(&workspace.join("r4_int8.jsonl"), &int8_records).expect("write int8 jsonl");

    eprintln!("wrote r4_fp32.jsonl ({} records)", fp32_records.len());
    eprintln!("wrote r4_int8.jsonl ({} records)", int8_records.len());
    eprintln!("now run: python3 ml/recipes/r4_int8_vs_fp32.py --fp32 r4_fp32.jsonl --int8 r4_int8.jsonl --out docs/m3-r4-report.md --cohort <name>");
}
