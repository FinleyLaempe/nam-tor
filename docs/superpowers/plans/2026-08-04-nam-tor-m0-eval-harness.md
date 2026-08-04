# Nam-tor M0 — Measuring Stick Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the capture abstraction and evaluation harness that make every later accuracy, speed, and anti-spoof claim measurable and reproducible without a camera.

**Architecture:** A Rust workspace provides `CaptureSource` — the trait every capture path implements — plus `FileReplay`, which replays recorded frame sessions from disk, and a deterministic synthetic session generator so CI has data with no hardware. The Rust side emits score records as JSONL; a Python package consumes them and computes FAR/FRR/DET, APCER/BPCER per attack type, and latency distributions, rendering a Markdown report. The JSONL file is the seam: Rust does capture and inference, Python does statistics.

**Tech Stack:** Rust (serde, serde_json, thiserror, tempfile), Python 3.11+ (numpy, pytest), GitHub Actions.

## Global Constraints

Copied from `docs/superpowers/plans/2026-08-04-nam-tor-roadmap.md`. Every task's requirements implicitly include these.

- Rust edition 2021 or later for all shipped runtime code.
- Python + PyTorch confined to `ml/`. Never shipped to users.
- SPDX header on every source file. `namtor-hwdb`, `namtor-probe`, `quirks/` are **Apache-2.0**; everything else is **GPL-3.0**.
- No model weights and no datasets are ever committed.
- TDD: test first, watch it fail, minimal implementation, watch it pass, commit.
- Commit after every task.
- No biometric data in logs at any level.
- Reference hardware: Arch Linux, kernel 7.1.x, Intel i9-13900H, Sonix `3277:0018` camera, IR node `/dev/video2`, IR metadata node `/dev/video3`.

### Deviations from the spec, and why

Two refinements to spec §4.7, applied consistently throughout this plan:

1. **`Frame.ts_mono` is `std::time::Duration` (since session start), not `Instant`.** `Instant` is not serialisable and carries no meaning across a recording boundary. Replay needs a relative timestamp.
2. **`CaptureSource` is pull-based (`next_frame`) rather than returning a `FrameStream`.** An iterator-shaped interface is simpler to implement, simpler to fake in tests, and avoids committing to an async runtime in the capture layer. Nothing later depends on the stream shape.

---

### Task 1: Workspace skeleton and SPDX enforcement

**Files:**
- Create: `Cargo.toml`
- Create: `crates/namtor-capture/Cargo.toml`
- Create: `crates/namtor-capture/src/lib.rs`
- Create: `crates/namtor-eval/Cargo.toml`
- Create: `crates/namtor-eval/src/lib.rs`
- Create: `scripts/check-spdx.sh`
- Create: `.github/workflows/ci.yml`
- Create: `.gitignore`

**Interfaces:**
- Consumes: nothing
- Produces: a Cargo workspace with members `namtor-capture` and `namtor-eval`; `scripts/check-spdx.sh` exiting 0 when every `.rs` file carries an SPDX header and 1 otherwise.

- [ ] **Step 1: Write the failing test**

Create `scripts/check-spdx.sh`:

```bash
#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Asserts every Rust source file carries an SPDX license identifier.
set -euo pipefail

fail=0
while IFS= read -r -d '' f; do
    if ! head -n 3 "$f" | grep -q 'SPDX-License-Identifier:'; then
        echo "missing SPDX header: $f" >&2
        fail=1
    fi
done < <(find crates -name '*.rs' -print0)

if [ "$fail" -ne 0 ]; then
    echo "SPDX check FAILED" >&2
    exit 1
fi
echo "SPDX check passed"
```

Make it executable:

```bash
chmod +x scripts/check-spdx.sh
```

- [ ] **Step 2: Run it to verify it fails**

Run: `./scripts/check-spdx.sh`
Expected: FAIL — `find: 'crates': No such file or directory`, or once crates exist but lack headers, `missing SPDX header: ...` and exit 1.

- [ ] **Step 3: Create the workspace**

Create `Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = [
    "crates/namtor-capture",
    "crates/namtor-eval",
]

[workspace.package]
edition = "2021"
rust-version = "1.75"
license = "GPL-3.0-only"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tempfile = "3"
```

Create `crates/namtor-capture/Cargo.toml`:

```toml
[package]
name = "namtor-capture"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

Create `crates/namtor-capture/src/lib.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only
//! Capture abstraction for Nam-tor.
//!
//! Provides the [`CaptureSource`] trait implemented by every capture path,
//! along with recording, replay, and synthetic session generation.
```

Create `crates/namtor-eval/Cargo.toml`:

```toml
[package]
name = "namtor-eval"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

Create `crates/namtor-eval/src/lib.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only
//! Score record types and JSONL emission for the Nam-tor evaluation harness.
```

Create `.gitignore`:

```
/target
__pycache__/
*.pyc
.pytest_cache/
/ml/**/data/
/ml/**/weights/
/fixtures/large/
```

- [ ] **Step 4: Run the check to verify it passes**

Run: `./scripts/check-spdx.sh && cargo build`
Expected: `SPDX check passed`, then a clean build of both crates.

- [ ] **Step 5: Add CI**

Create `.github/workflows/ci.yml`:

```yaml
name: CI
on: [push, pull_request]

jobs:
  rust:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      - run: ./scripts/check-spdx.sh
      - run: cargo fmt --all -- --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo test --all

  python:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-python@v5
        with:
          python-version: "3.11"
      - run: pip install -e 'ml/eval[dev]'   # quoted: brackets are glob chars
      - run: pytest ml/eval/tests -v
```

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates scripts .github .gitignore
git commit -m "chore: cargo workspace, SPDX enforcement, CI"
```

---

### Task 2: Pixel formats and frame geometry

**Files:**
- Create: `crates/namtor-capture/src/frame.rs`
- Modify: `crates/namtor-capture/src/lib.rs`

**Interfaces:**
- Consumes: nothing
- Produces: `PixFmt` enum with variants `Grey8`, `Yuyv`, `Mjpeg`, `Depth16`; `PixFmt::bits_per_pixel(self) -> Option<u32>` returning `None` for compressed formats; `PixFmt::expected_len(self, width: u32, height: u32) -> Option<usize>`.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-capture/src/frame.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only
//! Frame representation and pixel format geometry.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey8_expected_len_matches_ir_node_geometry() {
        // The reference IR sensor: 640x360 GREY, one byte per pixel.
        assert_eq!(PixFmt::Grey8.expected_len(640, 360), Some(230_400));
    }

    #[test]
    fn yuyv_is_two_bytes_per_pixel() {
        assert_eq!(PixFmt::Yuyv.expected_len(640, 480), Some(614_400));
    }

    #[test]
    fn depth16_is_two_bytes_per_pixel() {
        assert_eq!(PixFmt::Depth16.expected_len(640, 360), Some(460_800));
    }

    #[test]
    fn compressed_formats_have_no_fixed_length() {
        assert_eq!(PixFmt::Mjpeg.bits_per_pixel(), None);
        assert_eq!(PixFmt::Mjpeg.expected_len(1920, 1080), None);
    }
}
```

Add to `crates/namtor-capture/src/lib.rs`:

```rust
pub mod frame;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-capture frame::`
Expected: FAIL — `cannot find type PixFmt in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add above the `tests` module in `crates/namtor-capture/src/frame.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Pixel formats Nam-tor can carry through the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixFmt {
    /// 8-bit greyscale. What `uvcvideo` reports for IR sensors.
    Grey8,
    /// YUYV 4:2:2, 16 bits per pixel.
    Yuyv,
    /// Motion-JPEG. Compressed, variable length.
    Mjpeg,
    /// 16-bit depth. Realsense-class hardware only.
    Depth16,
}

impl PixFmt {
    /// Bits per pixel, or `None` for compressed formats.
    pub fn bits_per_pixel(self) -> Option<u32> {
        match self {
            PixFmt::Grey8 => Some(8),
            PixFmt::Yuyv => Some(16),
            PixFmt::Depth16 => Some(16),
            PixFmt::Mjpeg => None,
        }
    }

