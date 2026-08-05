// SPDX-License-Identifier: GPL-3.0-only
//! G5.1 structural assertion: `namtor-proto` carries no ML dependencies.
//!
//! The future PAM module (M5) depends on `namtor-proto` for the wire types.
//! If a heavy ML crate (ort, onnxruntime, opencv, image, ndarray, tch) ever
//! leaks into `namtor-proto`'s dependency graph, the PAM module would pull
//! ML code into the PAM process — a security boundary violation (spec §3.4,
//! §11.4). This test runs `cargo tree` and fails the CI gate if any banned
//! crate appears.

use std::process::Command;

#[test]
fn namtor_proto_has_no_ml_deps() {
    let output = Command::new("cargo")
        .args(["tree", "-p", "namtor-proto", "--no-default-features"])
        .output()
        .expect("cargo tree");

    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let tree = String::from_utf8_lossy(&output.stdout);
    eprintln!("namtor-proto tree:\n{tree}");

    let banned = ["ort", "onnxruntime", "opencv", "image", "ndarray", "tch"];
    for b in &banned {
        let pattern = format!(" {b} ");
        assert!(
            !tree.contains(&pattern),
            "G5.1 violation: namtor-proto depends on banned ML crate '{b}'"
        );
    }
}
