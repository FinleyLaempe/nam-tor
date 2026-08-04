// SPDX-License-Identifier: GPL-3.0-only
//! Replays recorded sessions as a [`CaptureSource`].
//!
//! This is what makes the pipeline testable in CI with no camera attached,
//! and what lets contributors reproduce published error rates exactly.

use crate::frame::Frame;
use crate::recording::{SessionManifest, MANIFEST_VERSION};
use crate::source::{CaptureError, CaptureSource, SourceCaps};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A [`CaptureSource`] backed by a recorded session directory.
///
/// Replays as fast as the caller pulls; recorded timestamps are preserved on
/// each [`Frame`] but are not used to pace delivery.
pub struct FileReplay {
    dir: PathBuf,
    manifest: SessionManifest,
    cursor: usize,
    started: bool,
}

impl FileReplay {
    pub fn open(dir: &Path) -> Result<Self, CaptureError> {
        let raw = fs::read_to_string(dir.join("manifest.json"))?;
        let manifest: SessionManifest =
            serde_json::from_str(&raw).map_err(|e| CaptureError::Recording(e.to_string()))?;

        if manifest.version != MANIFEST_VERSION {
            return Err(CaptureError::Recording(format!(
                "unsupported manifest version {} (this build supports {})",
                manifest.version, MANIFEST_VERSION
            )));
        }

        Ok(Self {
            dir: dir.to_path_buf(),
            manifest,
            cursor: 0,
            started: false,
        })
    }

    /// The label this session was recorded under. Drives APCER bookkeeping.
    pub fn label(&self) -> &crate::recording::SessionLabel {
        &self.manifest.label
    }
}

impl CaptureSource for FileReplay {
    fn caps(&self) -> SourceCaps {
        self.manifest.source
    }

    fn start(&mut self) -> Result<(), CaptureError> {
        self.cursor = 0;
        self.started = true;
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError> {
        if !self.started {
            return Err(CaptureError::NotStarted);
        }
        let Some(rec) = self.manifest.frames.get(self.cursor) else {
            return Ok(None);
        };
        self.cursor += 1;

        let data = fs::read(self.dir.join(&rec.file))?;
        let caps = self.manifest.source;

        Ok(Some(Frame {
            data,
            format: caps.format,
            width: caps.width,
            height: caps.height,
            ts_mono: Duration::from_nanos(rec.ts_ns),
            sof: rec.sof,
            emitter: rec.emitter,
            quality: None,
        }))
    }

    fn stop(&mut self) {
        self.started = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{EmitterState, Frame, PixFmt};
    use crate::recording::{SessionLabel, SessionWriter};
    use crate::source::{CaptureSource, SourceCaps, Spectrum};
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

    fn write_session(dir: &std::path::Path) {
        let mut w = SessionWriter::create(dir, SessionLabel::Synthetic, caps()).unwrap();
        for i in 0..4u64 {
            w.push(&Frame {
                data: vec![i as u8; 230_400],
                format: PixFmt::Grey8,
                width: 640,
                height: 360,
                ts_mono: Duration::from_millis(33 * i),
                sof: Some(i as u16),
                emitter: if i % 2 == 0 {
                    EmitterState::On
                } else {
                    EmitterState::Off
                },
                quality: None,
            })
            .unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn replay_reproduces_written_frames_exactly() {
        let dir = tempfile::tempdir().unwrap();
        write_session(dir.path());

        let mut r = FileReplay::open(dir.path()).unwrap();
        r.start().unwrap();

        for i in 0..4u64 {
            let f = r.next_frame().unwrap().expect("frame missing");
            assert_eq!(f.data[0], i as u8);
            assert_eq!(f.ts_mono, Duration::from_millis(33 * i));
            assert_eq!(f.sof, Some(i as u16));
            assert_eq!(
                f.emitter,
                if i % 2 == 0 {
                    EmitterState::On
                } else {
                    EmitterState::Off
                }
            );
            assert!(f.validate().is_ok());
        }
        assert!(r.next_frame().unwrap().is_none(), "stream should terminate");
    }

    #[test]
    fn replay_reports_the_recorded_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        write_session(dir.path());
        let r = FileReplay::open(dir.path()).unwrap();
        assert_eq!(r.caps(), caps());
    }

    #[test]
    fn replay_before_start_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        write_session(dir.path());
        let mut r = FileReplay::open(dir.path()).unwrap();
        assert!(r.next_frame().is_err());
    }

    #[test]
    fn unknown_manifest_version_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_session(dir.path());
        let p = dir.path().join("manifest.json");
        let raw = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, raw.replace("\"version\": 1", "\"version\": 999")).unwrap();

        assert!(FileReplay::open(dir.path()).is_err());
    }
}
