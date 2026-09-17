// SPDX-License-Identifier: GPL-3.0-only
//! Accessibility helpers for the Nam-tor enrollment UI.
//!
//! Currently houses [`ProgressAnnouncer`], the rate-limiter that prevents
//! screen-reader announcement flooding during live capture.

use std::time::{Duration, Instant};

/// Minimum interval between two screen-reader progress announcements.
/// Matches the daemon's progress throttle (`PROGRESS_INTERVAL_MS = 250`),
/// so the UI never spams AT-SPI with more than 4 announcements per second.
pub const PROGRESS_ANNOUNCE_INTERVAL: Duration = Duration::from_millis(250);

/// Rate-limiter for screen-reader announcements of capture progress.
///
/// GTK's `AccessibleRole::Status` triggers an AT-SPI announcement every
/// time the label text changes. During live capture the daemon emits
/// progress signals many times per second; without throttling the screen
/// reader would flood. This gate keeps at most one announcement per
/// [`PROGRESS_ANNOUNCE_INTERVAL`].
pub struct ProgressAnnouncer {
    last: Option<Instant>,
    interval: Duration,
    /// The text last announced, so an unchanged headline is not repeated.
    last_text: Option<String>,
}

impl ProgressAnnouncer {
    /// Create a throttled announcer with the default 250 ms interval.
    pub fn new() -> Self {
        Self::with_interval(PROGRESS_ANNOUNCE_INTERVAL)
    }

    /// Create a throttled announcer with a custom interval (for tests).
    pub fn with_interval(interval: Duration) -> Self {
        Self {
            last: None,
            interval,
            last_text: None,
        }
    }

    /// Returns `true` if `text` should be announced at `now`.
    ///
    /// Suppresses a repeat of the text that was last announced, whatever the
    /// interval. The time-based gate alone could not do the job here: the
    /// capture page re-renders on a 500 ms poll and this interval is 250 ms,
    /// so every render was "due" and nothing was ever throttled. What actually
    /// floods a screen reader is re-announcing the *same* guidance on every
    /// redraw, which is what this stops.
    pub fn should_announce_text(&mut self, text: &str, now: Instant) -> bool {
        if self.last_text.as_deref() == Some(text) {
            return false;
        }
        if !self.should_announce(now) {
            return false;
        }
        self.last_text = Some(text.to_string());
        true
    }

    /// Returns `true` if an announcement should fire at `now`, and records
    /// the timestamp. Returns `false` (and changes nothing) when the last
    /// announcement was less than `interval` ago.
    pub fn should_announce(&mut self, now: Instant) -> bool {
        let fire = match self.last {
            None => true,
            Some(last) => now.duration_since(last) >= self.interval,
        };
        if fire {
            self.last = Some(now);
        }
        fire
    }

    /// The cap on announcements per second implied by `interval`. Exposed
    /// for the rate-limit test.
    pub fn max_per_second(&self) -> u32 {
        if self.interval.is_zero() {
            return u32::MAX;
        }
        (Duration::from_secs(1).as_nanos() / self.interval.as_nanos()) as u32
    }
}

impl Default for ProgressAnnouncer {
    fn default() -> Self {
        Self::new()
    }
}