    /// Expected buffer length for the given geometry, or `None` if variable.
    pub fn expected_len(self, width: u32, height: u32) -> Option<usize> {
        self.bits_per_pixel()
            .map(|bpp| (width as usize * height as usize * bpp as usize) / 8)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-capture frame::`
Expected: PASS — 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-capture/src/frame.rs crates/namtor-capture/src/lib.rs
git commit -m "feat(capture): pixel formats and frame geometry"
```

---

### Task 3: Frame type and validation

**Files:**
- Modify: `crates/namtor-capture/src/frame.rs`

**Interfaces:**
- Consumes: `PixFmt` from Task 2
- Produces: `EmitterState` enum (`On`, `Off`, `Torn`, `Unknown`); `FrameQuality { blur: f32, mean_luma: f32, saturated_frac: f32 }`; `Frame { data, format, width, height, ts_mono: Duration, sof: Option<u16>, emitter, quality: Option<FrameQuality> }`; `Frame::validate(&self) -> Result<(), FrameError>`; `FrameError` enum.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/namtor-capture/src/frame.rs`:

```rust
    use std::time::Duration;

    fn grey_frame(len: usize) -> Frame {
        Frame {
            data: vec![0u8; len],
            format: PixFmt::Grey8,
            width: 640,
            height: 360,
            ts_mono: Duration::from_millis(0),
            sof: None,
            emitter: EmitterState::Unknown,
            quality: None,
        }
    }

    #[test]
    fn correctly_sized_frame_validates() {
        assert!(grey_frame(230_400).validate().is_ok());
    }

    #[test]
    fn undersized_frame_is_rejected() {
        let err = grey_frame(1000).validate().unwrap_err();
        assert!(matches!(
            err,
            FrameError::LengthMismatch { expected: 230_400, actual: 1000 }
        ));
    }

    #[test]
    fn compressed_frame_accepts_any_length() {
        let f = Frame {
            data: vec![0u8; 4242],
            format: PixFmt::Mjpeg,
            width: 1920,
            height: 1080,
            ts_mono: Duration::from_millis(0),
            sof: None,
            emitter: EmitterState::Unknown,
            quality: None,
        };
        assert!(f.validate().is_ok());
    }

    #[test]
    fn torn_frames_are_distinguishable() {
        // Torn frames must never be silently treated as usable.
        assert_ne!(EmitterState::Torn, EmitterState::On);
        assert_ne!(EmitterState::Torn, EmitterState::Off);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-capture frame::`
Expected: FAIL — `cannot find struct Frame in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-capture/src/frame.rs` above the `tests` module:

```rust
use std::time::Duration;
use thiserror::Error;

/// State of the IR emitter when a frame was exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmitterState {
    /// Emitter was on for the whole exposure.
    On,
    /// Emitter was off for the whole exposure.
    Off,
    /// The emitter toggled mid-exposure. Unusable; must be discarded.
    Torn,
    /// Emitter state could not be determined.
    Unknown,
}

/// Per-frame quality signals used for enrollment filtering and fusion weighting.
///
/// Defined here so the [`Frame`] shape is stable, but **not populated during
/// M0** — `Frame::quality` is `None` throughout this milestone. M4 computes it
/// during enrollment (spec §6.6 Tier 1), and M6 uses it to weight multi-frame
/// fusion. Do not add computation for it here; there is nothing to consume it
/// yet, and the metric definitions depend on M1's measurements.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrameQuality {
    /// Variance of the Laplacian. Higher is sharper.
    pub blur: f32,
    /// Mean luminance, normalised to 0.0..=1.0.
    pub mean_luma: f32,
    /// Fraction of pixels at full scale.
    pub saturated_frac: f32,
}

/// A single captured frame.
#[derive(Debug, Clone)]
pub struct Frame {
    pub data: Vec<u8>,
    pub format: PixFmt,
    pub width: u32,
    pub height: u32,
    /// Elapsed time since the capture session started.
    pub ts_mono: Duration,
    /// USB start-of-frame counter, from UVC payload header metadata.
    pub sof: Option<u16>,
    pub emitter: EmitterState,
    pub quality: Option<FrameQuality>,
}

#[derive(Debug, Error, PartialEq)]
pub enum FrameError {
    #[error("frame buffer length mismatch: expected {expected}, got {actual}")]
    LengthMismatch { expected: usize, actual: usize },
}

impl Frame {
    /// Checks the buffer length against the declared geometry.
    ///
    /// Compressed formats have no fixed length and always validate.
    pub fn validate(&self) -> Result<(), FrameError> {
        match self.format.expected_len(self.width, self.height) {
            Some(expected) if expected != self.data.len() => Err(FrameError::LengthMismatch {
                expected,
                actual: self.data.len(),
            }),
            _ => Ok(()),
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-capture frame::`
Expected: PASS — 8 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-capture/src/frame.rs
git commit -m "feat(capture): frame type with geometry validation"
```

---

### Task 4: CaptureSource trait

**Files:**
- Create: `crates/namtor-capture/src/source.rs`
- Modify: `crates/namtor-capture/src/lib.rs`

**Interfaces:**
- Consumes: `Frame`, `PixFmt` from Tasks 2–3
- Produces: `Spectrum` enum (`Ir`, `Rgb`, `Depth`); `SourceCaps { spectrum, format, width, height, fps, has_metadata }`; `CaptureError` enum; `trait CaptureSource { fn caps(&self) -> SourceCaps; fn start(&mut self) -> Result<(), CaptureError>; fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError>; fn stop(&mut self); }`. `next_frame` returns `Ok(None)` at end of stream.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-capture/src/source.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only
//! The capture source abstraction.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{EmitterState, Frame, PixFmt};
    use std::time::Duration;

    /// Minimal in-test implementation, proving the trait is implementable
    /// and that end-of-stream is expressible.
    struct TwoFrameSource {
        emitted: u32,
        started: bool,
    }

    impl CaptureSource for TwoFrameSource {
        fn caps(&self) -> SourceCaps {
            SourceCaps {
                spectrum: Spectrum::Ir,
                format: PixFmt::Grey8,
                width: 640,
                height: 360,
                fps: 30,
                has_metadata: true,
            }
        }

        fn start(&mut self) -> Result<(), CaptureError> {
            self.started = true;
            Ok(())
        }

        fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError> {
            if !self.started {
                return Err(CaptureError::NotStarted);
            }
            if self.emitted >= 2 {
                return Ok(None);
            }
            let f = Frame {
                data: vec![0u8; 230_400],
                format: PixFmt::Grey8,
                width: 640,
                height: 360,
                ts_mono: Duration::from_millis(33 * self.emitted as u64),
                sof: Some(self.emitted as u16),
                emitter: if self.emitted % 2 == 0 {
                    EmitterState::On
                } else {
                    EmitterState::Off
                },
                quality: None,
            };
            self.emitted += 1;
            Ok(Some(f))
        }

        fn stop(&mut self) {
            self.started = false;
        }
    }

    #[test]
    fn source_reports_its_capabilities() {
        let s = TwoFrameSource { emitted: 0, started: false };
        assert_eq!(s.caps().spectrum, Spectrum::Ir);
        assert_eq!(s.caps().width, 640);
    }

    #[test]
    fn next_frame_before_start_is_an_error() {
        let mut s = TwoFrameSource { emitted: 0, started: false };
        assert!(matches!(s.next_frame(), Err(CaptureError::NotStarted)));
    }

    #[test]
    fn stream_terminates_with_ok_none() {
        let mut s = TwoFrameSource { emitted: 0, started: false };
        s.start().unwrap();
        assert!(s.next_frame().unwrap().is_some());
        assert!(s.next_frame().unwrap().is_some());
        assert!(s.next_frame().unwrap().is_none());
    }
}
```

Add to `crates/namtor-capture/src/lib.rs`:

```rust
pub mod source;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-capture source::`
Expected: FAIL — `cannot find trait CaptureSource in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-capture/src/source.rs` above the `tests` module:

```rust
use crate::frame::{Frame, PixFmt};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Which part of the spectrum a source observes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Spectrum {
    Ir,
    Rgb,
    Depth,
}

/// Static description of what a source produces.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SourceCaps {
    pub spectrum: Spectrum,
    pub format: PixFmt,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Whether a UVC metadata node is available for this source.
    pub has_metadata: bool,
}

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("capture source was not started")]
    NotStarted,
    #[error("device error: {0}")]
    Device(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed recording: {0}")]
    Recording(String),
}

/// A source of frames.
///
/// Pull-based: callers drive the stream with [`CaptureSource::next_frame`],
/// which returns `Ok(None)` once the stream is exhausted. Live device sources
/// block until the next frame arrives and never return `Ok(None)` until stopped.
pub trait CaptureSource {
    fn caps(&self) -> SourceCaps;
    fn start(&mut self) -> Result<(), CaptureError>;
    fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError>;
    fn stop(&mut self);
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-capture source::`
Expected: PASS — 3 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-capture/src/source.rs crates/namtor-capture/src/lib.rs
git commit -m "feat(capture): CaptureSource trait and capability description"
```

---

### Task 5: Recording manifest format

**Files:**
- Create: `crates/namtor-capture/src/recording.rs`
- Modify: `crates/namtor-capture/src/lib.rs`

**Interfaces:**
- Consumes: `EmitterState`, `SourceCaps` from Tasks 3–4
- Produces: `SessionLabel` enum (`Bonafide { subject_id }`, `Attack { attack_type, subject_id }`, `Synthetic`); `FrameRecord { index, file, ts_ns, sof, emitter }`; `SessionManifest { version, label, source, frames }`; `MANIFEST_VERSION: u32 = 1`.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-capture/src/recording.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only
//! On-disk format for recorded capture sessions.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{EmitterState, PixFmt};
    use crate::source::{SourceCaps, Spectrum};

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
}
```

Add to `crates/namtor-capture/src/lib.rs`:

```rust
pub mod recording;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-capture recording::`
Expected: FAIL — `cannot find struct SessionManifest in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-capture/src/recording.rs` above the `tests` module:

```rust
use crate::frame::EmitterState;
use crate::source::SourceCaps;
use serde::{Deserialize, Serialize};

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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-capture recording::`
Expected: PASS — 2 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-capture/src/recording.rs crates/namtor-capture/src/lib.rs
git commit -m "feat(capture): session recording manifest format"
```

---

### Task 6: Session writer

**Files:**
- Modify: `crates/namtor-capture/src/recording.rs`

**Interfaces:**
- Consumes: `SessionManifest`, `FrameRecord`, `Frame`, `SourceCaps`
- Produces: `SessionWriter::create(dir: &Path, label: SessionLabel, source: SourceCaps) -> Result<SessionWriter, CaptureError>`; `SessionWriter::push(&mut self, frame: &Frame) -> Result<(), CaptureError>`; `SessionWriter::finish(self) -> Result<(), CaptureError>` writing `manifest.json`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/namtor-capture/src/recording.rs`:

```rust
    use crate::frame::Frame;
    use std::time::Duration;

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
        let mut w = SessionWriter::create(
            dir.path(),
            SessionLabel::Synthetic,
            caps(),
        )
        .unwrap();

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
        let mut w =
            SessionWriter::create(dir.path(), SessionLabel::Synthetic, caps()).unwrap();

        let mut bad = frame_at(0, EmitterState::On);
        bad.data.truncate(10);

