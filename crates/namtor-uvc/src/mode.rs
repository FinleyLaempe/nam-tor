// SPDX-License-Identifier: Apache-2.0
//! Device-driven illumination: asking the camera to alternate the emitter
//! itself, which is what Windows Hello does.

use crate::descriptors::ExtensionUnit;
use crate::emitter::luma_stats;
use crate::probe::{SafeXu, XuTransport};
use crate::xu::Query;
use namtor_capture::frame::Frame;

/// A control whose payload contains a bounded field — a mode selector.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeControl {
    pub unit: u8,
    pub selector: u8,
    pub len: usize,
    pub current: Vec<u8>,
    pub max: Vec<u8>,
    pub default: Vec<u8>,
}

impl ModeControl {
    /// Index of the byte that varies between CUR and MAX.
    ///
    /// Bytes equal in both are fixed — version numbers, capability masks —
    /// and must be preserved. Hardcoding an offset would break on any other
    /// vendor's payload layout.
    pub fn mode_byte(&self) -> Option<usize> {
        self.current
            .iter()
            .zip(self.max.iter())
            .position(|(c, m)| c != m)
    }

    /// Payloads to try: every value of the mode field from its minimum
    /// through its maximum, excluding the current value, with all other
    /// bytes preserved exactly.
    pub fn mode_candidates(&self) -> Vec<Vec<u8>> {
        let Some(idx) = self.mode_byte() else {
            // No varying byte. A boolean whose CUR equals MAX still has a
            // distinct DEF worth trying.
            return if self.default != self.current {
                vec![self.default.clone()]
            } else {
                Vec::new()
            };
        };

        let hi = self.max[idx];
        let mut out = Vec::new();
        for v in 0..=hi {
            if v == self.current[idx] {
                continue;
            }
            let mut p = self.current.clone();
            p[idx] = v;
            out.push(p);
        }
        out
    }
}

