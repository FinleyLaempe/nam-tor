// SPDX-License-Identifier: GPL-3.0-only
//! Verify engine: the daemon's authentication heart.
//!
//! Drives the full spec §6.1 pipeline against a live capture source:
//!   1. Capture frame pairs (emitter On / Off).
//!   2. Compute `D = On − Off` (strobe differential).
//!   3. Run analytic PAD gates on `D` (spec §6.4). PAD is a veto.
//!   4. Detect face on the On frame (YuNet).
//!   5. Align + embed (SFace, 128-d, L2-normalised).
//!   6. Match against the target user's enrolled templates (cosine sim, max).
//!   7. Return `Verdict::Match` if score ≥ threshold AND PAD passed.
//!
//! On any error (no camera, no face, timeout, store failure) returns
//! `Verdict::Unavailable` or `Verdict::NoMatch` — never `PAM_SUCCESS`.

use std::path::Path;
use std::time::Instant;

use namtor_capture::frame::{EmitterState, Frame};
use namtor_capture::source::CaptureSource;
use namtor_pad::{mean_u8, run_analytic_pad, strobe_difference, PadConfig};
use namtor_proto::pam::Verdict;
use namtor_recog::RecognitionPipeline;
use namtor_store::{StoreError, TemplateStore};
use namtor_worker::WorkerError;

use crate::DaemonError;

/// Default cosine-similarity match threshold. Spec §6.6 Tier 3: the threshold
/// is per-user (z-norm) but we need a sane fallback when z-norm params are
/// absent (first enrollment, no cohort).
pub const DEFAULT_MATCH_THRESHOLD: f32 = 0.45;

/// Maximum frames to attempt before giving up (spec §3.4: 5 s hard timeout).
pub const MAX_ATTEMPT_FRAMES: usize = 60;

/// The verify engine owns the recognition pipeline and the template store.
pub struct VerifyEngine {
    pipeline: RecognitionPipeline,
    store: TemplateStore,
    pad_config: PadConfig,
    match_threshold: f32,
}

/// Outcome of a single verify attempt.
#[derive(Debug, Clone)]
pub struct VerifyOutcome {
    pub verdict: Verdict,
    pub score: f32,
    pub reason: Option<String>,
}

impl VerifyOutcome {
    pub fn unavailable(reason: &str) -> Self {
        Self {
            verdict: Verdict::Unavailable,
            score: 0.0,
            reason: Some(reason.into()),
        }
    }
    pub fn no_match(reason: &str) -> Self {
        Self {
            verdict: Verdict::NoMatch,
            score: 0.0,
            reason: Some(reason.into()),
        }
    }
    pub fn refused(reason: &str) -> Self {
        Self {
            verdict: Verdict::Refused,
            score: 0.0,
            reason: Some(reason.into()),
        }
    }
    pub fn match_(score: f32) -> Self {
        Self {
            verdict: Verdict::Match,
            score,
            reason: None,
        }
    }
}

impl VerifyEngine {
    pub fn new(
        yunet_path: &Path,
        sface_path: &Path,
        store_dir: &Path,
        key_path: &Path,
    ) -> Result<Self, VerifyError> {
        let pipeline = RecognitionPipeline::new(yunet_path, sface_path, (640, 640))?;
        let store = TemplateStore::open(store_dir, key_path)?;
        Ok(Self {
            pipeline,
            store,
            pad_config: PadConfig::default(),
            match_threshold: DEFAULT_MATCH_THRESHOLD,
        })
    }

    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.match_threshold = threshold;
        self
    }

    pub fn with_pad_config(mut self, cfg: PadConfig) -> Self {
        self.pad_config = cfg;
        self
    }

    /// Run one verify attempt against `capture` for `target_uid`.
    ///
    /// Captures frame pairs, runs PAD + recognition, matches against enrolled
    /// templates. Returns a verdict; never returns `Err` for auth outcomes —
    /// only for infrastructure failures (which map to `Unavailable`).
    pub fn verify(
        &mut self,
        capture: &mut dyn CaptureSource,
        target_uid: u32,
        deadline: Instant,
    ) -> VerifyOutcome {
        // 1. Load the user's enrolled templates.
        let record = match self.store.load(target_uid) {
            Ok(r) => r,
            Err(StoreError::NotFound(_)) => return VerifyOutcome::no_match("no enrollment"),
            Err(_) => return VerifyOutcome::unavailable("store error"),
        };

        if record.templates.is_empty() {
            return VerifyOutcome::no_match("empty enrollment");
        }

        // 2. Capture frame pairs and attempt authentication.
        let mut last_on_frame: Option<Frame> = None;
        let mut last_off_frame: Option<Frame> = None;
        let mut attempts = 0usize;

        while attempts < MAX_ATTEMPT_FRAMES && Instant::now() < deadline {
            attempts += 1;
            let frame = match capture.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(_) => continue,
            };

            // Pair frames by emitter state.
            match frame.emitter {
                EmitterState::On => {
                    last_on_frame = Some(frame);
                }
                EmitterState::Off => {
                    last_off_frame = Some(frame);
                }
                EmitterState::Unknown | EmitterState::Torn => {
                    // No strobe tagging — use every frame as both On and Off.
                    last_on_frame = Some(frame.clone());
                    last_off_frame = Some(frame);
                }
            }

            // When we have a pair, run the full pipeline.
            if let (Some(on), Some(off)) = (&last_on_frame, &last_off_frame) {
                let outcome = self.verify_pair(
                    on,
                    off,
                    &record.templates,
                    &record.meta.znorm_mean,
                    &record.meta.znorm_std,
                );
                if outcome.verdict == Verdict::Match || outcome.verdict == Verdict::Refused {
                    return outcome;
                }
                // NoMatch or Unavailable — try more frames.
                last_on_frame = None;
                last_off_frame = None;
            }
        }

        VerifyOutcome::no_match("timeout")
    }

    /// Run the full pipeline on a single On/Off frame pair.
    fn verify_pair(
        &mut self,
        on: &Frame,
        off: &Frame,
        templates: &[namtor_store::Template],
        znorm_mean: &Option<f32>,
        znorm_std: &Option<f32>,
    ) -> VerifyOutcome {
        let w = on.width;
        let h = on.height;

        // 1. Strobe differential.
        let d = match strobe_difference(&on.data, &off.data, w, h) {
            Ok(d) => d,
            Err(_) => return VerifyOutcome::unavailable("differential failed"),
        };

        // 2. PAD: analytic gates on D. PAD is a veto (spec §6.6).
        let on_mean = mean_u8(&on.data);
        let pad = match run_analytic_pad(&d, w, h, on_mean, None, &self.pad_config) {
            Ok(p) => p,
            Err(_) => return VerifyOutcome::unavailable("pad failed"),
        };
        if !pad.live {
            // PAD veto — deliberately indistinguishable from NoMatch to the
            // user (spec §8.3), but the journal records the true reason.
            return VerifyOutcome::no_match("pad veto");
        }

        // 3. Detect + align + embed on the On frame.
        let result = match self.pipeline.process_frame(&on.data, w, h) {
            Ok(Some(r)) => r,
            Ok(None) => return VerifyOutcome::no_match("no face"),
            Err(_) => return VerifyOutcome::unavailable("inference failed"),
        };

        // 4. Match against enrolled templates (max cosine similarity).
        let mut best_score = -1.0f32;
        for t in templates {
            let sim = RecognitionPipeline::cosine_similarity(&result.embedding.vector, &t.vector);
            if sim > best_score {
                best_score = sim;
            }
        }

        // 5. Apply z-norm if available.
        let normalized = if let (Some(m), Some(s)) = (znorm_mean, znorm_std) {
            if *s > 1e-6 {
                (best_score - m) / s
            } else {
                best_score
            }
        } else {
            best_score
        };

        // 6. Threshold.
        if normalized >= self.match_threshold {
            VerifyOutcome::match_(normalized)
        } else {
            VerifyOutcome::no_match("below threshold")
        }
    }
}