        assert!(w.push(&bad).is_err());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-capture recording::`
Expected: FAIL — `cannot find struct SessionWriter in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-capture/src/recording.rs`:

```rust
use crate::frame::Frame;
use crate::source::CaptureError;
use std::fs;
use std::path::{Path, PathBuf};

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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-capture recording::`
Expected: PASS — 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-capture/src/recording.rs
git commit -m "feat(capture): session writer with validation on push"
```

---

### Task 7: FileReplay source

**Files:**
- Create: `crates/namtor-capture/src/replay.rs`
- Modify: `crates/namtor-capture/src/lib.rs`

**Interfaces:**
- Consumes: `SessionManifest`, `CaptureSource`, `Frame`, `CaptureError`
- Produces: `FileReplay::open(dir: &Path) -> Result<FileReplay, CaptureError>` implementing `CaptureSource`. Replays as fast as possible — no real-time pacing, so CI is not throttled by recording length.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-capture/src/replay.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only
//! Replays recorded sessions as a [`CaptureSource`].
//!
//! This is what makes the pipeline testable in CI with no camera attached,
//! and what lets contributors reproduce published error rates exactly.

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
        let mut w =
            SessionWriter::create(dir, SessionLabel::Synthetic, caps()).unwrap();
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
```

Add to `crates/namtor-capture/src/lib.rs`:

```rust
pub mod replay;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-capture replay::`
Expected: FAIL — `cannot find struct FileReplay in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-capture/src/replay.rs` above the `tests` module:

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-capture replay::`
Expected: PASS — 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-capture/src/replay.rs crates/namtor-capture/src/lib.rs
git commit -m "feat(capture): FileReplay source for camera-free testing"
```

---

### Task 8: Deterministic synthetic session generator

**Files:**
- Create: `crates/namtor-capture/src/synth.rs`
- Modify: `crates/namtor-capture/src/lib.rs`

**Interfaces:**
- Consumes: `SessionWriter`, `SessionLabel`, `Frame`, `SourceCaps`
- Produces: `SynthOpts { pairs: u32, seed: u64, geometry: SynthGeometry, label: SessionLabel }`; `SynthGeometry` enum (`Face`, `Flat`); `synth_session(dir: &Path, opts: &SynthOpts) -> Result<(), CaptureError>`.

**Why `SynthGeometry`:** `Face` renders a radial illumination falloff (nose nearer than ears, spec §6.4 signal 3). `Flat` renders uniform illumination, simulating a printed photo. This gives M6's analytic PAD gates something to discriminate in CI before any real attack data exists.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-capture/src/synth.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only
//! Deterministic synthetic capture sessions.
//!
//! Gives CI frame data with no camera attached. Not a substitute for real
//! captures in evaluation — synthetic sessions are labelled
//! [`SessionLabel::Synthetic`] and are excluded from published error rates.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::EmitterState;
    use crate::recording::SessionLabel;
    use crate::replay::FileReplay;
    use crate::source::CaptureSource;

    fn mean(data: &[u8]) -> f64 {
        data.iter().map(|&b| b as f64).sum::<f64>() / data.len() as f64
    }

    #[test]
    fn generated_session_alternates_emitter_state() {
        let dir = tempfile::tempdir().unwrap();
        synth_session(
            dir.path(),
            &SynthOpts {
                pairs: 3,
                seed: 42,
                geometry: SynthGeometry::Face,
                label: SessionLabel::Synthetic,
            },
        )
        .unwrap();

        let mut r = FileReplay::open(dir.path()).unwrap();
        r.start().unwrap();

        let mut states = Vec::new();
        while let Some(f) = r.next_frame().unwrap() {
            states.push(f.emitter);
        }

        assert_eq!(states.len(), 6, "3 pairs should yield 6 frames");
        assert_eq!(
            states,
            vec![
                EmitterState::On,
                EmitterState::Off,
                EmitterState::On,
                EmitterState::Off,
                EmitterState::On,
                EmitterState::Off,
            ]
        );
    }

    #[test]
    fn emitter_on_frames_are_brighter_than_off_frames() {
        let dir = tempfile::tempdir().unwrap();
        synth_session(
            dir.path(),
            &SynthOpts {
                pairs: 2,
                seed: 7,
                geometry: SynthGeometry::Face,
                label: SessionLabel::Synthetic,
            },
        )
        .unwrap();

        let mut r = FileReplay::open(dir.path()).unwrap();
        r.start().unwrap();

        let on = r.next_frame().unwrap().unwrap();
        let off = r.next_frame().unwrap().unwrap();

        assert!(
            mean(&on.data) > mean(&off.data) + 10.0,
            "emitter-on frame must be measurably brighter: on={}, off={}",
            mean(&on.data),
            mean(&off.data)
        );
    }

    #[test]
    fn face_and_flat_geometries_differ_in_the_differential() {
        // The radial falloff signal (spec §6.4) must be present for Face and
        // absent for Flat, so analytic PAD has something to separate.
        let centre_minus_edge = |g: SynthGeometry| -> f64 {
            let dir = tempfile::tempdir().unwrap();
            synth_session(
                dir.path(),
                &SynthOpts {
                    pairs: 1,
                    seed: 1,
                    geometry: g,
                    label: SessionLabel::Synthetic,
                },
            )
            .unwrap();

            let mut r = FileReplay::open(dir.path()).unwrap();
            r.start().unwrap();
            let on = r.next_frame().unwrap().unwrap();
            let off = r.next_frame().unwrap().unwrap();

            let w = on.width as usize;
            let h = on.height as usize;
            let centre = on.data[(h / 2) * w + w / 2] as f64 - off.data[(h / 2) * w + w / 2] as f64;
            let edge = on.data[(h / 2) * w] as f64 - off.data[(h / 2) * w] as f64;
            centre - edge
        };

        let face = centre_minus_edge(SynthGeometry::Face);
        let flat = centre_minus_edge(SynthGeometry::Flat);

        assert!(face > 20.0, "face geometry must show radial falloff, got {face}");
        assert!(flat.abs() < 5.0, "flat geometry must be uniform, got {flat}");
    }

    #[test]
    fn generation_is_deterministic_for_a_given_seed() {
        let render = |seed: u64| -> Vec<u8> {
            let dir = tempfile::tempdir().unwrap();
            synth_session(
                dir.path(),
                &SynthOpts {
                    pairs: 1,
                    seed,
                    geometry: SynthGeometry::Face,
                    label: SessionLabel::Synthetic,
                },
            )
            .unwrap();
            let mut r = FileReplay::open(dir.path()).unwrap();
            r.start().unwrap();
            r.next_frame().unwrap().unwrap().data
        };

        assert_eq!(render(99), render(99), "same seed must produce same frames");
        assert_ne!(render(1), render(2), "different seeds must differ");
    }
}
```

Add to `crates/namtor-capture/src/lib.rs`:

```rust
pub mod synth;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-capture synth::`
Expected: FAIL — `cannot find function synth_session in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-capture/src/synth.rs` above the `tests` module:

```rust
use crate::frame::{EmitterState, Frame, PixFmt};
use crate::recording::{SessionLabel, SessionWriter};
use crate::source::{CaptureError, SourceCaps, Spectrum};
use std::path::Path;
use std::time::Duration;

/// Illumination geometry of the synthetic subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynthGeometry {
    /// Radial falloff: centre brighter than edges, as a real face under a
    /// point emitter. Spec §6.4 signal 3.
    Face,
    /// Uniform illumination, as a flat printed photo.
    Flat,
}

pub struct SynthOpts {
    /// Number of emitter on/off pairs. Produces `pairs * 2` frames.
    pub pairs: u32,
    pub seed: u64,
    pub geometry: SynthGeometry,
    pub label: SessionLabel,
}

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const AMBIENT: f64 = 40.0;
const EMITTER_GAIN: f64 = 90.0;

/// Deterministic pseudo-random noise in 0.0..1.0, derived from seed and index.
/// Not cryptographic; only needs to be reproducible and decorrelated.
fn noise(seed: u64, index: u64) -> f64 {
    let mut x = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(index.wrapping_mul(1_442_695_040_888_963_407));
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

fn render(opts: &SynthOpts, emitter_on: bool, frame_index: u32) -> Vec<u8> {
    let w = WIDTH as f64;
    let h = HEIGHT as f64;
    let cx = w / 2.0;
    let cy = h / 2.0;
    let max_r = (cx * cx + cy * cy).sqrt();

    let mut data = vec![0u8; (WIDTH * HEIGHT) as usize];

    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let idx = (y * WIDTH + x) as usize;

            let mut value = AMBIENT;

            if emitter_on {
                let contribution = match opts.geometry {
                    SynthGeometry::Flat => EMITTER_GAIN,
                    SynthGeometry::Face => {
                        let dx = x as f64 - cx;
                        let dy = y as f64 - cy;
                        let r = (dx * dx + dy * dy).sqrt() / max_r;
                        // Inverse-square-like falloff from the centre.
                        EMITTER_GAIN / (1.0 + 3.0 * r * r)
                    }
                };
                value += contribution;
            }

            // Small reproducible sensor noise.
            value += noise(opts.seed, (frame_index as u64) << 32 | idx as u64) * 4.0;

            data[idx] = value.clamp(0.0, 255.0) as u8;
        }
    }
    data
}

/// Writes a synthetic session of `opts.pairs` emitter on/off pairs to `dir`.
pub fn synth_session(dir: &Path, opts: &SynthOpts) -> Result<(), CaptureError> {
    let caps = SourceCaps {
        spectrum: Spectrum::Ir,
        format: PixFmt::Grey8,
        width: WIDTH,
        height: HEIGHT,
        fps: 30,
        has_metadata: true,
    };

    let mut w = SessionWriter::create(dir, opts.label.clone(), caps)?;

    for i in 0..(opts.pairs * 2) {
        let on = i % 2 == 0;
        w.push(&Frame {
            data: render(opts, on, i),
            format: PixFmt::Grey8,
            width: WIDTH,
            height: HEIGHT,
            ts_mono: Duration::from_nanos(33_333_333u64 * i as u64),
            sof: Some(i as u16),
            emitter: if on {
                EmitterState::On
            } else {
                EmitterState::Off
            },
            quality: None,
        })?;
    }

    w.finish()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-capture synth::`
Expected: PASS — 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-capture/src/synth.rs crates/namtor-capture/src/lib.rs
git commit -m "feat(capture): deterministic synthetic session generator"
```

---

### Task 9: Score record types and JSONL emission

**Files:**
- Create: `crates/namtor-eval/src/score.rs`
- Modify: `crates/namtor-eval/src/lib.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks
- Produces: `LatencyBreakdown { capture_ms, detect_ms, embed_ms, pad_ms, total_ms }`; `ScoreRecord { probe_id, gallery_id, genuine, score, pad_score, attack_type, latency }`; `write_jsonl(path: &Path, records: &[ScoreRecord]) -> Result<(), ScoreError>`; `read_jsonl(path: &Path) -> Result<Vec<ScoreRecord>, ScoreError>`.

**Note:** `ScoreRecord` carries no threshold and no accept/reject decision. Thresholds are swept in Python (Task 12), so one score file supports every operating point without re-running inference.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-eval/src/score.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only
//! Score records: the seam between the Rust pipeline and the Python harness.

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
```

Add to `crates/namtor-eval/src/lib.rs`:

```rust
pub mod score;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-eval score::`
Expected: FAIL — `cannot find struct ScoreRecord in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-eval/src/score.rs` above the `tests` module:

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-eval score::`
Expected: PASS — 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-eval/src/score.rs crates/namtor-eval/src/lib.rs
git commit -m "feat(eval): score record types and JSONL emission"
```

---

### Task 10: Python package skeleton and record loader

**Files:**
- Create: `ml/eval/pyproject.toml`
- Create: `ml/eval/namtor_eval/__init__.py`
- Create: `ml/eval/namtor_eval/records.py`
- Create: `ml/eval/tests/test_records.py`

**Interfaces:**
- Consumes: the JSONL format produced by Task 9
- Produces: `Latency` dataclass; `ScoreRecord` dataclass with fields `probe_id, gallery_id, genuine, score, pad_score, attack_type, latency`; `load_jsonl(path) -> list[ScoreRecord]`; `ScoreRecord.is_attack` property.

- [ ] **Step 1: Write the failing test**

Create `ml/eval/tests/test_records.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
import json

from namtor_eval.records import ScoreRecord, load_jsonl


def _row(genuine: bool, score: float, attack: str | None = None) -> dict:
    return {
        "probe_id": "p001",
        "gallery_id": "g001",
        "genuine": genuine,
        "score": score,
        "pad_score": 0.9,
        "attack_type": attack,
        "latency": {
            "capture_ms": 66.0,
            "detect_ms": 5.0,
            "embed_ms": 20.0,
            "pad_ms": 3.0,
            "total_ms": 94.0,
        },
    }


def test_loads_records_written_by_the_rust_side(tmp_path):
    p = tmp_path / "scores.jsonl"
    p.write_text(
        json.dumps(_row(True, 0.82)) + "\n"
        + json.dumps(_row(False, 0.11, "screen_replay_phone")) + "\n"
    )

