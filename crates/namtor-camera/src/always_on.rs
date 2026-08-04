// SPDX-License-Identifier: GPL-3.0-only
//! Always-on emitter detection and reduced-protection surfacing.
//!
//! Spec §4.10.5: a camera whose emitter is always on (no control wire) cannot
//! do strobe-differential liveness. The daemon detects this by observing that
//! frames stay bright and steady with the emitter control *off*, and surfaces
//! a reduced protection level to the user via `namtor-cli status`.

use namtor_capture::frame::Frame;
use namtor_uvc::emitter::luma_stats;
use namtor_uvc::mode::measure_alternation;

/// Mean luminance above which a frame is considered "bright".
const BRIGHT_THRESHOLD: f64 = 5.0;

/// `true` when the emitter appears to be always on: frames are bright with
/// the emitter control off AND show no firmware alternation.
pub fn detect_always_on(idle_baseline_frames: &[Frame], with_emitter_off_frames: &[Frame]) -> bool {
    if with_emitter_off_frames.is_empty() {
        return false;
    }

    let mean = with_emitter_off_frames
        .iter()
        .map(|f| luma_stats(f).mean)
        .sum::<f64>()
        / with_emitter_off_frames.len() as f64;

    if mean < BRIGHT_THRESHOLD {
        return false;
    }

    let ev = measure_alternation(with_emitter_off_frames);
    if ev.alternates {
        return false;
    }

    let baseline_ev = measure_alternation(idle_baseline_frames);
    !baseline_ev.alternates
}

/// Protection level surfaced to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectionLevel {
    Full,
    Reduced,
}

impl ProtectionLevel {
    /// Returns `Reduced` when the emitter method is `AlwaysOn`, else `Full`.
    pub fn from_method(method: namtor_hwdb::quirks::EmitterMethod) -> Self {
        match method {
            namtor_hwdb::quirks::EmitterMethod::AlwaysOn => ProtectionLevel::Reduced,
            _ => ProtectionLevel::Full,
        }
    }
}

impl std::fmt::Display for ProtectionLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtectionLevel::Full => f.write_str("full"),
            ProtectionLevel::Reduced => f.write_str("reduced"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_capture::frame::{EmitterState, Frame, PixFmt};
    use std::time::Duration;

    fn frame(level: u8) -> Frame {
        Frame {
            data: vec![level; 64 * 64],
            format: PixFmt::Grey8,
            width: 64,
            height: 64,
            ts_mono: Duration::ZERO,
            sof: None,
            emitter: EmitterState::Unknown,
            quality: None,
        }
    }

    fn alternating_frames(bright: u8, dark: u8, n: usize) -> Vec<Frame> {
        (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    frame(bright)
                } else {
                    frame(dark)
                }
            })
            .collect()
    }

    #[test]
    fn bright_steady_frames_with_emitter_off_are_always_on() {
        // (a): bright, no alternation, emitter control off -> always-on.
        let baseline = vec![frame(120); 6];
        let off = vec![frame(120); 6];
        assert!(detect_always_on(&baseline, &off));
    }

    #[test]
    fn alternating_frames_are_not_always_on() {
        // (b): alternation means the emitter is controllable, not always-on.
        let baseline = alternating_frames(200, 0, 8);
        let off = alternating_frames(200, 0, 8);
        assert!(!detect_always_on(&baseline, &off));
    }

    #[test]
    fn dark_frames_are_not_always_on() {
        // (c): dark with emitter off means no emitter at all, not always-on.
        let baseline = vec![frame(2); 6];
        let off = vec![frame(2); 6];
        assert!(!detect_always_on(&baseline, &off));
    }

    #[test]
    fn empty_off_frames_are_not_always_on() {
        let baseline = vec![frame(120); 6];
        assert!(!detect_always_on(&baseline, &[]));
    }

    #[test]
    fn protection_reduced_for_always_on_method() {
        assert_eq!(
            ProtectionLevel::from_method(namtor_hwdb::quirks::EmitterMethod::AlwaysOn),
            ProtectionLevel::Reduced
        );
    }

    #[test]
    fn protection_full_for_controllable_methods() {
        for m in [
            namtor_hwdb::quirks::EmitterMethod::MsxuMode,
            namtor_hwdb::quirks::EmitterMethod::Msxu,
            namtor_hwdb::quirks::EmitterMethod::VendorXu,
            namtor_hwdb::quirks::EmitterMethod::FirmwareStrobed,
            namtor_hwdb::quirks::EmitterMethod::Unknown,
        ] {
            assert_eq!(
                ProtectionLevel::from_method(m),
                ProtectionLevel::Full,
                "{m:?} must be Full"
            );
        }
    }
}
