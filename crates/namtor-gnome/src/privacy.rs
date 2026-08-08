// SPDX-License-Identifier: GPL-3.0-only
//! Privacy scan over the UI-facing data types.
//!
//! The GNOME UI must never receive embeddings, raw frames, match scores, or
//! thresholds (spec §9.2, task 26). This module serializes each public
//! D-Bus struct to a JSON sample and asserts no field name carries a
//! forbidden biometric / matcher term. The scan is exercised by
//! `tests/privacy_scan.rs` so CI fails if anyone widens the contract.

use serde::Serialize;

/// Substrings that, if they appear in a UI struct field name, would leak
/// matcher internals the user must never see.
///
/// Note: `frames_captured` (an approved coarse count, spec §6.5) is
/// intentionally NOT matched — we forbid compound raw-frame field names
/// (`raw_frame`, `frame_data`, `frame_buf`, `frame_pixel`) instead of the
/// bare `frame` substring so the count is allowed through.
pub const FORBIDDEN_SUBSTRINGS: &[&str] = &[
    "vector",
    "embedding",
    "template",
    "landmark",
    "descriptor",
    "raw_frame",
    "frame_data",
    "frame_buf",
    "frame_pixel",
    "frame_raw",
    "score",
    "threshold",
    "biometric",
];

/// Walk the serialized JSON of `value` and collect every key (recursively)
/// that contains a forbidden substring. Empty result = clean.
pub fn forbidden_fields<T: Serialize>(value: &T) -> Vec<String> {
    let json = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    let mut hits = Vec::new();
    walk(&json, &mut hits);
    hits
}

/// Walk a serialized JSON value and collect every key (recursively) that
/// contains a forbidden substring. Exposed so mutation tests can inject
/// forbidden field names into JSON objects and verify they are caught.
pub fn scan_value(value: &serde_json::Value, hits: &mut Vec<String>) {
    walk(value, hits);
}

fn walk(value: &serde_json::Value, hits: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let lower = key.to_lowercase();
                for forbidden in FORBIDDEN_SUBSTRINGS {
                    if lower.contains(forbidden) {
                        hits.push(key.clone());
                        break;
                    }
                }
                walk(child, hits);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                walk(item, hits);
            }
        }
        _ => {}
    }
}
