// SPDX-License-Identifier: GPL-3.0-only
//! On-disk format for recorded capture sessions.

use crate::frame::{EmitterState, Frame};
use crate::source::{CaptureError, SourceCaps};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Current on-disk manifest version. Bump on any incompatible change.
pub const MANIFEST_VERSION: u32 = 1;

/// What a recorded session contains, for evaluation purposes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionLabel {
    /// A genuine presentation by a real person.
    Bonafide { subject_id: String },
    /// A presentation attack. `attack_type` is reported separately in APCER.
    Attack {
        attack_type: String,
        subject_id: String,
    },
    /// Generated, not captured. Used for CI.
    Synthetic,
}

/// One frame's entry in a session manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameRecord {
    pub index: u32,
    /// Path to the raw frame buffer, relative to the session directory.
    pub file: String,
    /// Nanoseconds since session start.
    pub ts_ns: u64,
    pub sof: Option<u16>,
    pub emitter: EmitterState,
}

/// The `manifest.json` at the root of a session directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionManifest {
    pub version: u32,
    pub label: SessionLabel,
    pub source: SourceCaps,
    pub frames: Vec<FrameRecord>,
}

/// Writes a capture session to a directory as `manifest.json` plus raw frames.
pub struct SessionWriter {
    dir: PathBuf,
    manifest: SessionManifest,
}

impl SessionWriter {
    /// Creates the session directory and its `frames/` subdirectory.
    pub fn create(
        dir: &Path,
        label: SessionLabel,
        source: SourceCaps,
    ) -> Result<Self, CaptureError> {
        fs::create_dir_all(dir.join("frames"))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            manifest: SessionManifest {
                version: MANIFEST_VERSION,
                label,
                source,
                frames: Vec::new(),
            },
        })
    }

    /// Appends a frame. Rejects frames whose geometry does not validate,
    /// so a corrupt recording cannot be produced silently.
    pub fn push(&mut self, frame: &Frame) -> Result<(), CaptureError> {
        frame
            .validate()
            .map_err(|e| CaptureError::Recording(e.to_string()))?;

        let index = self.manifest.frames.len() as u32;
        let rel = format!("frames/{index:06}.bin");
        fs::write(self.dir.join(&rel), &frame.data)?;

        self.manifest.frames.push(FrameRecord {
            index,
            file: rel,
            ts_ns: frame.ts_mono.as_nanos() as u64,
            sof: frame.sof,
            emitter: frame.emitter,
        });
        Ok(())
    }

    /// Writes `manifest.json`, completing the session.
    pub fn finish(self) -> Result<(), CaptureError> {
        let json = serde_json::to_string_pretty(&self.manifest)
            .map_err(|e| CaptureError::Recording(e.to_string()))?;
        fs::write(self.dir.join("manifest.json"), json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{EmitterState, Frame, PixFmt};
    use crate::source::{SourceCaps, Spectrum};
    use std::time::Duration;

    fn caps() -> SourceCaps {
        SourceCaps {
            spectrum: Spectrum::Ir,
            format: PixFmt::Grey8,
            width: 640,
            height: 360,
            fps: 30,
            has_metadata: true,
        }
    }

    #[test]
    fn manifest_round_trips_through_json() {
        let m = SessionManifest {
            version: MANIFEST_VERSION,
            label: SessionLabel::Attack {
                attack_type: "print_colour_laser".to_string(),
                subject_id: "s001".to_string(),
            },
            source: caps(),
            frames: vec![FrameRecord {
                index: 0,
                file: "frames/000000.bin".to_string(),
                ts_ns: 0,
                sof: Some(7),
                emitter: EmitterState::On,
            }],
        };

        let json = serde_json::to_string(&m).unwrap();
        let back: SessionManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(back.version, MANIFEST_VERSION);
        assert_eq!(back.frames.len(), 1);
        assert_eq!(back.frames[0].emitter, EmitterState::On);
        assert_eq!(back.frames[0].sof, Some(7));
        match back.label {
            SessionLabel::Attack { attack_type, .. } => {
                assert_eq!(attack_type, "print_colour_laser")
            }
            other => panic!("wrong label variant: {other:?}"),
        }
    }

    #[test]
    fn attack_type_survives_so_apcer_can_be_reported_per_type() {
        // Spec §10.6 requires per-attack-type APCER. The label must carry it.
        let l = SessionLabel::Attack {
            attack_type: "screen_replay_phone".to_string(),
            subject_id: "s001".to_string(),
        };
        let json = serde_json::to_string(&l).unwrap();
        assert!(json.contains("screen_replay_phone"));
    }

    fn frame_at(ms: u64, emitter: EmitterState) -> Frame {
        Frame {
            data: vec![ms as u8; 230_400],
            format: PixFmt::Grey8,
            width: 640,
            height: 360,
            ts_mono: Duration::from_millis(ms),
            sof: Some(ms as u16),
            emitter,
            quality: None,
        }
    }

    #[test]
    fn writer_produces_a_readable_session_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = SessionWriter::create(dir.path(), SessionLabel::Synthetic, caps()).unwrap();

        w.push(&frame_at(0, EmitterState::On)).unwrap();
        w.push(&frame_at(33, EmitterState::Off)).unwrap();
        w.finish().unwrap();

        let manifest_path = dir.path().join("manifest.json");
        assert!(manifest_path.exists(), "manifest.json not written");

        let m: SessionManifest =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();

        assert_eq!(m.frames.len(), 2);
        assert_eq!(m.frames[0].ts_ns, 0);
        assert_eq!(m.frames[1].ts_ns, 33_000_000);
        assert_eq!(m.frames[1].emitter, EmitterState::Off);

        for fr in &m.frames {
            let p = dir.path().join(&fr.file);
            assert!(p.exists(), "frame file missing: {}", fr.file);
            assert_eq!(std::fs::metadata(&p).unwrap().len(), 230_400);
        }
    }

    #[test]
    fn writer_rejects_frames_that_fail_validation() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = SessionWriter::create(dir.path(), SessionLabel::Synthetic, caps()).unwrap();

        let mut bad = frame_at(0, EmitterState::On);
        bad.data.truncate(10);

        assert!(w.push(&bad).is_err());
    }
}
