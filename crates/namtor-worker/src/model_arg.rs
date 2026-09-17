// SPDX-License-Identifier: GPL-3.0-only
//! `--model` command-line encoding of a [`ModelSpec`].
//!
//! The worker must open every model file *before* it seals itself with
//! seccomp + Landlock (the sandbox denies `open`/`openat` and every path),
//! so the set of models the daemon intends to use has to be known at spawn
//! time rather than arriving later over the request pipe. The daemon
//! therefore passes one `--model <spec>` per model on the worker's command
//! line, and the worker preloads them all before calling
//! [`crate::sandbox::apply_sandbox`].
//!
//! The wire form is `<id>:<quant>:<sha256-hex>`, e.g.
//! `yunet_2023mar:fp32:0000…00`. Both sides of that contract live here so a
//! change to one cannot silently desynchronise the other: the daemon's
//! supervisor calls [`encode`] and the worker's argument parser calls
//! [`parse`], and `encode_parse_round_trips` pins them together.

use namtor_proto::worker::{ModelSpec, Quant};

/// Render `spec` as a single `--model` argument value.
pub fn encode(spec: &ModelSpec) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(spec.id.len() + 6 + 64);
    out.push_str(&spec.id);
    out.push(':');
    out.push_str(quant_str(spec.quant));
    out.push(':');
    for b in &spec.sha256 {
        // Writing into a String is infallible; the Result is discarded rather
        // than unwrapped so this stays free of `expect` in non-test code.
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Parse a `--model` argument value back into a [`ModelSpec`].
///
/// Returns a human-readable message on malformed input; the worker turns that
/// into a usage error rather than starting up with a half-understood model
/// set.
pub fn parse(s: &str) -> Result<ModelSpec, String> {
    let mut parts = s.splitn(3, ':');
    let id = parts.next().unwrap_or("");
    let quant = parts.next().unwrap_or("");
    let sha = parts.next().unwrap_or("");
    if id.is_empty() {
        return Err(format!("model spec {s:?}: empty model id"));
    }
    let quant = match quant {
        "fp32" => Quant::Fp32,
        "int8" => Quant::Int8,
        other => return Err(format!("model spec {s:?}: unknown quant {other:?}")),
    };
    if sha.len() != 64 {
        return Err(format!(
            "model spec {s:?}: sha256 must be 64 hex chars, got {}",
            sha.len()
        ));
    }
    let mut sha256 = [0u8; 32];
    for (i, byte) in sha256.iter_mut().enumerate() {
        let hex = sha.get(i * 2..i * 2 + 2).unwrap_or("");
        *byte = u8::from_str_radix(hex, 16)
            .map_err(|e| format!("model spec {s:?}: bad sha256 hex {hex:?}: {e}"))?;
    }
    Ok(ModelSpec {
        id: id.to_string(),
        quant,
        sha256,
    })
}

fn quant_str(q: Quant) -> &'static str {
    match q {
        Quant::Fp32 => "fp32",
        Quant::Int8 => "int8",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str, quant: Quant, fill: u8) -> ModelSpec {
        ModelSpec {
            id: id.into(),
            quant,
            sha256: [fill; 32],
        }
    }

    #[test]
    fn encode_parse_round_trips() {
        for s in [
            spec("yunet_2023mar", Quant::Fp32, 0),
            spec("sface_2021dec", Quant::Int8, 0xab),
            spec("minifasnet_pad", Quant::Fp32, 0xff),
        ] {
            let encoded = encode(&s);
            assert_eq!(parse(&encoded).expect("round trip"), s, "from {encoded:?}");
        }
    }

    #[test]
    fn encode_uses_lowercase_hex_sha256() {
        let encoded = encode(&spec("m", Quant::Fp32, 0xde));
        assert_eq!(encoded, format!("m:fp32:{}", "de".repeat(32)));
    }

    #[test]
    fn parse_rejects_malformed_input() {
        let zeros = "0".repeat(64);
        for bad in [
            String::new(),
            "id".to_string(),
            "id:fp32".to_string(),
            format!(":fp32:{zeros}"),
            format!("id:fp16:{zeros}"),
            format!("id:fp32:{}", "0".repeat(63)),
            format!("id:fp32:{}", "z".repeat(64)),
        ] {
            assert!(parse(&bad).is_err(), "{bad:?} should not parse");
        }
    }
}
