// SPDX-License-Identifier: GPL-3.0-only
//! G5.1 structural test: no ML/ONNX/image libraries linked into pam_namtor.so.
//!
//! Spec §3.4 + G5.1: the PAM module must not link any ML runtime, ONNX, or
//! image decoder. We assert this by inspecting the release-mode cdylib's
//! dynamic dependencies via `ldd` and rejecting a known blocklist.

use std::path::PathBuf;
use std::process::Command;

/// Blocklist of substrings that must never appear in pam_namtor.so's ldd
/// output. Matches are on the library basename (e.g. `libonnxruntime`).
const BLOCKLIST: &[&str] = &[
    "onnx",
    "onnxruntime",
    "opencv",
    "dlib",
    "tensorflow",
    "torch",
    "libtorch",
    "ncnn",
    "tflite",
    "tensorflow-lite",
    "libpng",
    "libjpeg",
    "libwebp",
    "libtiff",
    "libavcodec",
    "libavformat",
    "libavutil",
    "v4lconvert",
];

fn find_pam_so() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from("target/release/libnamtor_pam.so"),
        PathBuf::from("../target/release/libnamtor_pam.so"),
        PathBuf::from("../../target/release/libnamtor_pam.so"),
    ];
    for c in &candidates {
        if c.exists() {
            return Some(c.clone());
        }
    }
    None
}

#[test]
fn no_ml_or_image_libs_linked() {
    let so = match find_pam_so() {
        Some(p) => p,
        None => {
            eprintln!(
                "skipped: pam_namtor.so not built (run `cargo build --release -p namtor-pam`)"
            );
            return;
        }
    };
    let output = Command::new("ldd")
        .arg(&so)
        .output()
        .expect("ldd available");
    assert!(output.status.success(), "ldd failed on {}", so.display());
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("=== ldd {} ===\n{stdout}", so.display());

    let mut violations = Vec::new();
    for line in stdout.lines() {
        let lower = line.to_lowercase();
        for &blocked in BLOCKLIST {
            if lower.contains(blocked) {
                violations.push(line.trim().to_string());
            }
        }
    }
    assert!(
        violations.is_empty(),
        "G5.1 violation — pam_namtor.so links ML/image libs:\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn binary_size_under_budget() {
    let so = match find_pam_so() {
        Some(p) => p,
        None => {
            eprintln!("skipped: pam_namtor.so not built");
            return;
        }
    };
    let size = std::fs::metadata(&so).map(|m| m.len()).expect("stat");
    const BUDGET: u64 = 500 * 1024;
    println!("pam_namtor.so = {size} bytes (budget {BUDGET})");
    assert!(size <= BUDGET, "G§3.4 violation: {} > {BUDGET} bytes", size);
}
