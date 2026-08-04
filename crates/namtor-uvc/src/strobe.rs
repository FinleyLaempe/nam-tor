// SPDX-License-Identifier: Apache-2.0
//! Strobe sequencing and torn-frame rejection.
//!
//! Extension unit writes are USB control transfers with ~1–5 ms latency and
//! no frame synchronisation. At 30 fps a toggle can land mid-exposure,
//! producing a partially-lit frame. Such frames are discarded, never
//! averaged into a differential.

use namtor_capture::frame::EmitterState;

/// A recorded emitter state change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Toggle {
    /// Monotonic nanoseconds, on the same clock as frame metadata.
    pub at_ns: u64,
    pub to_on: bool,
}

/// The exposure interval a frame covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameWindow {
    pub start_ns: u64,
    pub end_ns: u64,
}

/// Determines a frame's emitter state, or [`EmitterState::Torn`] when the
/// emitter changed during the exposure.
///
/// Boundary toggles are attributed to the *following* frame: a toggle at
/// exactly `end_ns` did not affect this exposure.
pub fn classify(window: FrameWindow, toggles: &[Toggle], initial_on: bool) -> EmitterState {
    let torn = toggles
        .iter()
        .any(|t| t.at_ns > window.start_ns && t.at_ns < window.end_ns);
    if torn {
        return EmitterState::Torn;
    }

    let state = toggles
        .iter()
        .filter(|t| t.at_ns <= window.start_ns)
        .max_by_key(|t| t.at_ns)
        .map(|t| t.to_on)
        .unwrap_or(initial_on);

    if state {
        EmitterState::On
    } else {
        EmitterState::Off
    }
}

/// Turns a stream of frame-arrival timestamps into exposure windows.
pub struct StrobeSequencer {
    nominal_duration_ns: u64,
    previous_end_ns: Option<u64>,
}

impl StrobeSequencer {
    /// `nominal_duration_ns` is `1e9 / fps`, used for the first frame and as
    /// a fallback when timestamps are not monotonic.
    pub fn new(nominal_duration_ns: u64) -> Self {
        Self {
            nominal_duration_ns,
            previous_end_ns: None,
        }
    }

    /// Records a frame arrival and returns its exposure window.
    pub fn push(&mut self, end_ns: u64) -> FrameWindow {
        let start_ns = match self.previous_end_ns {
            Some(prev) if prev <= end_ns => prev,
            _ => end_ns.saturating_sub(self.nominal_duration_ns),
        };
        self.previous_end_ns = Some(end_ns);
        FrameWindow { start_ns, end_ns }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_capture::frame::EmitterState;

    const MS: u64 = 1_000_000;

    fn window(start_ms: u64, end_ms: u64) -> FrameWindow {
        FrameWindow {
            start_ns: start_ms * MS,
            end_ns: end_ms * MS,
        }
    }

    #[test]
    fn frame_entirely_after_an_on_toggle_is_on() {
        let toggles = [Toggle {
            at_ns: 10 * MS,
            to_on: true,
        }];
        assert_eq!(classify(window(20, 53), &toggles, false), EmitterState::On);
    }

    #[test]
    fn frame_entirely_before_any_toggle_keeps_the_initial_state() {
        let toggles = [Toggle {
            at_ns: 100 * MS,
            to_on: true,
        }];
        assert_eq!(classify(window(20, 53), &toggles, false), EmitterState::Off);
    }

    #[test]
    fn toggle_inside_the_exposure_makes_the_frame_torn() {
        // The whole point of this module.
        let toggles = [Toggle {
            at_ns: 35 * MS,
            to_on: true,
        }];
        assert_eq!(
            classify(window(20, 53), &toggles, false),
            EmitterState::Torn
        );
    }

    #[test]
    fn toggle_exactly_at_the_boundary_is_not_torn() {
        // A toggle at the instant the frame ends affects the next frame.
        let toggles = [Toggle {
            at_ns: 53 * MS,
            to_on: true,
        }];
        assert_eq!(classify(window(20, 53), &toggles, false), EmitterState::Off);
    }

    #[test]
    fn multiple_toggles_inside_one_exposure_are_still_torn() {
        let toggles = [
            Toggle {
                at_ns: 30 * MS,
                to_on: true,
            },
            Toggle {
                at_ns: 40 * MS,
                to_on: false,
            },
        ];
        assert_eq!(
            classify(window(20, 53), &toggles, false),
            EmitterState::Torn
        );
    }

    #[test]
    fn sequencer_derives_windows_from_consecutive_timestamps() {
        let mut seq = StrobeSequencer::new(33_333_333);

        // First frame has no predecessor: the nominal duration is assumed.
        let w0 = seq.push(100 * MS);
        assert_eq!(w0.end_ns, 100 * MS);
        assert_eq!(w0.start_ns, 100 * MS - 33_333_333);

        // Second frame's window starts where the previous one ended.
        let w1 = seq.push(133 * MS);
        assert_eq!(w1.start_ns, 100 * MS);
        assert_eq!(w1.end_ns, 133 * MS);
    }

    #[test]
    fn sequencer_falls_back_to_nominal_duration_on_a_backwards_timestamp() {
        // Timestamps must be monotonic; a device that violates this must not
        // produce a window with start > end.
        let mut seq = StrobeSequencer::new(33_333_333);
        seq.push(200 * MS);
        let w = seq.push(100 * MS);
        assert!(w.start_ns <= w.end_ns);
    }
}