/// Reads CUR, MAX and DEF for the selectors a unit advertises, keeping those
/// that look like mode controls.
pub fn find_mode_controls<T: XuTransport>(
    xu: &mut SafeXu<T>,
    units: &[ExtensionUnit],
) -> Vec<ModeControl> {
    let mut out = Vec::new();

    for unit in units {
        for selector in unit.advertised_selectors() {
            let Ok(profile) = xu.probe_control(unit.unit_id, selector) else {
                continue;
            };
            if !profile.info.supports_set() || profile.len == 0 {
                continue;
            }

            let mut max = vec![0u8; profile.len];
            let mut default = vec![0u8; profile.len];
            if xu
                .transport_query(unit.unit_id, selector, Query::GetMax, &mut max)
                .is_err()
            {
                continue;
            }
            let _ = xu.transport_query(unit.unit_id, selector, Query::GetDef, &mut default);

            out.push(ModeControl {
                unit: unit.unit_id,
                selector,
                len: profile.len,
                current: profile.current,
                max,
                default,
            });
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlternationEvidence {
    /// Mean luminance difference between the bright and dark phases.
    pub alternation: f64,
    pub on_mean: f64,
    pub off_mean: f64,
    /// Whether the burst shows genuine frame-to-frame alternation.
    pub alternates: bool,
}

/// Minimum luminance separation between phases to call it alternation.
const ALTERNATION_THRESHOLD: f64 = 20.0;

/// Detects firmware-driven strobing in a captured burst.
///
/// **Classification is by frame parity, not by a global brightness
/// threshold.** Measured reference data shows illuminated frames decaying
/// 217.6 → 45.3 under auto-exposure while clean frames hold at 0.0. A global
/// threshold derived from the burst mean nearly misclassifies the settled
/// illuminated frames, and would fail outright if AE settled lower.
///
/// The shape check comes first: strict 1:1 alternation means consecutive
/// deltas flip sign at nearly every step. A monotonic ramp — AE hunting with
/// no strobe — never does, and must not be reported as alternation.
pub fn measure_alternation(frames: &[Frame]) -> AlternationEvidence {
    let none = AlternationEvidence {
        alternation: 0.0,
        on_mean: 0.0,
        off_mean: 0.0,
        alternates: false,
    };
    if frames.len() < 4 {
        return none;
    }

    let means: Vec<f64> = frames.iter().map(|f| luma_stats(f).mean).collect();

    // Strict alternation flips the sign of the delta at (almost) every step.
    let signs: Vec<i8> = means
        .windows(2)
        .map(|w| (w[1] - w[0]).signum() as i8)
        .collect();
    let flips = signs
        .windows(2)
        .filter(|s| s[0] != s[1] && s[0] != 0)
        .count();
    // Allow two non-flips for noise and for a settled tail.
    let alternating_shape = flips + 2 >= signs.len().saturating_sub(1);

    // Split by parity, then decide which parity is the illuminated one.
    // Robust to the illuminated phase decaying under auto-exposure.
    let (even, odd) = means
        .iter()
        .enumerate()
        .partition::<Vec<_>, _>(|(i, _)| i % 2 == 0);
    let even: Vec<f64> = even.into_iter().map(|(_, v)| *v).collect();
    let odd: Vec<f64> = odd.into_iter().map(|(_, v)| *v).collect();

    if even.is_empty() || odd.is_empty() {
        return none;
    }
    let even_mean = even.iter().sum::<f64>() / even.len() as f64;
    let odd_mean = odd.iter().sum::<f64>() / odd.len() as f64;

    let (on_mean, off_mean) = if odd_mean >= even_mean {
        (odd_mean, even_mean)
    } else {
        (even_mean, odd_mean)
    };
    let alternation = on_mean - off_mean;

    AlternationEvidence {
        alternation,
        on_mean,
        off_mean,
        alternates: alternating_shape && alternation >= ALTERNATION_THRESHOLD,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_capture::frame::{EmitterState, Frame, PixFmt};
    use std::time::Duration;

    fn frames(levels: &[u8]) -> Vec<Frame> {
        levels
            .iter()
            .enumerate()
            .map(|(i, &l)| Frame {
                data: vec![l; 64 * 64],
                format: PixFmt::Grey8,
                width: 64,
                height: 64,
                ts_mono: Duration::from_millis(33 * i as u64),
                sof: Some(i as u16),
                emitter: EmitterState::Unknown,
                quality: None,
            })
            .collect()
    }

    #[test]
    fn alternating_frames_are_detected() {
        let ev = measure_alternation(&frames(&[200, 40, 200, 40, 200, 40]));
        assert!(ev.alternates, "clear alternation must be detected");
        assert!(ev.alternation > 100.0);
        assert!(ev.on_mean > ev.off_mean);
    }

    #[test]
    fn steady_frames_do_not_alternate() {
        let ev = measure_alternation(&frames(&[120, 120, 120, 120, 120, 120]));
        assert!(!ev.alternates);
        assert!(ev.alternation < 5.0);
    }

    #[test]
    fn a_slow_ramp_is_not_alternation() {
        // Auto-exposure hunting produces a monotonic drift, not alternation.
        // Misreading it as alternation would report Tier 0 support falsely.
        let ev = measure_alternation(&frames(&[40, 60, 80, 100, 120, 140]));
        assert!(!ev.alternates, "a monotonic ramp is drift, not strobing");
    }

    #[test]
    fn either_phase_may_come_first() {
        // The burst may start on either phase; both must classify correctly.
        for seq in [[40u8, 200, 40, 200], [200, 40, 200, 40]] {
            let ev = measure_alternation(&frames(&seq));
            assert!(ev.alternates, "sequence {seq:?} should alternate");
            assert!(ev.on_mean > ev.off_mean);
        }
    }

    #[test]
    fn decaying_illuminated_frames_still_classify_correctly() {
        // Real reference-camera data: auto-exposure decays the illuminated
        // phase 217 -> 45 while clean frames hold at 0. A global brightness
        // threshold derived from the burst mean nearly misclassifies the
        // settled tail and would fail outright if AE settled lower.
        let seq: Vec<u8> = vec![
            0, 217, 0, 216, 0, 115, 0, 115, 0, 69, 0, 69, 0, 54, 0, 53, 0, 50, 0, 49, 0, 46, 0, 46,
            0, 46, 0, 45, 0, 45,
        ];
        let ev = measure_alternation(&frames(&seq));

        assert!(ev.alternates, "strict alternation must survive AE decay");
        assert!(ev.off_mean < 1.0, "clean phase should stay near zero");
        assert!(
            ev.on_mean > 40.0,
            "illuminated phase should stay well above it"
        );
    }

    #[test]
    fn too_few_frames_is_inconclusive() {
        assert!(!measure_alternation(&frames(&[200])).alternates);
        assert!(!measure_alternation(&frames(&[])).alternates);
    }

    #[test]
    fn mode_candidates_walk_the_field_up_to_its_maximum() {
        // Selector 6 on the reference camera: byte 2 is the mode field,
        // current 1, max 3. Candidates must cover 2 and 3, and must preserve
        // every other byte of the payload.
        let ctrl = ModeControl {
            unit: 4,
            selector: 6,
            len: 9,
            current: vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
            max: vec![0x01, 0x03, 0x03, 0, 0, 0, 0, 0, 0],
            default: vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
        };

        let cands = ctrl.mode_candidates();

        assert!(cands.contains(&vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0]));
        assert!(cands.contains(&vec![0x01, 0x03, 0x03, 0, 0, 0, 0, 0, 0]));
        assert!(
            !cands.contains(&ctrl.current),
            "the current value produces no state change to observe"
        );
        for c in &cands {
            assert_eq!(c.len(), 9);
            assert_eq!(c[0], 0x01, "fixed bytes must be preserved");
            assert_eq!(c[1], 0x03, "capability mask must be preserved");
        }
    }

    #[test]
    fn the_varying_byte_is_located_by_comparing_current_with_max() {
        // Bytes equal in CUR and MAX are fixed; the differing byte is the
        // field with a range. Hardcoding an offset would break on any other
        // vendor's payload layout.
        let ctrl = ModeControl {
            unit: 4,
            selector: 6,
            len: 9,
            current: vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
            max: vec![0x01, 0x03, 0x03, 0, 0, 0, 0, 0, 0],
            default: vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
        };
        assert_eq!(ctrl.mode_byte(), Some(2));
    }

    #[test]
    fn a_boolean_control_yields_its_opposite_value() {
        // Selector 9 on the reference camera: CUR 1, MAX 1, DEF 0.
        let ctrl = ModeControl {
            unit: 4,
            selector: 9,
            len: 4,
            current: vec![0x01, 0, 0, 0],
            max: vec![0x01, 0, 0, 0],
            default: vec![0x00, 0, 0, 0],
        };
        assert!(ctrl.mode_candidates().contains(&vec![0x00, 0, 0, 0]));
    }
}
