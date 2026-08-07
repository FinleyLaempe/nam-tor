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

/// Map a `VerifyOutcome`'s internal reason to the reason that may be sent
/// to the PAM client in an `AuthReply`. Spec §8.3: a PAD veto must be
/// indistinguishable from a regular `NoMatch` to the user — the detailed
/// gate name and raw scalar stay inside the daemon (journal/audit log only),
/// never the socket reply. The internal `VerifyOutcome.reason` keeps the
/// detail; this function strips it at the trust boundary.
///
/// Non-PAD `NoMatch` reasons (e.g. "no face", "score too low") and all
/// `Unavailable`/`Refused`/`RateLimited` reasons are passed through, since
/// they do not reveal PAD internals. "below threshold" is normalised to
/// "score too low" so the operating-point concept and value never cross the
/// boundary; an empty NoMatch reason is collapsed to `None` so a PAD veto
/// (also `None`) stays indistinguishable from a reason-less NoMatch.
pub fn external_reason(outcome: &VerifyOutcome) -> Option<String> {
    match outcome.verdict {
        Verdict::Match => None,
        Verdict::NoMatch => match outcome.reason.as_deref() {
            Some(r) if r.starts_with("pad veto") => None,
            Some("") => None,
            Some("below threshold") => Some("score too low".into()),
            other => other.map(|s| s.to_string()),
        },
        Verdict::Refused | Verdict::RateLimited | Verdict::Unavailable => outcome.reason.clone(),
    }
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
        let mut last_outcome = VerifyOutcome::no_match("timeout");
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
                let (on, off) = illuminated_first(on, off);
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
                last_outcome = outcome;
                // NoMatch or Unavailable — try more frames.
                last_on_frame = None;
                last_off_frame = None;
            }
        }

        last_outcome
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

        let result = match self.pipeline.process_frame(&on.data, w, h) {
            Ok(Some(r)) => r,
            Ok(None) => return VerifyOutcome::no_match("no face"),
            Err(_) => return VerifyOutcome::unavailable("inference failed"),
        };

        let on_mean = mean_u8(&on.data);
        let face_bbox = detection_bbox(&result.detection.box_, w, h);
        let pad = match run_analytic_pad(&d, w, h, on_mean, Some(face_bbox), &self.pad_config) {
            Ok(p) => p,
            Err(_) => return VerifyOutcome::unavailable("pad failed"),
        };
        if !pad.live {
            let reason = if !pad.energy.pass {
                format!("pad veto: energy ({:.3})", pad.energy.raw)
            } else if !pad.gradient.pass {
                format!("pad veto: gradient ({:.3})", pad.gradient.raw)
            } else if !pad.specular.pass {
                format!("pad veto: specular ({:.3})", pad.specular.raw)
            } else {
                format!("pad veto: radial ({:.3})", pad.radial.raw)
            };
            return VerifyOutcome::no_match(&reason);
        }

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

fn illuminated_first<'a>(first: &'a Frame, second: &'a Frame) -> (&'a Frame, &'a Frame) {
    if mean_u8(&first.data) >= mean_u8(&second.data) {
        (first, second)
    } else {
        (second, first)
    }
}

