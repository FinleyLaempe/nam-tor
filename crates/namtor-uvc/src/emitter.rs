// SPDX-License-Identifier: Apache-2.0
//! Automatic verification that an extension unit control drives the IR
//! emitter, by measuring the luminance change it causes.

use namtor_capture::frame::{Frame, PixFmt};

/// Fraction of the image half-width treated as "centre" for the radial test.
const CENTRE_FRACTION: f64 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LumaStats {
    pub mean: f64,
    pub centre_mean: f64,
    pub edge_mean: f64,
}

/// Mean luminance overall, in the centre region, and in the outer region.
///
/// Only meaningful for 8-bit greyscale; other formats yield zeroes.
pub fn luma_stats(frame: &Frame) -> LumaStats {
    if frame.format != PixFmt::Grey8 || frame.data.is_empty() {
        return LumaStats {
            mean: 0.0,
            centre_mean: 0.0,
            edge_mean: 0.0,
        };
    }

    let w = frame.width as usize;
    let h = frame.height as usize;
    let cx = w as f64 / 2.0;
    let cy = h as f64 / 2.0;
    let max_r = (cx * cx + cy * cy).sqrt();

    let (mut total, mut centre_sum, mut centre_n, mut edge_sum, mut edge_n) =
        (0.0f64, 0.0f64, 0usize, 0.0f64, 0usize);

    for y in 0..h {
        for x in 0..w {
            let Some(&v) = frame.data.get(y * w + x) else {
                continue;
            };
            let v = v as f64;
            total += v;

            let dx = x as f64 - cx;
            let dy = y as f64 - cy;
            let r = (dx * dx + dy * dy).sqrt() / max_r;

            if r < CENTRE_FRACTION {
                centre_sum += v;
                centre_n += 1;
            } else if r > 1.0 - CENTRE_FRACTION {
                edge_sum += v;
                edge_n += 1;
            }
        }
    }

    let n = (w * h) as f64;
    LumaStats {
        mean: total / n,
        centre_mean: if centre_n > 0 {
            centre_sum / centre_n as f64
        } else {
            0.0
        },
        edge_mean: if edge_n > 0 {
            edge_sum / edge_n as f64
        } else {
            0.0
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmitterVerifyCfg {
    /// Minimum mean luminance increase to accept, in 8-bit levels.
    pub min_delta: f64,
    /// Require the increase to be stronger at the centre than at the edges.
    pub require_centre_weighted: bool,
}

impl Default for EmitterVerifyCfg {
    fn default() -> Self {
        Self {
            min_delta: 10.0,
            require_centre_weighted: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmitterEvidence {
    /// Mean luminance increase from off-set to on-set.
    pub delta: f64,
    /// Whether the increase is stronger at the centre than at the edges,
    /// as a point emitter's inverse-square falloff implies.
    pub centre_weighted: bool,
    /// Whether the evidence satisfies the configured thresholds.
    pub confident: bool,
}

fn mean_of(frames: &[Frame], pick: fn(&LumaStats) -> f64) -> f64 {
    if frames.is_empty() {
        return 0.0;
    }
    frames.iter().map(|f| pick(&luma_stats(f))).sum::<f64>() / frames.len() as f64
}

/// Decides whether a control drove the IR emitter, from frames captured
/// before and after it was set.
pub fn verify_emitter(off: &[Frame], on: &[Frame], cfg: &EmitterVerifyCfg) -> EmitterEvidence {
    if off.is_empty() || on.is_empty() {
        return EmitterEvidence {
            delta: 0.0,
            centre_weighted: false,
            confident: false,
        };
    }

    let delta = mean_of(on, |s| s.mean) - mean_of(off, |s| s.mean);

    let centre_delta = mean_of(on, |s| s.centre_mean) - mean_of(off, |s| s.centre_mean);
    let edge_delta = mean_of(on, |s| s.edge_mean) - mean_of(off, |s| s.edge_mean);
    // Require a clear margin so sensor noise cannot flip the verdict.
    let centre_weighted = centre_delta > edge_delta + 5.0;

    let confident = delta >= cfg.min_delta && (!cfg.require_centre_weighted || centre_weighted);

    EmitterEvidence {
        delta,
        centre_weighted,
        confident,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_capture::frame::Frame;
    use namtor_capture::recording::SessionLabel;
    use namtor_capture::replay::FileReplay;
    use namtor_capture::source::CaptureSource;
    use namtor_capture::synth::{synth_session, SynthGeometry, SynthOpts};

    /// Renders a synthetic session and splits it into off/on frame vectors.
    fn synth_pairs(geometry: SynthGeometry, pairs: u32) -> (Vec<Frame>, Vec<Frame>) {
        let dir = tempfile::tempdir().unwrap();
        synth_session(
            dir.path(),
            &SynthOpts {
                pairs,
                seed: 5,
                geometry,
                label: SessionLabel::Synthetic,
            },
        )
        .unwrap();

        let mut r = FileReplay::open(dir.path()).unwrap();
        r.start().unwrap();

        let (mut on, mut off) = (Vec::new(), Vec::new());
        while let Some(f) = r.next_frame().unwrap() {
            match f.emitter {
                namtor_capture::frame::EmitterState::On => on.push(f),
                _ => off.push(f),
            }
        }
        (off, on)
    }

    #[test]
    fn detects_a_real_luminance_increase() {
        let (off, on) = synth_pairs(SynthGeometry::Face, 3);
        let ev = verify_emitter(&off, &on, &EmitterVerifyCfg::default());
        assert!(ev.delta > 10.0, "expected a clear delta, got {}", ev.delta);
        assert!(ev.confident);
    }

    #[test]
    fn identical_frame_sets_produce_no_evidence() {
        let (off, _) = synth_pairs(SynthGeometry::Face, 3);
        let ev = verify_emitter(&off, &off, &EmitterVerifyCfg::default());
        assert!(ev.delta.abs() < 2.0);
        assert!(!ev.confident, "must not claim an emitter with no delta");
    }

    #[test]
    fn face_geometry_is_recognised_as_centre_weighted() {
        // A point emitter illuminates the centre more than the edges. A
        // control that raises the whole image uniformly is more likely a gain
        // or exposure control than an emitter.
        let (off, on) = synth_pairs(SynthGeometry::Face, 2);
        let ev = verify_emitter(&off, &on, &EmitterVerifyCfg::default());
        assert!(ev.centre_weighted);
    }

    #[test]
    fn uniform_brightening_is_not_centre_weighted() {
        let (off, on) = synth_pairs(SynthGeometry::Flat, 2);
        let ev = verify_emitter(&off, &on, &EmitterVerifyCfg::default());
        assert!(!ev.centre_weighted);
    }

    #[test]
    fn centre_weighting_can_be_required_or_relaxed() {
        // Some emitters are diffuse enough to brighten uniformly at close
        // range, so the requirement is configurable rather than absolute.
        let (off, on) = synth_pairs(SynthGeometry::Flat, 2);

        let strict = verify_emitter(
            &off,
            &on,
            &EmitterVerifyCfg {
                min_delta: 10.0,
                require_centre_weighted: true,
            },
        );
        assert!(!strict.confident);

        let relaxed = verify_emitter(
            &off,
            &on,
            &EmitterVerifyCfg {
                min_delta: 10.0,
                require_centre_weighted: false,
            },
        );
        assert!(relaxed.confident);
    }

    #[test]
    fn empty_frame_sets_are_not_confident() {
        let ev = verify_emitter(&[], &[], &EmitterVerifyCfg::default());
        assert!(!ev.confident);
    }
}
