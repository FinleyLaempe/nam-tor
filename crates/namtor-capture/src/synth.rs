// SPDX-License-Identifier: GPL-3.0-only
//! Deterministic synthetic capture sessions.
//!
//! Gives CI frame data with no camera attached. Not a substitute for real
//! captures in evaluation — synthetic sessions are labelled
//! [`SessionLabel::Synthetic`] and are excluded from published error rates.

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

        assert!(
            face > 20.0,
            "face geometry must show radial falloff, got {face}"
        );
        assert!(
            flat.abs() < 5.0,
            "flat geometry must be uniform, got {flat}"
        );
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