    records = load_jsonl(p)

    assert len(records) == 2
    assert records[0].genuine is True
    assert records[0].latency.total_ms == 94.0
    assert records[1].attack_type == "screen_replay_phone"


def test_is_attack_distinguishes_impostors_from_presentation_attacks(tmp_path):
    p = tmp_path / "scores.jsonl"
    p.write_text(
        json.dumps(_row(False, 0.11)) + "\n"
        + json.dumps(_row(False, 0.44, "print_colour_laser")) + "\n"
    )

    records = load_jsonl(p)

    # A zero-effort impostor is not a presentation attack. Conflating them
    # would corrupt both FAR and APCER.
    assert records[0].is_attack is False
    assert records[1].is_attack is True


def test_blank_lines_are_ignored(tmp_path):
    p = tmp_path / "scores.jsonl"
    p.write_text(json.dumps(_row(True, 0.5)) + "\n\n")
    assert len(load_jsonl(p)) == 1
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd ml/eval && python -m pytest tests/test_records.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'namtor_eval'`.

- [ ] **Step 3: Write minimal implementation**

Create `ml/eval/pyproject.toml`:

```toml
[project]
name = "namtor-eval"
version = "0.1.0"
description = "Evaluation harness for Nam-tor"
requires-python = ">=3.11"
license = { text = "GPL-3.0-only" }
dependencies = ["numpy>=1.26"]

[project.optional-dependencies]
dev = ["pytest>=8.0"]

[build-system]
requires = ["setuptools>=68"]
build-backend = "setuptools.build_meta"

[tool.setuptools.packages.find]
include = ["namtor_eval*"]
```

Create `ml/eval/namtor_eval/__init__.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
"""Evaluation harness for Nam-tor.

Consumes score records emitted by the Rust pipeline and produces the
FAR/FRR, APCER/BPCER, and latency figures that back every published claim.
"""
```

Create `ml/eval/namtor_eval/records.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
"""Score record types mirroring `crates/namtor-eval/src/score.rs`."""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Latency:
    capture_ms: float
    detect_ms: float
    embed_ms: float
    pad_ms: float
    total_ms: float

    @property
    def compute_ms(self) -> float:
        """Everything that is not waiting on the camera."""
        return self.total_ms - self.capture_ms


@dataclass(frozen=True)
class ScoreRecord:
    probe_id: str
    gallery_id: str
    genuine: bool
    score: float
    pad_score: float
    attack_type: str | None
    latency: Latency

    @property
    def is_attack(self) -> bool:
        """True for presentation attacks only.

        A zero-effort impostor has ``attack_type is None`` and is *not* an
        attack presentation. Conflating the two corrupts FAR and APCER alike.
        """
        return self.attack_type is not None


def load_jsonl(path: str | Path) -> list[ScoreRecord]:
    """Loads a JSON Lines score file emitted by the Rust pipeline."""
    records: list[ScoreRecord] = []
    with open(path, encoding="utf-8") as fh:
        for lineno, line in enumerate(fh):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError as exc:
                raise ValueError(f"malformed record at line {lineno}: {exc}") from exc
            records.append(
                ScoreRecord(
                    probe_id=row["probe_id"],
                    gallery_id=row["gallery_id"],
                    genuine=row["genuine"],
                    score=row["score"],
                    pad_score=row["pad_score"],
                    attack_type=row.get("attack_type"),
                    latency=Latency(**row["latency"]),
                )
            )
    return records
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd ml/eval && pip install -e .[dev] && python -m pytest tests/test_records.py -v`
Expected: PASS — 3 tests.

- [ ] **Step 5: Commit**

```bash
git add ml/eval/pyproject.toml ml/eval/namtor_eval ml/eval/tests
git commit -m "feat(eval): python package skeleton and score record loader"
```

---

### Task 11: FAR/FRR sweep with confidence intervals

**Files:**
- Create: `ml/eval/namtor_eval/metrics.py`
- Create: `ml/eval/tests/test_metrics.py`

**Interfaces:**
- Consumes: `ScoreRecord` from Task 10
- Produces: `OperatingPoint { threshold, far, frr, n_genuine, n_impostor, far_ci, frr_ci }`; `far_frr_sweep(records, thresholds=None) -> list[OperatingPoint]`; `point_at_far(points, target_far) -> OperatingPoint`; `wilson_interval(k, n, z=1.96) -> tuple[float, float]`.

**Semantics:** a comparison is accepted when `score >= threshold`. FAR is the fraction of impostor comparisons accepted; FRR is the fraction of genuine comparisons rejected. Presentation attacks are excluded from both — they belong to APCER (Task 12).

- [ ] **Step 1: Write the failing test**

Create `ml/eval/tests/test_metrics.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
import numpy as np
import pytest

from namtor_eval.metrics import far_frr_sweep, point_at_far, wilson_interval
from namtor_eval.records import Latency, ScoreRecord

LAT = Latency(66.0, 5.0, 20.0, 3.0, 94.0)


def rec(genuine: bool, score: float, attack: str | None = None) -> ScoreRecord:
    return ScoreRecord("p", "g", genuine, score, 0.9, attack, LAT)


def test_perfectly_separated_scores_have_zero_error_between_the_clusters():
    records = [
        rec(True, 0.9), rec(True, 0.8), rec(True, 0.7),
        rec(False, 0.3), rec(False, 0.2), rec(False, 0.1),
    ]
    points = far_frr_sweep(records, thresholds=[0.5])
    assert len(points) == 1
    assert points[0].far == pytest.approx(0.0)
    assert points[0].frr == pytest.approx(0.0)
    assert points[0].n_genuine == 3
    assert points[0].n_impostor == 3


def test_threshold_above_a_genuine_score_rejects_it():
    records = [
        rec(True, 0.9), rec(True, 0.8), rec(True, 0.7),
        rec(False, 0.3), rec(False, 0.2), rec(False, 0.1),
    ]
    points = far_frr_sweep(records, thresholds=[0.75])
    # 0.7 falls below threshold: one of three genuine rejected.
    assert points[0].frr == pytest.approx(1 / 3)
    assert points[0].far == pytest.approx(0.0)


def test_threshold_below_an_impostor_score_accepts_it():
    records = [rec(True, 0.9), rec(False, 0.3), rec(False, 0.1)]
    points = far_frr_sweep(records, thresholds=[0.2])
    assert points[0].far == pytest.approx(0.5)


def test_presentation_attacks_are_excluded_from_far():
    # Attacks belong to APCER, never to FAR. Including them would understate
    # or overstate FAR depending on attack difficulty.
    records = [
        rec(True, 0.9),
        rec(False, 0.1),
        rec(False, 0.95, "print_colour_laser"),
    ]
    points = far_frr_sweep(records, thresholds=[0.5])
    assert points[0].n_impostor == 1
    assert points[0].far == pytest.approx(0.0)


def test_point_at_far_returns_the_strictest_point_meeting_the_target():
    records = [rec(True, s / 100) for s in range(50, 100)] + [
        rec(False, s / 100) for s in range(0, 50)
    ]
    points = far_frr_sweep(records)
    p = point_at_far(points, 1e-5)
    assert p.far <= 1e-5


def test_point_at_far_raises_when_target_is_unreachable():
    records = [rec(True, 0.5), rec(False, 0.5)]
    points = far_frr_sweep(records)
    with pytest.raises(ValueError, match="no operating point"):
        point_at_far(points, 0.0)


def test_wilson_interval_widens_as_sample_shrinks():
    lo_big, hi_big = wilson_interval(1, 100_000)
    lo_small, hi_small = wilson_interval(1, 10)
    assert (hi_big - lo_big) < (hi_small - lo_small)


def test_wilson_interval_of_zero_events_is_bounded_below_by_zero():
    lo, hi = wilson_interval(0, 1000)
    assert lo == pytest.approx(0.0)
    assert 0.0 < hi < 0.01


def test_sweep_is_fast_enough_for_the_sample_size_gate_g3_requires():
    # Gate G3.2 requires >=100k impostor comparisons. A per-threshold boolean
    # mask would be O(T*N) and take hours here; the sorted binary search is
    # O((N+T) log N). This test fails loudly if the implementation regresses.
    import time

