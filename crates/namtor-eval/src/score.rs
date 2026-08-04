// SPDX-License-Identifier: GPL-3.0-only
//! Score records: the seam between the Rust pipeline and the Python harness.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::Path;
use thiserror::Error;

/// Per-attempt latency, split so capture and compute can be reported
/// separately (spec §12 G2).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatencyBreakdown {
    pub capture_ms: f64,
    pub detect_ms: f64,
    pub embed_ms: f64,
    pub pad_ms: f64,
    pub total_ms: f64,
}

/// One comparison of a probe against a gallery entry.
///
/// Carries no threshold and no decision: thresholds are swept downstream, so
/// a single score file supports every operating point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreRecord {
    pub probe_id: String,
    pub gallery_id: String,
    /// True when probe and gallery are the same identity.
    pub genuine: bool,
    /// Match score. Higher means more similar.
    pub score: f32,
    /// PAD score. Higher means more likely bona fide.
    pub pad_score: f32,
    /// Attack type for attack presentations; `None` for bona fide.
    pub attack_type: Option<String>,
    pub latency: LatencyBreakdown,
}

#[derive(Debug, Error)]
pub enum ScoreError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed record at line {line}: {source}")]
    Malformed {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("serialisation failed: {0}")]
    Serialise(serde_json::Error),
}

/// Writes records as JSON Lines, one record per line.
pub fn write_jsonl(path: &Path, records: &[ScoreRecord]) -> Result<(), ScoreError> {
    let mut f = fs::File::create(path)?;
    for r in records {
        let line = serde_json::to_string(r).map_err(ScoreError::Serialise)?;
        writeln!(f, "{line}")?;
    }
    Ok(())
}

/// Reads a JSON Lines score file, reporting the failing line on error.
///
/// Line numbers are **1-based**, matching what an editor shows. Blank lines
/// are skipped but still consume a line number, so the reported position
/// always refers to the real file.
pub fn read_jsonl(path: &Path) -> Result<Vec<ScoreRecord>, ScoreError> {
    let raw = fs::read_to_string(path)?;
    raw.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            serde_json::from_str(l).map_err(|source| ScoreError::Malformed {
                line: i + 1,
                source,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(genuine: bool, score: f32, attack: Option<&str>) -> ScoreRecord {
        ScoreRecord {
            probe_id: "p001".to_string(),
            gallery_id: "g001".to_string(),
            genuine,
            score,
            pad_score: 0.9,
            attack_type: attack.map(str::to_string),
            latency: LatencyBreakdown {
                capture_ms: 66.0,
                detect_ms: 5.0,
                embed_ms: 20.0,
                pad_ms: 3.0,
                total_ms: 94.0,
            },
        }
    }

    #[test]
    fn records_round_trip_through_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scores.jsonl");

        let written = vec![
            rec(true, 0.82, None),
            rec(false, 0.11, Some("screen_replay_phone")),
        ];
        write_jsonl(&path, &written).unwrap();

        let read = read_jsonl(&path).unwrap();
        assert_eq!(read.len(), 2);
        assert!(read[0].genuine);
        assert_eq!(read[1].attack_type.as_deref(), Some("screen_replay_phone"));
        assert_eq!(read[1].latency.total_ms, 94.0);
    }

    #[test]
    fn jsonl_is_one_record_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scores.jsonl");
        write_jsonl(&path, &[rec(true, 0.5, None), rec(false, 0.2, None)]).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.lines().count(), 2);
    }

    #[test]
    fn malformed_lines_are_reported_with_one_based_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.jsonl");

        // A VALID record first, so the reported line unambiguously identifies
        // the garbage on line 2 rather than an incomplete record on line 1.
        let good = serde_json::to_string(&rec(true, 0.5, None)).unwrap();
        std::fs::write(&path, format!("{good}\nnot json\n")).unwrap();

        let err = read_jsonl(&path).unwrap_err();
        assert!(
            err.to_string().contains("line 2"),
            "error must identify the failing line as 2, got: {err}"
        );
    }

    #[test]
    fn records_missing_required_fields_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.jsonl");
        std::fs::write(&path, "{\"probe_id\":\"p\"}\n").unwrap();

        let err = read_jsonl(&path).unwrap_err();
        assert!(err.to_string().contains("line 1"), "got: {err}");
    }
}