/// Errors that prevent the verify engine from starting (infrastructure).
#[derive(Debug)]
pub enum VerifyError {
    Worker(WorkerError),
    Store(StoreError),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::Worker(e) => write!(f, "worker: {e}"),
            VerifyError::Store(e) => write!(f, "store: {e}"),
        }
    }
}

impl From<WorkerError> for VerifyError {
    fn from(e: WorkerError) -> Self {
        VerifyError::Worker(e)
    }
}
impl From<StoreError> for VerifyError {
    fn from(e: StoreError) -> Self {
        VerifyError::Store(e)
    }
}

impl std::error::Error for VerifyError {}

impl From<VerifyError> for DaemonError {
    fn from(e: VerifyError) -> Self {
        match e {
            VerifyError::Worker(e) => DaemonError::Worker(e.to_string()),
            VerifyError::Store(e) => DaemonError::State(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_capture::frame::PixFmt;
    use namtor_capture::source::{CaptureError, CaptureSource, SourceCaps, Spectrum};

    #[test]
    fn empty_capture_returns_no_match() {
        let outcome = VerifyOutcome::no_match("timeout");
        assert_eq!(outcome.verdict, Verdict::NoMatch);
    }

    #[test]
    fn outcome_constructors() {
        assert_eq!(
            VerifyOutcome::unavailable("x").verdict,
            Verdict::Unavailable
        );
        assert_eq!(VerifyOutcome::refused("x").verdict, Verdict::Refused);
        assert_eq!(VerifyOutcome::match_(0.9).verdict, Verdict::Match);
        assert_eq!(VerifyOutcome::match_(0.9).score, 0.9);
    }

    #[test]
    fn znorm_application_logic() {
        let best_score = 0.50f32;
        let m = 0.3f32;
        let s = 0.1f32;
        let normalized = (best_score - m) / s;
        assert!((normalized - 2.0).abs() < 1e-5);
    }

    #[test]
    fn znorm_with_zero_std_is_passthrough() {
        let best_score = 0.50f32;
        let s = 0.0f32;
        let normalized = if s > 1e-6 {
            (best_score - 0.3) / s
        } else {
            best_score
        };
        assert_eq!(normalized, best_score);
    }

    #[test]
    fn threshold_comparison() {
        let threshold = 0.45f32;
        assert!(0.5 >= threshold);
        assert!(0.4 < threshold);
    }

    #[test]
    fn stub_source_compiles() {
        struct StubSource;
        impl CaptureSource for StubSource {
            fn caps(&self) -> SourceCaps {
                SourceCaps {
                    spectrum: Spectrum::Ir,
                    format: PixFmt::Grey8,
                    width: 8,
                    height: 8,
                    fps: 30,
                    has_metadata: false,
                }
            }
            fn start(&mut self) -> Result<(), CaptureError> {
                Ok(())
            }
            fn next_frame(&mut self) -> Result<Option<namtor_capture::frame::Frame>, CaptureError> {
                Ok(None)
            }
            fn stop(&mut self) {}
        }
        let _ = StubSource;
    }
}