    rng = np.random.default_rng(0)
    records = [rec(True, float(s)) for s in rng.normal(0.8, 0.05, 5_000)] + [
        rec(False, float(s)) for s in rng.normal(0.2, 0.05, 100_000)
    ]

    start = time.perf_counter()
    points = far_frr_sweep(records)
    elapsed = time.perf_counter() - start

    assert len(points) > 1000
    assert elapsed < 5.0, f"sweep took {elapsed:.1f}s; implementation is O(T*N)"


def test_sweep_counts_match_a_naive_reference_implementation():
    # Guards the binary-search boundary conditions, which are easy to get
    # off by one against the >= / < semantics.
    rng = np.random.default_rng(1)
    scores = np.round(rng.uniform(0, 1, 200), 3)
    records = [rec(i % 2 == 0, float(s)) for i, s in enumerate(scores)]

    genuine = [r.score for r in records if r.genuine]
    impostor = [r.score for r in records if not r.genuine]

    for p in far_frr_sweep(records):
        naive_far = sum(1 for s in impostor if s >= p.threshold) / len(impostor)
        naive_frr = sum(1 for s in genuine if s < p.threshold) / len(genuine)
        assert p.far == pytest.approx(naive_far)
        assert p.frr == pytest.approx(naive_frr)
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd ml/eval && python -m pytest tests/test_metrics.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'namtor_eval.metrics'`.

- [ ] **Step 3: Write minimal implementation**

Create `ml/eval/namtor_eval/metrics.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
"""FAR/FRR computation with confidence intervals.

A comparison is *accepted* when ``score >= threshold``. FAR is the fraction of
zero-effort impostor comparisons accepted; FRR the fraction of genuine
comparisons rejected. Presentation attacks are excluded — they are measured by
APCER in :mod:`namtor_eval.pad`.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

import numpy as np

from .records import ScoreRecord


@dataclass(frozen=True)
class OperatingPoint:
    threshold: float
    far: float
    frr: float
    n_genuine: int
    n_impostor: int
    far_ci: tuple[float, float]
    frr_ci: tuple[float, float]


def wilson_interval(k: int, n: int, z: float = 1.96) -> tuple[float, float]:
    """Wilson score interval for a binomial proportion.

    Used instead of the normal approximation because error rates here are near
    zero with large n, where the normal approximation is badly behaved.
    """
    if n == 0:
        return (0.0, 1.0)
    p = k / n
    denom = 1.0 + z * z / n
    centre = (p + z * z / (2 * n)) / denom
    margin = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / denom
    return (max(0.0, centre - margin), min(1.0, centre + margin))


def far_frr_sweep(
    records: list[ScoreRecord],
    thresholds: list[float] | None = None,
) -> list[OperatingPoint]:
    """Computes FAR and FRR across a threshold sweep.

    When ``thresholds`` is omitted, every observed score is used as a
    candidate threshold, which is sufficient to recover the exact DET curve.
    """
    genuine = np.sort(np.array([r.score for r in records if r.genuine], dtype=float))
    impostor = np.sort(
        np.array(
            [r.score for r in records if not r.genuine and not r.is_attack],
            dtype=float,
        )
    )

    if thresholds is None:
        both = np.concatenate([genuine, impostor]) if len(records) else np.array([])
        candidates = np.unique(both)
    else:
        candidates = np.unique(np.asarray(list(thresholds), dtype=float))

    n_g = len(genuine)
    n_i = len(impostor)

    # Counting per threshold with a boolean mask is O(T*N), which at the
    # >=100k impostor comparisons gate G3.2 requires would be ~1e10 operations
    # and take hours. Both arrays are sorted, so binary search gives the same
    # counts in O((N + T) log N).
    #
    #   false accepts  = impostor >= t  = n_i - searchsorted(impostor, t, 'left')
    #   false rejects  = genuine  < t   =       searchsorted(genuine,  t, 'left')
    false_accepts = n_i - np.searchsorted(impostor, candidates, side="left")
    false_rejects = np.searchsorted(genuine, candidates, side="left")

    points: list[OperatingPoint] = []
    for t, fa, fr in zip(candidates, false_accepts, false_rejects):
        points.append(
            OperatingPoint(
                threshold=float(t),
                far=int(fa) / n_i if n_i else 0.0,
                frr=int(fr) / n_g if n_g else 0.0,
                n_genuine=n_g,
                n_impostor=n_i,
                far_ci=wilson_interval(int(fa), n_i),
                frr_ci=wilson_interval(int(fr), n_g),
            )
        )
    return points


def point_at_far(
    points: list[OperatingPoint], target_far: float
) -> OperatingPoint:
    """Returns the point with lowest FRR among those meeting ``target_far``."""
    eligible = [p for p in points if p.far <= target_far]
    if not eligible:
        raise ValueError(f"no operating point achieves FAR <= {target_far}")
    return min(eligible, key=lambda p: p.frr)
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd ml/eval && python -m pytest tests/test_metrics.py -v`
Expected: PASS — 10 tests. The performance test sweeps 105k records and should finish well under a second.

- [ ] **Step 5: Commit**

```bash
git add ml/eval/namtor_eval/metrics.py ml/eval/tests/test_metrics.py
git commit -m "feat(eval): FAR/FRR sweep with Wilson confidence intervals"
```

---

### Task 12: APCER and BPCER per attack type

**Files:**
- Create: `ml/eval/namtor_eval/pad.py`
- Create: `ml/eval/tests/test_pad.py`

**Interfaces:**
- Consumes: `ScoreRecord` from Task 10
- Produces: `PadReport { threshold, bpcer, n_bonafide, apcer: dict[str, float], n_attack: dict[str, int], apcer_ci: dict[str, tuple[float, float]] }`; `pad_rates(records, threshold) -> PadReport`; `PadReport.worst_apcer() -> tuple[str, float]`.

**Semantics:** a presentation is classified bona fide when `pad_score >= threshold`. APCER is the fraction of attacks of a given type wrongly classified bona fide. BPCER is the fraction of bona fide presentations wrongly classified as attacks. Per spec §10.6, APCER is **never averaged across attack types**.

- [ ] **Step 1: Write the failing test**

Create `ml/eval/tests/test_pad.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
import pytest

from namtor_eval.pad import pad_rates
from namtor_eval.records import Latency, ScoreRecord

LAT = Latency(66.0, 5.0, 20.0, 3.0, 94.0)


def rec(pad_score: float, attack: str | None) -> ScoreRecord:
    genuine = attack is None
    return ScoreRecord("p", "g", genuine, 0.8, pad_score, attack, LAT)


def test_apcer_is_reported_per_attack_type_not_averaged():
    records = [
        rec(0.9, None), rec(0.9, None),                     # bona fide
        rec(0.1, "print_colour_laser"),                     # correctly rejected
        rec(0.9, "screen_replay_phone"),                    # wrongly accepted
        rec(0.9, "screen_replay_phone"),                    # wrongly accepted
    ]
    report = pad_rates(records, threshold=0.5)

    assert report.apcer["print_colour_laser"] == pytest.approx(0.0)
    assert report.apcer["screen_replay_phone"] == pytest.approx(1.0)
    # An averaged figure would report 0.5 and hide that replay defeats us.
    assert "average" not in report.apcer


def test_bpcer_counts_bona_fide_wrongly_called_attacks():
    records = [
        rec(0.9, None), rec(0.9, None), rec(0.1, None), rec(0.9, None),
        rec(0.1, "print_colour_laser"),
    ]
    report = pad_rates(records, threshold=0.5)
    assert report.bpcer == pytest.approx(0.25)
    assert report.n_bonafide == 4


def test_sample_counts_are_reported_per_attack_type():
    records = [
        rec(0.9, None),
        rec(0.1, "print_ir_optimised"),
        rec(0.2, "print_ir_optimised"),
        rec(0.9, "mask_2d_cutout"),
    ]
    report = pad_rates(records, threshold=0.5)
    assert report.n_attack["print_ir_optimised"] == 2
    assert report.n_attack["mask_2d_cutout"] == 1


def test_worst_apcer_surfaces_the_attack_that_works():
    records = [
        rec(0.9, None),
        rec(0.1, "print_colour_laser"),
        rec(0.9, "screen_replay_monitor"),
    ]
    report = pad_rates(records, threshold=0.5)
    attack, rate = report.worst_apcer()
    assert attack == "screen_replay_monitor"
    assert rate == pytest.approx(1.0)


def test_empty_attack_set_yields_empty_apcer_not_a_crash():
    report = pad_rates([rec(0.9, None)], threshold=0.5)
    assert report.apcer == {}
    assert report.worst_apcer() is None
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd ml/eval && python -m pytest tests/test_pad.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'namtor_eval.pad'`.

- [ ] **Step 3: Write minimal implementation**

Create `ml/eval/namtor_eval/pad.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
"""Presentation attack detection rates, per ISO/IEC 30107-3.

APCER is reported **per attack type and never averaged**. A single averaged
figure conceals exactly the attack that works, which is the one that matters.
"""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass

from .metrics import wilson_interval
from .records import ScoreRecord


@dataclass(frozen=True)
class PadReport:
    threshold: float
    bpcer: float
    n_bonafide: int
    apcer: dict[str, float]
    n_attack: dict[str, int]
    apcer_ci: dict[str, tuple[float, float]]

    def worst_apcer(self) -> tuple[str, float] | None:
        """The attack type with the highest acceptance rate, or ``None``."""
        if not self.apcer:
            return None
        return max(self.apcer.items(), key=lambda kv: kv[1])


def pad_rates(records: list[ScoreRecord], threshold: float) -> PadReport:
    """Computes BPCER and per-type APCER at a PAD threshold.