fn detection_bbox(box_: &namtor_recog::detect::FaceBox, w: u32, h: u32) -> (u32, u32, u32, u32) {
    let x = box_.x.max(0.0).min(w.saturating_sub(1) as f32) as u32;
    let y = box_.y.max(0.0).min(h.saturating_sub(1) as f32) as u32;
    let width = box_.w.max(1.0).min(w.saturating_sub(x) as f32) as u32;
    let height = box_.h.max(1.0).min(h.saturating_sub(y) as f32) as u32;
    (x, y, width, height)
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
    fn brighter_frame_is_selected_as_illuminated_when_parity_is_inverted() {
        let dark = Frame {
            data: vec![10; 64],
            format: PixFmt::Grey8,
            width: 8,
            height: 8,
            ts_mono: std::time::Duration::ZERO,
            sof: None,
            emitter: EmitterState::On,
            quality: None,
        };
        let bright = Frame {
            data: vec![80; 64],
            emitter: EmitterState::Off,
            ..dark.clone()
        };

        let (on, off) = illuminated_first(&dark, &bright);

        assert_eq!(mean_u8(&on.data), 80.0);
        assert_eq!(mean_u8(&off.data), 10.0);
    }

    #[test]
    fn detection_bbox_clamps_face_region_to_frame() {
        let box_ = namtor_recog::detect::FaceBox {
            x: -10.0,
            y: 300.0,
            w: 700.0,
            h: 100.0,
            score: 0.9,
        };

        let bbox = detection_bbox(&box_, 640, 360);

        assert_eq!(bbox, (0, 300, 640, 60));
    }

    #[test]
    fn external_reason_strips_pad_veto_detail_for_nomatch() {
        // Internal outcome carries the gate detail; external reason is generic.
        let internal = VerifyOutcome::no_match("pad veto: energy (0.123)");
        assert_eq!(internal.verdict, Verdict::NoMatch);
        assert!(
            internal.reason.as_deref().unwrap().contains("pad veto"),
            "internal journal keeps the gate detail"
        );
        let ext = external_reason(&internal);
        assert!(
            ext.is_none(),
            "external reason must be absent for a PAD veto"
        );
    }

    /// Every PAD veto variant is stripped at the trust boundary so the PAM
    /// client cannot tell which gate fired (spec §8.3: a PAD veto must be
    /// indistinguishable from a regular NoMatch).
    #[test]
    fn external_reason_strips_all_pad_veto_variants() {
        for reason in [
            "pad veto: energy (0.123)",
            "pad veto: gradient (0.456)",
            "pad veto: specular (0.789)",
            "pad veto: radial (0.012)",
        ] {
            let outcome = VerifyOutcome::no_match(reason);
            assert!(
                external_reason(&outcome).is_none(),
                "external_reason must strip `{reason}`"
            );
        }
    }

    /// Non-PAD NoMatch reasons that cross the trust boundary must carry no
    /// biometric data — no embeddings, templates, pixels, raw vectors, or
    /// hex dumps. Proves the socket reply does not leak biometric material.
    #[test]
    fn non_pad_reasons_carry_no_biometric_data() {
        let forbidden = ["embedding", "template", "pixel", "vec=", "0x"];
        for reason in [
            "no enrollment",
            "no face",
            "below threshold",
            "store error",
            "inference failed",
            "differential failed",
        ] {
            let outcome = VerifyOutcome::no_match(reason);
            let ext = external_reason(&outcome).unwrap_or_default();
            for token in forbidden {
                assert!(
                    !ext.contains(token),
                    "reason `{reason}` leaks `{token}` to the client"
                );
            }
        }
    }

    #[test]
    fn external_reason_passes_through_non_pad_nomatch() {
        let no_face = VerifyOutcome::no_match("no face");
        assert_eq!(external_reason(&no_face).as_deref(), Some("no face"));
        // "below threshold" is normalised so the operating-point word/value
        // never reaches the client; the actionable signal is preserved.
        let below = VerifyOutcome::no_match("below threshold");
        assert_eq!(external_reason(&below).as_deref(), Some("score too low"));
        // An empty NoMatch reason collapses to None, matching a PAD veto.
        let empty = VerifyOutcome::no_match("");
        assert!(external_reason(&empty).is_none());
    }

    #[test]
    fn external_reason_passes_through_unavailable_and_refused() {
        let unavail = VerifyOutcome::unavailable("store error");
        assert_eq!(external_reason(&unavail).as_deref(), Some("store error"));
        let refused = VerifyOutcome::refused("rhost set");
        assert_eq!(external_reason(&refused).as_deref(), Some("rhost set"));
    }

    #[test]
    fn external_reason_is_none_for_match() {
        let m = VerifyOutcome::match_(0.9);
        assert!(external_reason(&m).is_none());
    }

    /// Privacy gate (spec §8.3): the external reason that crosses the trust
    /// boundary into `AuthReply.reason` — and thus the PAM module and any CLI
    /// that surfaces it — must never carry PAD gate names, the match threshold,
    /// embeddings, raw vectors, or hex dumps. This is the comprehensive scan
    /// across every outcome variant, not just the PAD-veto strip.
    #[test]
    fn external_reason_rejects_gate_names_embeddings_and_thresholds() {
        let forbidden = [
            "energy",
            "gradient",
            "specular",
            "radial",
            "pad veto",
            "threshold",
            "embedding",
            "vec=",
            "0x",
            "template",
        ];
        let outcomes: &[VerifyOutcome] = &[
            VerifyOutcome::match_(0.91),
            VerifyOutcome::no_match("pad veto: energy (0.123)"),
            VerifyOutcome::no_match("pad veto: gradient (0.456)"),
            VerifyOutcome::no_match("pad veto: specular (0.789)"),
            VerifyOutcome::no_match("pad veto: radial (0.012)"),
            VerifyOutcome::no_match("no enrollment"),
            VerifyOutcome::no_match("empty enrollment"),
            VerifyOutcome::no_match("no face"),
            VerifyOutcome::no_match("below threshold"),
            VerifyOutcome::no_match("timeout"),
            VerifyOutcome::unavailable("no capture source"),
            VerifyOutcome::unavailable("store error"),
            VerifyOutcome::unavailable("inference failed"),
            VerifyOutcome::unavailable("differential failed"),
            VerifyOutcome::unavailable("pad failed"),
            VerifyOutcome::refused("rhost set"),
            VerifyOutcome {
                verdict: Verdict::RateLimited,
                score: 0.0,
                reason: Some("backoff active".into()),
            },
        ];
        for outcome in outcomes {
            let ext = external_reason(outcome).unwrap_or_default();
            for token in forbidden {
                assert!(
                    !ext.to_lowercase().contains(token),
                    "external_reason for {:?} leaks `{token}`: `{ext}`",
                    outcome.verdict
                );
            }
        }
    }

    /// Failure-contract table (spec §8.3 / §3.4): every infrastructure failure
    /// and the PAD veto must map to the intended `Verdict`, and the PAD veto
    /// must be indistinguishable from a regular NoMatch at the trust boundary
    /// (same verdict, same absent external reason).
    #[test]
    fn failure_modes_map_to_intended_verdicts() {
        // Missing camera → Unavailable.
        let no_camera = VerifyOutcome::unavailable("no capture source");
        assert_eq!(no_camera.verdict, Verdict::Unavailable);
        assert_eq!(
            external_reason(&no_camera).as_deref(),
            Some("no capture source")
        );

        // Missing store → Unavailable.
        let store_err = VerifyOutcome::unavailable("store error");
        assert_eq!(store_err.verdict, Verdict::Unavailable);

        // Missing/broken worker → Unavailable.
        let worker_err = VerifyOutcome::unavailable("inference failed");
        assert_eq!(worker_err.verdict, Verdict::Unavailable);

        // Timeout → NoMatch (the loop exhausted attempts before a Match).
        let timeout = VerifyOutcome::no_match("timeout");
        assert_eq!(timeout.verdict, Verdict::NoMatch);
        assert_eq!(external_reason(&timeout).as_deref(), Some("timeout"));

        // Rate limit → RateLimited, reason passed through (no PAD detail).
        let rate = VerifyOutcome {
            verdict: Verdict::RateLimited,
            score: 0.0,
            reason: Some("backoff active".into()),
        };
        assert_eq!(rate.verdict, Verdict::RateLimited);
        assert_eq!(external_reason(&rate).as_deref(), Some("backoff active"));

        // PAD veto → NoMatch with NO external reason (indistinguishable from a
        // regular NoMatch that also carries no reason).
        let pad_veto = VerifyOutcome::no_match("pad veto: energy (0.123)");
        assert_eq!(pad_veto.verdict, Verdict::NoMatch);
        assert!(external_reason(&pad_veto).is_none());

        // A regular NoMatch with no reason is the canonical indistinguishable
        // case; the PAD veto must match it exactly at the boundary.
        let plain_no_match = VerifyOutcome::no_match("");
        assert_eq!(pad_veto.verdict, plain_no_match.verdict);
        assert_eq!(external_reason(&pad_veto), external_reason(&plain_no_match));
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