    A presentation is classified bona fide when ``pad_score >= threshold``.
    """
    bonafide = [r for r in records if not r.is_attack]
    n_bonafide = len(bonafide)
    false_rejects = sum(1 for r in bonafide if r.pad_score < threshold)

    accepted: dict[str, int] = defaultdict(int)
    totals: dict[str, int] = defaultdict(int)
    for r in records:
        if not r.is_attack:
            continue
        assert r.attack_type is not None
        totals[r.attack_type] += 1
        if r.pad_score >= threshold:
            accepted[r.attack_type] += 1

    apcer = {k: accepted[k] / totals[k] for k in totals}
    apcer_ci = {k: wilson_interval(accepted[k], totals[k]) for k in totals}

    return PadReport(
        threshold=threshold,
        bpcer=false_rejects / n_bonafide if n_bonafide else 0.0,
        n_bonafide=n_bonafide,
        apcer=apcer,
        n_attack=dict(totals),
        apcer_ci=apcer_ci,
    )
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd ml/eval && python -m pytest tests/test_pad.py -v`
Expected: PASS — 5 tests.

- [ ] **Step 5: Commit**

```bash
git add ml/eval/namtor_eval/pad.py ml/eval/tests/test_pad.py
git commit -m "feat(eval): per-attack-type APCER and BPCER"
```

---

### Task 13: Latency statistics

**Files:**
- Create: `ml/eval/namtor_eval/latency.py`
- Create: `ml/eval/tests/test_latency.py`

**Interfaces:**
- Consumes: `ScoreRecord`, `Latency` from Task 10
- Produces: `LatencyStats { n, median_ms, p95_ms, p99_ms, median_capture_ms, median_compute_ms }`; `latency_stats(records) -> LatencyStats`; `meets_gate_g2(stats) -> bool`.

**Gate:** spec §12 G2.1 requires median ≤ 500 ms, G2.2 requires p95 ≤ 1000 ms.

- [ ] **Step 1: Write the failing test**

Create `ml/eval/tests/test_latency.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
import pytest

from namtor_eval.latency import latency_stats, meets_gate_g2
from namtor_eval.records import Latency, ScoreRecord


def rec(total_ms: float, capture_ms: float = 66.0) -> ScoreRecord:
    compute = total_ms - capture_ms
    return ScoreRecord(
        "p", "g", True, 0.8, 0.9, None,
        Latency(capture_ms, compute * 0.2, compute * 0.7, compute * 0.1, total_ms),
    )


def test_median_and_percentiles():
    records = [rec(float(t)) for t in range(100, 200)]
    s = latency_stats(records)
    assert s.n == 100
    assert s.median_ms == pytest.approx(149.5, abs=1.0)
    assert s.p95_ms == pytest.approx(194.0, abs=2.0)


def test_capture_and_compute_are_reported_separately():
    # Spec §12 G2 requires the split, because capture dominates and the
    # distinction determines where optimisation effort belongs.
    records = [rec(100.0, capture_ms=66.0) for _ in range(10)]
    s = latency_stats(records)
    assert s.median_capture_ms == pytest.approx(66.0)
    assert s.median_compute_ms == pytest.approx(34.0)


def test_gate_g2_passes_within_budget():
    records = [rec(110.0) for _ in range(100)]
    assert meets_gate_g2(latency_stats(records)) is True


def test_gate_g2_fails_when_median_exceeds_500ms():
    records = [rec(600.0) for _ in range(100)]
    assert meets_gate_g2(latency_stats(records)) is False


def test_gate_g2_fails_when_p95_exceeds_1000ms():
    records = [rec(100.0) for _ in range(95)] + [rec(1500.0) for _ in range(5)]
    stats = latency_stats(records)
    assert stats.median_ms < 500
    assert meets_gate_g2(stats) is False


def test_empty_input_raises_rather_than_reporting_zero():
    # Reporting 0 ms for no data would silently pass the gate.
    with pytest.raises(ValueError, match="no records"):
        latency_stats([])
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd ml/eval && python -m pytest tests/test_latency.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'namtor_eval.latency'`.

- [ ] **Step 3: Write minimal implementation**

Create `ml/eval/namtor_eval/latency.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
"""End-to-end latency statistics and gate G2 evaluation."""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

from .records import ScoreRecord

#: Spec §12 G2.1
GATE_G2_MEDIAN_MS = 500.0
#: Spec §12 G2.2
GATE_G2_P95_MS = 1000.0


@dataclass(frozen=True)
class LatencyStats:
    n: int
    median_ms: float
    p95_ms: float
    p99_ms: float
    median_capture_ms: float
    median_compute_ms: float


def latency_stats(records: list[ScoreRecord]) -> LatencyStats:
    """Computes the latency distribution, split into capture and compute."""
    if not records:
        raise ValueError("no records: refusing to report latency for an empty set")

    total = np.array([r.latency.total_ms for r in records], dtype=float)
    capture = np.array([r.latency.capture_ms for r in records], dtype=float)
    compute = np.array([r.latency.compute_ms for r in records], dtype=float)

    return LatencyStats(
        n=len(records),
        median_ms=float(np.median(total)),
        p95_ms=float(np.percentile(total, 95)),
        p99_ms=float(np.percentile(total, 99)),
        median_capture_ms=float(np.median(capture)),
        median_compute_ms=float(np.median(compute)),
    )


def meets_gate_g2(stats: LatencyStats) -> bool:
    """True when both G2.1 (median) and G2.2 (p95) thresholds are met."""
    return stats.median_ms <= GATE_G2_MEDIAN_MS and stats.p95_ms <= GATE_G2_P95_MS
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd ml/eval && python -m pytest tests/test_latency.py -v`
Expected: PASS — 6 tests.

- [ ] **Step 5: Commit**

```bash
git add ml/eval/namtor_eval/latency.py ml/eval/tests/test_latency.py
git commit -m "feat(eval): latency statistics and gate G2 evaluation"
```

---

### Task 14: Report renderer with the no-rate-without-N rule

**Files:**
- Create: `ml/eval/namtor_eval/report.py`
- Create: `ml/eval/tests/test_report.py`

**Interfaces:**
- Consumes: `OperatingPoint`, `PadReport`, `LatencyStats` from Tasks 11–13
- Produces: `render_report(*, title, operating_point, pad_report, latency, config_note) -> str` returning Markdown; `write_csv(path, points) -> None`.

**Rule under test:** spec §10.6 states "a number without its sample size is not published." The renderer enforces this, and a test asserts it — so a future change that drops sample sizes fails CI rather than shipping an unfalsifiable claim.

- [ ] **Step 1: Write the failing test**

Create `ml/eval/tests/test_report.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
import re

from namtor_eval.latency import LatencyStats
from namtor_eval.metrics import OperatingPoint
from namtor_eval.pad import PadReport
from namtor_eval.report import render_report, write_csv

POINT = OperatingPoint(
    threshold=0.62,
    far=8e-6,
    frr=0.021,
    n_genuine=4_000,
    n_impostor=250_000,
    far_ci=(2e-6, 2.4e-5),
    frr_ci=(0.017, 0.026),
)

PAD = PadReport(
    threshold=0.5,
    bpcer=0.03,
    n_bonafide=4_000,
    apcer={"print_colour_laser": 0.01, "screen_replay_phone": 0.0},
    n_attack={"print_colour_laser": 500, "screen_replay_phone": 500},
    apcer_ci={"print_colour_laser": (0.004, 0.024), "screen_replay_phone": (0.0, 0.008)},
)

LAT = LatencyStats(
    n=4_000, median_ms=112.0, p95_ms=430.0, p99_ms=610.0,
    median_capture_ms=66.0, median_compute_ms=46.0,
)


def _report() -> str:
    return render_report(
        title="Nam-tor eval — balanced",
        operating_point=POINT,
        pad_report=PAD,
        latency=LAT,
        config_note="AdaFace R50 INT8, 5-pair burst, per-user z-norm",
    )


def test_report_contains_the_required_sections():
    md = _report()
    for section in ("## Accuracy", "## Presentation attack detection", "## Latency"):
        assert section in md, f"missing section: {section}"


def test_every_reported_rate_carries_its_sample_size():
    # Spec §10.6: "A number without its sample size is not published."
    md = _report()
    rate_lines = [
        line for line in md.splitlines()
        if re.search(r"\b(FAR|FRR|APCER|BPCER)\b", line) and "|" in line
    ]
    assert rate_lines, "no rate rows found in report"
    for line in rate_lines:
        assert re.search(r"n\s*=\s*[\d,]+", line), f"rate without sample size: {line}"


def test_apcer_rows_are_per_attack_type():
    md = _report()
    assert "print_colour_laser" in md
    assert "screen_replay_phone" in md


def test_gate_verdicts_are_stated_explicitly():
    md = _report()
    assert "G2" in md and "G3" in md and "G4" in md


def test_confidence_intervals_are_shown():
    md = _report()
    assert "95% CI" in md


def test_operating_points_an_order_of_magnitude_apart_render_differently():
    # Fixed 3-decimal percent renders 1e-5 and 1e-6 identically as "0.001%",
    # which would make Balanced and Paranoid indistinguishable in the report
    # whose purpose is to compare them.
    from namtor_eval.report import _pct

    assert _pct(1e-5) != _pct(1e-6)
    assert _pct(0.0) == "0.000%"
    assert _pct(0.021) == "2.100%"


def test_csv_export_has_a_header_and_one_row_per_point(tmp_path):
    p = tmp_path / "det.csv"
    write_csv(p, [POINT, POINT])
    lines = p.read_text().strip().splitlines()
    assert lines[0].startswith("threshold,far,frr")
    assert len(lines) == 3
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd ml/eval && python -m pytest tests/test_report.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'namtor_eval.report'`.

- [ ] **Step 3: Write minimal implementation**

Create `ml/eval/namtor_eval/report.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
"""Markdown and CSV rendering of evaluation results.

Enforces the spec §10.6 rule that no rate is published without its sample
size. The corresponding test fails CI if that rule is ever weakened.
"""

from __future__ import annotations

import csv
from pathlib import Path

from .latency import (
    GATE_G2_MEDIAN_MS,
    GATE_G2_P95_MS,
    LatencyStats,
    meets_gate_g2,
)
from .metrics import OperatingPoint
from .pad import PadReport

#: Spec §12 G3.1
GATE_G3_FAR = 1e-5
GATE_G3_FRR = 0.03
#: Spec §12 G4.1
GATE_G4_APCER = 0.05
GATE_G4_BPCER = 0.05


def _pct(x: float) -> str:
    """Formats a rate as a percentage with enough precision to be useful.

    Fixed 3-decimal percent renders both 1e-5 and 1e-6 as "0.001%", which
    would make the Balanced and Paranoid operating points indistinguishable
    in the report that exists to compare them. Small rates therefore get
    additional digits.
    """
    if x <= 0.0:
        return "0.000%"
    if x < 1e-3:
        return f"{x * 100:.6f}%"
    return f"{x * 100:.3f}%"


def _verdict(passed: bool) -> str:
    return "**PASS**" if passed else "**FAIL**"


def render_report(
    *,
    title: str,
    operating_point: OperatingPoint,
    pad_report: PadReport,
    latency: LatencyStats,
    config_note: str,
) -> str:
    """Renders a full evaluation report as Markdown."""
    p = operating_point

    g3_pass = p.far <= GATE_G3_FAR and p.frr <= GATE_G3_FRR
    g2_pass = meets_gate_g2(latency)
    worst = pad_report.worst_apcer()
    g4_pass = (
        pad_report.bpcer <= GATE_G4_BPCER
        and (worst is None or worst[1] <= GATE_G4_APCER)
    )

    lines: list[str] = [
        f"# {title}",
        "",
        f"Configuration: {config_note}",
        "",
        "## Accuracy",
        "",
        f"Threshold: `{p.threshold:.4f}`",
        "",
        "| Metric | Value | 95% CI | Sample |",
        "|---|---|---|---|",
        f"| FAR | {_pct(p.far)} | {_pct(p.far_ci[0])} – {_pct(p.far_ci[1])} "
        f"| n = {p.n_impostor:,} |",
        f"| FRR | {_pct(p.frr)} | {_pct(p.frr_ci[0])} – {_pct(p.frr_ci[1])} "
        f"| n = {p.n_genuine:,} |",
        "",
        f"Gate G3 (FAR ≤ {_pct(GATE_G3_FAR)} at FRR ≤ {_pct(GATE_G3_FRR)}): "
        f"{_verdict(g3_pass)}",
        "",
        "## Presentation attack detection",
        "",
        f"PAD threshold: `{pad_report.threshold:.4f}`",
        "",
        "| Presentation | Rate | 95% CI | Sample |",
        "|---|---|---|---|",
        f"| BPCER | {_pct(pad_report.bpcer)} | — | n = {pad_report.n_bonafide:,} |",
    ]

    for attack in sorted(pad_report.apcer):
        ci = pad_report.apcer_ci[attack]
        lines.append(
            f"| APCER `{attack}` | {_pct(pad_report.apcer[attack])} "
            f"| {_pct(ci[0])} – {_pct(ci[1])} | n = {pad_report.n_attack[attack]:,} |"
        )

    lines += [
        "",
        "APCER is reported per attack type and never averaged (spec §10.6).",
        "",
        f"Gate G4 (APCER ≤ {_pct(GATE_G4_APCER)} per type at "
        f"BPCER ≤ {_pct(GATE_G4_BPCER)}): {_verdict(g4_pass)}",
        "",
        "## Latency",
        "",
        "| Statistic | Value |",
        "|---|---|",
        f"| Median | {latency.median_ms:.1f} ms |",
        f"| p95 | {latency.p95_ms:.1f} ms |",
        f"| p99 | {latency.p99_ms:.1f} ms |",
        f"| Median capture | {latency.median_capture_ms:.1f} ms |",
        f"| Median compute | {latency.median_compute_ms:.1f} ms |",
        f"| Attempts | n = {latency.n:,} |",
        "",
        f"Gate G2 (median ≤ {GATE_G2_MEDIAN_MS:.0f} ms, "
        f"p95 ≤ {GATE_G2_P95_MS:.0f} ms): {_verdict(g2_pass)}",
        "",
    ]
    return "\n".join(lines)


def write_csv(path: str | Path, points: list[OperatingPoint]) -> None:
    """Writes the DET sweep as CSV for plotting."""
    with open(path, "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(
            ["threshold", "far", "frr", "n_genuine", "n_impostor",
             "far_ci_lo", "far_ci_hi", "frr_ci_lo", "frr_ci_hi"]
        )
        for p in points:
            w.writerow(
                [p.threshold, p.far, p.frr, p.n_genuine, p.n_impostor,
                 p.far_ci[0], p.far_ci[1], p.frr_ci[0], p.frr_ci[1]]
            )
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd ml/eval && python -m pytest tests/test_report.py -v`
Expected: PASS — 7 tests.

- [ ] **Step 5: Commit**

```bash
git add ml/eval/namtor_eval/report.py ml/eval/tests/test_report.py
git commit -m "feat(eval): markdown report renderer enforcing sample-size rule"
```

---

### Task 15: Ablation table runner

**Files:**
- Create: `ml/eval/namtor_eval/ablation.py`
- Create: `ml/eval/tests/test_ablation.py`

**Interfaces:**
- Consumes: `load_jsonl`, `far_frr_sweep`, `point_at_far`, `latency_stats`
- Produces: `AblationRow { name, far, frr, median_ms, n_genuine, n_impostor }`; `ablation_table(runs: dict[str, str | Path], target_far: float) -> list[AblationRow]`; `render_ablation(rows) -> str`.

**Purpose:** spec §10.6's ablation table is what determines which accuracy levers earned their cost. Each run is a score file produced with one lever added.

- [ ] **Step 1: Write the failing test**

Create `ml/eval/tests/test_ablation.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
import json

import pytest

from namtor_eval.ablation import ablation_table, render_ablation


def _write_run(path, genuine_scores, impostor_scores, total_ms):
    lat = {
        "capture_ms": 66.0, "detect_ms": 5.0, "embed_ms": 20.0,
        "pad_ms": 3.0, "total_ms": total_ms,
    }
    rows = []
    for s in genuine_scores:
        rows.append({"probe_id": "p", "gallery_id": "g", "genuine": True,
                     "score": s, "pad_score": 0.9, "attack_type": None,
                     "latency": lat})
    for s in impostor_scores:
        rows.append({"probe_id": "p", "gallery_id": "h", "genuine": False,
                     "score": s, "pad_score": 0.9, "attack_type": None,
                     "latency": lat})
    path.write_text("\n".join(json.dumps(r) for r in rows) + "\n")


def test_table_has_one_row_per_run_in_insertion_order(tmp_path):
    a = tmp_path / "a.jsonl"
    b = tmp_path / "b.jsonl"
    _write_run(a, [0.6] * 50, [0.1] * 50, 100.0)
    _write_run(b, [0.9] * 50, [0.1] * 50, 130.0)

    rows = ablation_table({"baseline": a, "+ flip TTA": b}, target_far=0.0)

    assert [r.name for r in rows] == ["baseline", "+ flip TTA"]


def test_row_carries_sample_sizes_and_latency(tmp_path):
    a = tmp_path / "a.jsonl"
    _write_run(a, [0.8] * 40, [0.1] * 60, 111.0)

    rows = ablation_table({"baseline": a}, target_far=0.0)

    assert rows[0].n_genuine == 40
    assert rows[0].n_impostor == 60
    assert rows[0].median_ms == pytest.approx(111.0)


def test_render_includes_sample_sizes(tmp_path):
    a = tmp_path / "a.jsonl"
    _write_run(a, [0.8] * 40, [0.1] * 60, 111.0)
    md = render_ablation(ablation_table({"baseline": a}, target_far=0.0))
    assert "n = 40" in md
    assert "n = 60" in md


def test_unreachable_target_far_names_the_offending_run(tmp_path):
    a = tmp_path / "a.jsonl"
    # Fully overlapping distributions: no threshold achieves FAR 0 with any
    # genuine accepted.
    _write_run(a, [0.5] * 10, [0.5] * 10, 100.0)

    with pytest.raises(ValueError, match="baseline"):
        ablation_table({"baseline": a}, target_far=0.0)
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd ml/eval && python -m pytest tests/test_ablation.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'namtor_eval.ablation'`.

- [ ] **Step 3: Write minimal implementation**

Create `ml/eval/namtor_eval/ablation.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
"""Ablation table: which accuracy levers earned their cost.

Each run is a score file produced with one additional lever enabled. The table
is the project's primary evidence for design decisions (spec §10.6).
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from .latency import latency_stats
from .metrics import far_frr_sweep, point_at_far
from .records import load_jsonl


@dataclass(frozen=True)
class AblationRow:
    name: str
    far: float
    frr: float
    median_ms: float
    n_genuine: int
    n_impostor: int


def ablation_table(
    runs: dict[str, str | Path], target_far: float
) -> list[AblationRow]:
    """Builds one row per run, each evaluated at the same target FAR."""
    rows: list[AblationRow] = []
    for name, path in runs.items():
        records = load_jsonl(path)
        points = far_frr_sweep(records)
        try:
            point = point_at_far(points, target_far)
        except ValueError as exc:
            raise ValueError(f"run {name!r}: {exc}") from exc

        rows.append(
            AblationRow(
                name=name,
                far=point.far,
                frr=point.frr,
                median_ms=latency_stats(records).median_ms,
                n_genuine=point.n_genuine,
                n_impostor=point.n_impostor,
            )
        )
    return rows


def render_ablation(rows: list[AblationRow]) -> str:
    """Renders the ablation table as Markdown."""
    out = [
        "| Configuration | FAR | FRR | Median latency | Genuine | Impostor |",
        "|---|---|---|---|---|---|",
    ]
    for r in rows:
        out.append(
            f"| {r.name} | {r.far * 100:.4f}% | {r.frr * 100:.3f}% "
            f"| {r.median_ms:.1f} ms | n = {r.n_genuine:,} | n = {r.n_impostor:,} |"
        )
    return "\n".join(out)
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd ml/eval && python -m pytest tests/test_ablation.py -v`
Expected: PASS — 4 tests.

- [ ] **Step 5: Commit**

```bash
git add ml/eval/namtor_eval/ablation.py ml/eval/tests/test_ablation.py
git commit -m "feat(eval): ablation table runner"
```

---

### Task 16: End-to-end smoke test and CI wiring

**Files:**
- Create: `crates/namtor-capture/src/bin/namtor-synth.rs`
- Create: `ml/eval/tests/test_end_to_end.py`
- Modify: `.github/workflows/ci.yml`
- Modify: `crates/namtor-capture/Cargo.toml`

**Interfaces:**
- Consumes: `synth_session`, `SynthOpts`, `SynthGeometry`, `FileReplay`, the Python harness
- Produces: binary `namtor-synth --out <dir> --pairs <n> --seed <n> --geometry face|flat` writing a synthetic session; a CI job proving the full chain runs with no camera.

- [ ] **Step 1: Write the failing test**

Create `ml/eval/tests/test_end_to_end.py`:

```python
# SPDX-License-Identifier: GPL-3.0-only
"""Proves the whole chain runs with no camera attached.

Generates a synthetic session with the Rust binary, confirms it replays, then
drives the Python harness end to end. This is the guard that keeps the project
CI-testable as it grows.
"""

import json
import pathlib
import shutil
import subprocess

import pytest

from namtor_eval.latency import latency_stats
from namtor_eval.metrics import far_frr_sweep, point_at_far
from namtor_eval.pad import pad_rates
from namtor_eval.records import load_jsonl
from namtor_eval.report import render_report

# The test runs from ml/eval, so a bare "target/debug/..." would resolve to
# ml/eval/target/ and never be found. Walk up to the workspace root instead.
REPO_ROOT = pathlib.Path(__file__).resolve().parents[3]


def _synth_binary() -> str | None:
    on_path = shutil.which("namtor-synth")
    if on_path:
        return on_path
    for profile in ("debug", "release"):
        candidate = REPO_ROOT / "target" / profile / "namtor-synth"
        if candidate.is_file():
            return str(candidate)
    return None


@pytest.mark.skipif(
    _synth_binary() is None,
    reason="namtor-synth not built; run `cargo build --bin namtor-synth`",
)
def test_synth_binary_produces_a_replayable_session(tmp_path):
    out = tmp_path / "session"
    subprocess.run(
        [_synth_binary(), "--out", str(out), "--pairs", "3",
         "--seed", "42", "--geometry", "face"],
        check=True,
    )

    manifest = json.loads((out / "manifest.json").read_text())
    assert manifest["version"] == 1
    assert len(manifest["frames"]) == 6
    assert manifest["frames"][0]["emitter"] == "On"
    assert manifest["frames"][1]["emitter"] == "Off"


def test_harness_produces_a_report_from_score_records(tmp_path):
    lat = {"capture_ms": 66.0, "detect_ms": 5.0, "embed_ms": 20.0,
           "pad_ms": 3.0, "total_ms": 94.0}
    rows = []
    for i in range(100):
        rows.append({"probe_id": f"p{i}", "gallery_id": f"p{i}", "genuine": True,
                     "score": 0.85, "pad_score": 0.95, "attack_type": None,
                     "latency": lat})
    for i in range(100):
        rows.append({"probe_id": f"p{i}", "gallery_id": f"q{i}", "genuine": False,
                     "score": 0.15, "pad_score": 0.95, "attack_type": None,
                     "latency": lat})
    for i in range(20):
        rows.append({"probe_id": f"a{i}", "gallery_id": "p0", "genuine": False,
                     "score": 0.80, "pad_score": 0.10,
                     "attack_type": "screen_replay_phone", "latency": lat})

    path = tmp_path / "scores.jsonl"
    path.write_text("\n".join(json.dumps(r) for r in rows) + "\n")

    records = load_jsonl(path)
    point = point_at_far(far_frr_sweep(records), target_far=0.0)
    md = render_report(
        title="smoke",
        operating_point=point,
        pad_report=pad_rates(records, threshold=0.5),
        latency=latency_stats(records),
        config_note="synthetic smoke test",
    )

    assert "## Accuracy" in md
    assert "screen_replay_phone" in md
    assert "n = 100" in md
    # The replay attack is fully rejected at pad threshold 0.5.
    assert "APCER `screen_replay_phone` | 0.000%" in md
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd ml/eval && python -m pytest tests/test_end_to_end.py -v`
Expected: FAIL — first test errors because `target/debug/namtor-synth` does not exist.

- [ ] **Step 3: Write the synth binary**

Add to `crates/namtor-capture/Cargo.toml`:

```toml
[[bin]]
name = "namtor-synth"
path = "src/bin/namtor-synth.rs"
```

Create `crates/namtor-capture/src/bin/namtor-synth.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-only
//! Generates a synthetic capture session for testing and CI.
//!
//! Synthetic sessions are labelled as such and are never included in
//! published error rates.

use namtor_capture::recording::SessionLabel;
use namtor_capture::synth::{synth_session, SynthGeometry, SynthOpts};
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ! {
    eprintln!(
        "usage: namtor-synth --out <dir> [--pairs N] [--seed N] [--geometry face|flat]"
    );
    std::process::exit(2);
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut out: Option<PathBuf> = None;
    let mut pairs: u32 = 5;
    let mut seed: u64 = 0;
    let mut geometry = SynthGeometry::Face;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                out = Some(PathBuf::from(args.get(i).unwrap_or_else(|| usage())));
            }
            "--pairs" => {
                i += 1;
                pairs = args.get(i).unwrap_or_else(|| usage()).parse().unwrap_or_else(|_| usage());
            }
            "--seed" => {
                i += 1;
                seed = args.get(i).unwrap_or_else(|| usage()).parse().unwrap_or_else(|_| usage());
            }
            "--geometry" => {
                i += 1;
                geometry = match args.get(i).map(String::as_str) {
                    Some("face") => SynthGeometry::Face,
                    Some("flat") => SynthGeometry::Flat,
                    _ => usage(),
                };
            }
            _ => usage(),
        }
        i += 1;
    }

    let Some(out) = out else { usage() };

    let opts = SynthOpts {
        pairs,
        seed,
        geometry,
        label: SessionLabel::Synthetic,
    };

    match synth_session(&out, &opts) {
        Ok(()) => {
            println!("wrote {} frames to {}", pairs * 2, out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo build && cd ml/eval && python -m pytest tests/test_end_to_end.py -v`
Expected: PASS — 2 tests. The first prints `wrote 6 frames to ...`.

- [ ] **Step 5: Wire the chain into CI**

Replace the `python` job in `.github/workflows/ci.yml`:

```yaml
  python:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: actions/setup-python@v5
        with:
          python-version: "3.11"
      - name: Build the synth binary the smoke test drives
        run: cargo build --bin namtor-synth
      - run: pip install -e 'ml/eval[dev]'   # quoted: brackets are glob chars
      - run: python -m pytest ml/eval/tests -v
```

- [ ] **Step 6: Run the full suite**

Run: `cargo test --all && cd ml/eval && python -m pytest tests -v`

Expected: everything passes. Approximate totals if you have followed every task — **27 Rust** (23 in `namtor-capture`, 4 in `namtor-eval`) and **37 Python**. Treat these as a sanity check, not a gate: a mismatch means you added or merged tests, not that something is broken. The gate is that nothing fails.

- [ ] **Step 7: Commit**

```bash
git add crates/namtor-capture/src/bin/namtor-synth.rs \
        crates/namtor-capture/Cargo.toml \
        ml/eval/tests/test_end_to_end.py \
        .github/workflows/ci.yml
git commit -m "feat: end-to-end smoke test, synth binary, CI wiring"
```

---

## M0 completion criteria

- [ ] `cargo test --all` passes
- [ ] `python -m pytest ml/eval/tests` passes
- [ ] `./scripts/check-spdx.sh` passes
- [ ] `cargo clippy --all-targets -- -D warnings` is clean
- [ ] CI green on a pushed branch
- [ ] `namtor-synth --out /tmp/s --pairs 5` produces a session that `FileReplay` reads back frame-identical

**What M0 unblocks:** M1 can record real capture sessions in a format the harness already reads. M3 can measure INT8 degradation against FP32 (retiring R4) the moment inference exists. Every later accuracy claim has a measuring stick that predates it.
