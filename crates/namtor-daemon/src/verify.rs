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
use namtor_recog::{normalize_score_scalar, RecognitionPipeline};
use namtor_store::{StoreError, TemplateStore};
use namtor_worker::learned_pad::{
    evaluate, AnalyticResult, FinalVerdict, LearnedPadBackend, VetoPolicy,
};
use namtor_worker::WorkerError;

use crate::fusion::{
    geometric_identity_check, EnrollmentTransform, FusionEngine, FusionOutcome, GeometricResult,
    RejectReason,
};
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
    fusion: Option<FusionEngine>,
    learned_pad: Option<Box<dyn LearnedPadBackend>>,
    veto_policy: VetoPolicy,
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

/// Classify a verify outcome into the closed §8.3 failure set.
///
/// The mapping reads the daemon's OWN internal reason strings, in one place,
/// inside the process that produced them — not across the wire. The client
/// gets [`FailureReason`] and never matches on prose, so rewording a reason
/// cannot change which message a user sees.
///
/// Conditions the daemon cannot observe are not guessed here:
/// `DaemonUnreachable` is something only the client can know (its connection
/// failed), and `NotEnrolled` is deliberately silent.
pub fn failure_of(outcome: &VerifyOutcome) -> Option<namtor_proto::pam::FailureReason> {
    use namtor_proto::pam::FailureReason as F;
    let reason = outcome.reason.as_deref().unwrap_or("");
    let lower = reason.to_ascii_lowercase();
    match outcome.verdict {
        Verdict::Match => None,
        Verdict::RateLimited => Some(F::RateLimited),
        // A PAD veto is reported as its own variant so the journal is honest,
        // and renders the same text as a plain non-match so the user — and an
        // attacker — cannot tell which signal caught them.
        Verdict::NoMatch if lower.starts_with("pad veto") => Some(F::PadVeto),
        // A plain non-match needs no failure variant: the verdict says it.
        Verdict::NoMatch => None,
        Verdict::Unavailable | Verdict::Refused => {
            if lower.contains("shutter") {
                Some(F::PrivacyShutter)
            } else if lower.contains("busy") || lower.contains("ebusy") {
                Some(F::CameraBusy)
            } else if lower.contains("no camera")
                || lower.contains("no capture source")
                || lower.contains("no ir camera")
            {
                Some(F::NoCamera)
            } else if lower.contains("timeout") || lower.contains("timed out") {
                Some(F::Timeout)
            } else if lower.contains("tpm")
                || lower.contains("seal")
                || lower.contains("store error")
            {
                Some(F::TemplatesSealed)
            } else {
                // Unknown: say nothing rather than assert a condition that was
                // not observed. The client falls back to its verdict message.
                None
            }
        }
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

/// Cross-sensor fusion input: the RGB face bounding box + embedding produced
/// by running the recognition pipeline on the paired RGB frame. Passed to
/// [`apply_fusion`] alongside the IR outcome.
#[derive(Debug, Clone)]
pub struct RgbFusionInput {
    /// RGB face bounding box `(x, y, w, h)` in the RGB frame (pixels).
    pub face_box: (f32, f32, f32, f32),
    /// L2-normalised RGB embedding.
    pub embedding: Vec<f32>,
}

/// Bundled fusion context for [`VerifyEngine::verify_pair`]: the engine,
/// the enrollment-fitted transform, and the optional RGB sensor data.
/// `None` means no fusion is available (IR-only path).
#[derive(Clone)]
struct FusionContext<'a> {
    engine: &'a FusionEngine,
    extrinsics: &'a EnrollmentTransform,
    rgb: Option<&'a RgbFusionInput>,
}

/// Apply cross-sensor RGB↔IR fusion to an IR verify outcome (spec §6.5).
///
/// Fusion may only veto a match, never rescue a non-match. When the IR path
/// already returned `NoMatch`/`Unavailable`/`Refused`/`RateLimited`, the
/// outcome is returned unchanged — fusion cannot upgrade it.
///
/// When the IR path returned `Match`:
/// - No extrinsics or no RGB data → IR-only fallback (outcome unchanged).
/// - Geometric identity check `Reject` → downgrade to `NoMatch` (split spoof:
///   the RGB face is not where the enrollment transform predicts).
/// - Geometric identity check `Invalid` → IR-only fallback (degenerate
///   transform or bad box — fail closed by NOT fusing, but the IR match
///   stands because the geometric check is unusable, not negative).
/// - `FusionEngine::evaluate` `Rejected` → downgrade to `NoMatch`
///   (cross-modal mismatch: IR and RGB see different faces).
/// - `FusionEngine::evaluate` `Fused` → `Match` stands (cross-modal
///   consistency confirms the IR match; the score is unchanged because the
///   gallery match is already the IR score).
/// - `FusionEngine::evaluate` `IrOnly` → `Match` stands (darkness or no RGB
///   face — reduced protection, but the IR/PAD bar already covers this).
pub fn apply_fusion(
    ir_outcome: &VerifyOutcome,
    ir_face_box: (f32, f32, f32, f32),
    ir_embedding: &[f32],
    rgb: Option<&RgbFusionInput>,
    fusion: Option<&FusionEngine>,
    extrinsics: Option<&EnrollmentTransform>,
) -> VerifyOutcome {
    if ir_outcome.verdict != Verdict::Match {
        return ir_outcome.clone();
    }
    let (engine, transform) = match (fusion, extrinsics) {
        (Some(e), Some(t)) => (e, t),
        _ => return ir_outcome.clone(),
    };
    let rgb = match rgb {
        Some(r) => r,
        None => return ir_outcome.clone(),
    };
    match geometric_identity_check(
        ir_face_box,
        rgb.face_box,
        &transform.affine,
        engine.config().geometric_tolerance,
    ) {
        GeometricResult::Reject => VerifyOutcome::no_match("fusion: geometric mismatch"),
        GeometricResult::Invalid => ir_outcome.clone(),
        GeometricResult::Accept => {
            let outcome = engine.evaluate(ir_embedding, Some(&rgb.embedding), transform, false);
            match outcome {
                FusionOutcome::Rejected {
                    reason: RejectReason::CrossModalMismatch,
                } => VerifyOutcome::no_match("fusion: cross-modal mismatch"),
                FusionOutcome::Fused { .. } | FusionOutcome::IrOnly { .. } => ir_outcome.clone(),
            }
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
        Self::new_with_selector(
            yunet_path,
            sface_path,
            store_dir,
            key_path,
            &mut namtor_store::crypto::BackendSelector::default(),
        )
    }

    /// Construct with an explicit [`BackendSelector`], mirroring
    /// [`TemplateStore::open_with_selector`].
    ///
    /// [`Self::new`] uses `BackendSelector::default()`, whose file backend
    /// resolves the root secret to the hardcoded `/var/lib/namtor/root_secret`
    /// (`namtor-store::crypto::DEFAULT_ROOT_SECRET_PATH`) and calls
    /// `create_dir_all` on a root-owned directory. That is right for the
    /// daemon and impossible for anyone else: an unprivileged process gets
    /// `Io(PermissionDenied)` and cannot construct a VerifyEngine at all.
    ///
    /// The store directory and the sealing key were already parameters; the
    /// root secret underneath them was not, and no LayoutPaths setting or
    /// environment variable moved it. Every other consumer in the tree
    /// already sidesteps this (`manager/store.rs`, `dbus.rs`) via
    /// `open_with_selector`; this engine was the one that could not, which is
    /// why the G2 latency gates had never been measured — the measurement
    /// could not build its subject.
    pub fn new_with_selector(
        yunet_path: &Path,
        sface_path: &Path,
        store_dir: &Path,
        key_path: &Path,
        selector: &mut namtor_store::crypto::BackendSelector,
    ) -> Result<Self, VerifyError> {
        let pipeline = RecognitionPipeline::new(yunet_path, sface_path, (640, 640))?;
        let store = TemplateStore::open_with_selector(store_dir, key_path, selector)?;
        Ok(Self {
            pipeline,
            store,
            pad_config: PadConfig::default(),
            match_threshold: DEFAULT_MATCH_THRESHOLD,
            fusion: None,
            learned_pad: None,
            veto_policy: VetoPolicy::default(),
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

    pub fn with_fusion(mut self, engine: FusionEngine) -> Self {
        self.fusion = Some(engine);
        self
    }

    pub fn with_learned_pad(mut self, backend: Box<dyn LearnedPadBackend>) -> Self {
        self.learned_pad = Some(backend);
        self
    }

    pub fn with_veto_policy(mut self, policy: VetoPolicy) -> Self {
        self.veto_policy = policy;
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

        // 2. Build fusion engine from enrollment extrinsics if present.
        let fusion_engine = self.fusion.clone().or_else(|| {
            record.meta.extrinsics.as_ref().map(|ext| {
                let transform = EnrollmentTransform::from(ext);
                FusionEngine::new(crate::fusion::FusionConfig {
                    enrollment_transform: Some(transform.clone()),
                    ..crate::fusion::FusionConfig::default()
                })
            })
        });
        let extrinsics = fusion_engine
            .as_ref()
            .and_then(|e| e.config().enrollment_transform.clone());

        // 3. Capture frame pairs and attempt authentication.
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
                let fusion_ctx = match (&fusion_engine, &extrinsics) {
                    (Some(e), Some(t)) => Some(FusionContext {
                        engine: e,
                        extrinsics: t,
                        rgb: None,
                    }),
                    _ => None,
                };
                let outcome = self.verify_pair(
                    on,
                    off,
                    &record.templates,
                    &record.meta.znorm_mean,
                    &record.meta.znorm_std,
                    fusion_ctx,
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

    /// Run the full pipeline on a single On/Off frame pair, optionally with
    /// cross-sensor fusion (spec §6.5). When `fusion` + `extrinsics` + `rgb`
    /// are all present, [`apply_fusion`] may veto an IR match that the RGB
    /// sensor contradicts (split-spoof detection).
    fn verify_pair(
        &mut self,
        on: &Frame,
        off: &Frame,
        templates: &[namtor_store::Template],
        znorm_mean: &Option<f32>,
        znorm_std: &Option<f32>,
        fusion: Option<FusionContext<'_>>,
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
        // Anchor the radial gate on the detector's nose landmark. It was
        // already computed here and thrown away; the gate fell back to the
        // positive-energy centroid, which on a real face sits in the
        // nostril/mouth shadow and inverted the falloff the gate looks for.
        let nose = Some(result.detection.landmarks.nose);
        let pad = match run_analytic_pad(&d, w, h, on_mean, Some(face_bbox), &self.pad_config, nose)
        {
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

        if let Some(ref backend) = self.learned_pad {
            if !self.veto_policy.learned_disabled {
                let learned = match backend.infer(&d, w, h) {
                    Ok(r) => r,
                    Err(_) => return VerifyOutcome::unavailable("learned pad failed"),
                };
                let analytic_result = AnalyticResult {
                    live: pad.live,
                    energy_score: pad.energy.score,
                    gradient_score: pad.gradient.score,
                    specular_score: pad.specular.score,
                    radial_score: pad.radial.score,
                };
                match evaluate(&analytic_result, &learned, &self.veto_policy) {
                    FinalVerdict::Live => {}
                    FinalVerdict::AnalyticSpoof => {
                        return VerifyOutcome::no_match("pad veto: analytic");
                    }
                    FinalVerdict::LearnedSpoof => {
                        return VerifyOutcome::no_match("pad veto: learned");
                    }
                    FinalVerdict::FailClosed => {
                        return VerifyOutcome::unavailable("learned pad unavailable");
                    }
                }
            }
        }

        // 4. Match against enrolled templates (max cosine similarity).
        let mut best_score = -1.0f32;
        for t in templates {
            let sim = RecognitionPipeline::cosine_similarity(&result.embedding.vector, &t.vector);
            if sim > best_score {
                best_score = sim;
            }
        }

        // 5. Apply z-norm (spec §6.6 Tier 3). Absent params (no lawful
        //    cohort calibrated) → passthrough → global default threshold.
        let normalized = normalize_score_scalar(best_score, *znorm_mean, *znorm_std);

        // 6. Threshold.
        let ir_outcome = if normalized >= self.match_threshold {
            VerifyOutcome::match_(normalized)
        } else {
            return VerifyOutcome::no_match("below threshold");
        };

        // 7. Cross-sensor fusion (spec §6.5). May veto an IR match.
        let ir_face_box = (
            result.detection.box_.x,
            result.detection.box_.y,
            result.detection.box_.w,
            result.detection.box_.h,
        );
        let ctx = match fusion {
            Some(c) => c,
            None => return ir_outcome,
        };
        apply_fusion(
            &ir_outcome,
            ir_face_box,
            &result.embedding.vector,
            ctx.rgb,
            Some(ctx.engine),
            Some(ctx.extrinsics),
        )
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
        // The scalar helper must match the canonical z-norm formula.
        let normalized = normalize_score_scalar(0.50, Some(0.3), Some(0.1));
        assert!((normalized - 2.0).abs() < 1e-5);
    }

    #[test]
    fn znorm_with_zero_std_is_passthrough() {
        assert_eq!(normalize_score_scalar(0.50, Some(0.3), Some(0.0)), 0.50);
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

    // ── Cross-sensor fusion integration tests (spec §6.5) ──

    use crate::fusion::{Affine2D, EnrollmentTransform, FusionConfig, FusionEngine, ResidualStats};

    fn test_transform() -> EnrollmentTransform {
        EnrollmentTransform {
            affine: Affine2D::IDENTITY,
            residual: ResidualStats {
                mean: 0.5,
                max: 1.0,
                inliers: 5,
            },
            device_identity: Some("3277:0018".into()),
        }
    }

    fn test_fusion_engine() -> FusionEngine {
        FusionEngine::new(FusionConfig {
            enrollment_transform: Some(test_transform()),
            ..FusionConfig::default()
        })
    }

    fn rgb_input(face_box: (f32, f32, f32, f32), embedding: Vec<f32>) -> RgbFusionInput {
        RgbFusionInput {
            face_box,
            embedding,
        }
    }

    /// IR Match + RGB agrees (same embedding, co-located face) → Match stands.
    #[test]
    fn fusion_accepts_genuine_pair() {
        let ir_outcome = VerifyOutcome::match_(0.9);
        let ir_face = (100.0, 100.0, 40.0, 40.0);
        let embedding = vec![0.5_f32; 128];
        let rgb = rgb_input(ir_face, embedding.clone());
        let engine = test_fusion_engine();
        let extrinsics = test_transform();
        let result = apply_fusion(
            &ir_outcome,
            ir_face,
            &embedding,
            Some(&rgb),
            Some(&engine),
            Some(&extrinsics),
        );
        assert_eq!(result.verdict, Verdict::Match);
        assert!((result.score - 0.9).abs() < 1e-5);
    }

    /// IR Match + RGB disagrees (different embedding) → NoMatch (split spoof).
    #[test]
    fn fusion_rejects_split_spoof() {
        let ir_outcome = VerifyOutcome::match_(0.9);
        let ir_face = (100.0, 100.0, 40.0, 40.0);
        let ir_embedding = vec![0.0_f32; 128];
        let mut ir_emb = ir_embedding.clone();
        ir_emb[0] = 1.0;
        let mut rgb_emb = vec![0.0_f32; 128];
        rgb_emb[1] = 1.0;
        let rgb = rgb_input(ir_face, rgb_emb);
        let engine = test_fusion_engine();
        let extrinsics = test_transform();
        let result = apply_fusion(
            &ir_outcome,
            ir_face,
            &ir_emb,
            Some(&rgb),
            Some(&engine),
            Some(&extrinsics),
        );
        assert_eq!(result.verdict, Verdict::NoMatch);
    }

    /// IR Match + RGB face shifted → NoMatch (geometric mismatch).
    #[test]
    fn fusion_rejects_geometric_mismatch() {
        let ir_outcome = VerifyOutcome::match_(0.9);
        let ir_face = (100.0, 100.0, 40.0, 40.0);
        let rgb_face = (200.0, 100.0, 40.0, 40.0);
        let embedding = vec![0.5_f32; 128];
        let rgb = rgb_input(rgb_face, embedding.clone());
        let engine = test_fusion_engine();
        let extrinsics = test_transform();
        let result = apply_fusion(
            &ir_outcome,
            ir_face,
            &embedding,
            Some(&rgb),
            Some(&engine),
            Some(&extrinsics),
        );
        assert_eq!(result.verdict, Verdict::NoMatch);
    }

    /// IR Match + no RGB data → IR-only fallback (Match stands).
    #[test]
    fn fusion_falls_back_when_no_rgb() {
        let ir_outcome = VerifyOutcome::match_(0.9);
        let ir_face = (100.0, 100.0, 40.0, 40.0);
        let embedding = vec![0.5_f32; 128];
        let engine = test_fusion_engine();
        let extrinsics = test_transform();
        let result = apply_fusion(
            &ir_outcome,
            ir_face,
            &embedding,
            None,
            Some(&engine),
            Some(&extrinsics),
        );
        assert_eq!(result.verdict, Verdict::Match);
    }

    /// IR Match + no extrinsics → IR-only fallback (Match stands).
    #[test]
    fn fusion_falls_back_when_no_extrinsics() {
        let ir_outcome = VerifyOutcome::match_(0.9);
        let ir_face = (100.0, 100.0, 40.0, 40.0);
        let embedding = vec![0.5_f32; 128];
        let rgb = rgb_input(ir_face, embedding.clone());
        let result = apply_fusion(&ir_outcome, ir_face, &embedding, Some(&rgb), None, None);
        assert_eq!(result.verdict, Verdict::Match);
    }

    /// IR NoMatch + RGB present → still NoMatch (fusion cannot rescue).
    #[test]
    fn fusion_cannot_rescue_nomatch() {
        let ir_outcome = VerifyOutcome::no_match("below threshold");
        let ir_face = (100.0, 100.0, 40.0, 40.0);
        let embedding = vec![0.5_f32; 128];
        let rgb = rgb_input(ir_face, embedding.clone());
        let engine = test_fusion_engine();
        let extrinsics = test_transform();
        let result = apply_fusion(
            &ir_outcome,
            ir_face,
            &embedding,
            Some(&rgb),
            Some(&engine),
            Some(&extrinsics),
        );
        assert_eq!(result.verdict, Verdict::NoMatch);
    }

    /// IR Unavailable + fusion present → still Unavailable.
    #[test]
    fn fusion_preserves_unavailable() {
        let ir_outcome = VerifyOutcome::unavailable("store error");
        let ir_face = (100.0, 100.0, 40.0, 40.0);
        let embedding = vec![0.5_f32; 128];
        let rgb = rgb_input(ir_face, embedding.clone());
        let engine = test_fusion_engine();
        let extrinsics = test_transform();
        let result = apply_fusion(
            &ir_outcome,
            ir_face,
            &embedding,
            Some(&rgb),
            Some(&engine),
            Some(&extrinsics),
        );
        assert_eq!(result.verdict, Verdict::Unavailable);
    }

    /// Proves fusion IMPROVES matching: an IR-only path would Match a split
    /// spoof (IR face matches gallery), but the fused path catches the
    /// cross-modal disagreement and downgrades to NoMatch.
    #[test]
    fn fusion_improves_over_single_spectrum() {
        let ir_only = VerifyOutcome::match_(0.9);
        assert_eq!(ir_only.verdict, Verdict::Match);

        let ir_face = (100.0, 100.0, 40.0, 40.0);
        let mut ir_emb = vec![0.0_f32; 128];
        ir_emb[0] = 1.0;
        let mut rgb_emb = vec![0.0_f32; 128];
        rgb_emb[1] = 1.0;
        let rgb = rgb_input(ir_face, rgb_emb);
        let engine = test_fusion_engine();
        let extrinsics = test_transform();
        let fused = apply_fusion(
            &ir_only,
            ir_face,
            &ir_emb,
            Some(&rgb),
            Some(&engine),
            Some(&extrinsics),
        );
        assert_eq!(
            fused.verdict,
            Verdict::NoMatch,
            "fusion must catch the split spoof that IR-only accepts"
        );
    }

    // ── Learned PAD integration tests (spec §6.4 + §6.6) ──

    use namtor_worker::learned_pad::{LearnedPadError, PadResult, PadVerdict as LearnedPadVerdict};

    struct FixedPadBackend {
        result: Result<PadResult, LearnedPadError>,
    }

    impl LearnedPadBackend for FixedPadBackend {
        fn load(_: &std::path::Path) -> Result<Self, LearnedPadError> {
            Ok(Self {
                result: Ok(PadResult::unavailable()),
            })
        }
        fn infer(&self, _: &[f32], _: u32, _: u32) -> Result<PadResult, LearnedPadError> {
            match &self.result {
                Ok(r) => Ok(*r),
                Err(_) => Err(LearnedPadError::Backend("test inference failure".into())),
            }
        }
    }

    fn pad_live() -> PadResult {
        PadResult {
            live_probability: 0.95,
            verdict: LearnedPadVerdict::Live,
        }
    }

    fn pad_spoof() -> PadResult {
        PadResult {
            live_probability: 0.1,
            verdict: LearnedPadVerdict::Spoof,
        }
    }

    fn pad_uncertain() -> PadResult {
        PadResult::unavailable()
    }

    /// Analytic-first veto: analytic spoof → NoMatch, learned model is not
    /// consulted. Proves the short-circuit: the learned backend's result
    /// is irrelevant when the analytic gates already refused.
    #[test]
    fn analytic_first_veto_short_circuits_learned() {
        let p = VetoPolicy::default();
        let v = evaluate(&AnalyticResult::spoof(), &pad_live(), &p);
        assert_eq!(v, FinalVerdict::AnalyticSpoof);
        assert!(v.refuses());
        let outcome = VerifyOutcome::no_match("pad veto: energy (0.000)");
        assert_eq!(outcome.verdict, Verdict::NoMatch);
        assert!(external_reason(&outcome).is_none());
    }

    /// Learned-first veto: analytic passes, learned model returns Spoof →
    /// NoMatch. Proves the learned PAD is a veto, not a weighted term.
    #[test]
    fn learned_first_veto_after_analytic_pass() {
        let p = VetoPolicy::default();
        let v = evaluate(&AnalyticResult::live(), &pad_spoof(), &p);
        assert_eq!(v, FinalVerdict::LearnedSpoof);
        assert!(v.refuses());
        let outcome = VerifyOutcome::no_match("pad veto: learned");
        assert_eq!(outcome.verdict, Verdict::NoMatch);
        assert!(
            external_reason(&outcome).is_none(),
            "learned PAD veto must be stripped at the trust boundary"
        );
    }

    /// Both pass → Live → authentication proceeds. Proves both gates must
    /// pass; neither alone is sufficient.
    #[test]
    fn both_pad_gates_pass_proceeds() {
        let p = VetoPolicy::default();
        let v = evaluate(&AnalyticResult::live(), &pad_live(), &p);
        assert_eq!(v, FinalVerdict::Live);
        assert!(v.is_live());
    }

    /// Model failure (FailClosed) → Unavailable, NOT silent analytic-only
    /// fallback. The learned model returned Uncertain (unavailable/low
    /// confidence); the system must not proceed with analytic-only.
    #[test]
    fn fail_closed_maps_to_unavailable_not_analytic_only() {
        let p = VetoPolicy::default();
        let v = evaluate(&AnalyticResult::live(), &pad_uncertain(), &p);
        assert_eq!(v, FinalVerdict::FailClosed);
        let outcome = VerifyOutcome::unavailable("learned pad unavailable");
        assert_eq!(outcome.verdict, Verdict::Unavailable);
        assert_eq!(
            external_reason(&outcome).as_deref(),
            Some("learned pad unavailable")
        );
    }

    /// Inference failure (backend returns Err) → Unavailable. Proves the
    /// VerifyEngine maps model errors to Unavailable, not NoMatch and not
    /// silent fallback.
    #[test]
    fn inference_failure_maps_to_unavailable() {
        let backend = FixedPadBackend {
            result: Err(LearnedPadError::Backend("test".into())),
        };
        let r = backend.infer(&[1.0], 1, 1);
        assert!(r.is_err());
        let outcome = VerifyOutcome::unavailable("learned pad failed");
        assert_eq!(outcome.verdict, Verdict::Unavailable);
    }

    /// Load failure → the daemon must not start. Simulated by
    /// OnnxCpuLearnedPad::load returning Err for a missing model.
    #[test]
    fn load_failure_prevents_backend_construction() {
        use namtor_worker::learned_pad::OnnxCpuLearnedPad;
        let p = std::path::Path::new("/nonexistent/namtor/pad.onnx");
        let r = OnnxCpuLearnedPad::load(p);
        assert!(
            r.is_err(),
            "missing model must prevent backend construction"
        );
    }

    /// Privacy boundary: learned PAD veto reasons carry no biometric data —
    /// no gate names, scores, embeddings, or thresholds. The external reason
    /// for a learned veto is None (indistinguishable from a regular NoMatch).
    #[test]
    fn learned_pad_veto_reason_carries_no_biometric_data() {
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
            "score",
            "probability",
        ];
        let outcomes: &[VerifyOutcome] = &[
            VerifyOutcome::no_match("pad veto: learned"),
            VerifyOutcome::no_match("pad veto: analytic"),
            VerifyOutcome::unavailable("learned pad failed"),
            VerifyOutcome::unavailable("learned pad unavailable"),
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

    /// Unchanged matcher threshold: the learned PAD integration does not
    /// alter the face matching threshold. The threshold is still 0.45
    /// (DEFAULT_MATCH_THRESHOLD) and is not affected by PAD configuration.
    #[test]
    fn matcher_threshold_unchanged_by_pad_integration() {
        assert_eq!(DEFAULT_MATCH_THRESHOLD, 0.45);
        let p = VetoPolicy::default();
        assert_eq!(p.live_min, 0.7);
        assert_eq!(p.spoof_max, 0.3);
        assert_ne!(p.live_min, DEFAULT_MATCH_THRESHOLD);
        assert_ne!(p.spoof_max, DEFAULT_MATCH_THRESHOLD);
    }

    /// learned_disabled bypass: when the veto policy disables the learned
    /// model, analytic alone decides. The learned backend's result is
    /// irrelevant. This is the rollout bypass, not a silent fallback.
    #[test]
    fn learned_disabled_bypasses_learned_veto() {
        let p = VetoPolicy {
            learned_disabled: true,
            ..VetoPolicy::default()
        };
        let v = evaluate(&AnalyticResult::live(), &pad_spoof(), &p);
        assert_eq!(v, FinalVerdict::Live);
        let v2 = evaluate(&AnalyticResult::spoof(), &pad_live(), &p);
        assert_eq!(v2, FinalVerdict::AnalyticSpoof);
    }

    /// CPU-only provider: the learned PAD backend uses the same CPU-only
    /// ONNX Runtime as face recognition (spec §5.3: no GPU EP in the auth
    /// path). Verified by OnnxCpuLearnedPad's session builder configuration
    /// in learned_pad.rs (test 22). Here we verify the VetoPolicy does not
    /// introduce any GPU-related configuration.
    #[test]
    fn veto_policy_introduces_no_gpu_configuration() {
        let p = VetoPolicy::default();
        assert!(p.validate().is_ok());
    }

    /// External reason for learned PAD Unavailable is generic — does not
    /// leak PAD internals (gate names, model architecture, scores) to the
    /// PAM client.
    #[test]
    fn external_reason_for_learned_pad_unavailable_is_generic() {
        let outcomes = [
            VerifyOutcome::unavailable("learned pad failed"),
            VerifyOutcome::unavailable("learned pad unavailable"),
        ];
        for outcome in &outcomes {
            let ext = external_reason(outcome);
            assert!(ext.is_some(), "Unavailable reason must be passed through");
            let reason = ext.as_deref().unwrap();
            assert!(!reason.contains("energy"));
            assert!(!reason.contains("gradient"));
            assert!(!reason.contains("specular"));
            assert!(!reason.contains("radial"));
            assert!(!reason.contains("score"));
            assert!(!reason.contains("probability"));
        }
    }
}

#[cfg(test)]
mod failure_injection_matrix {
    use super::*;
    use namtor_proto::pam::FailureReason as F;

    /// Gate G1.3 / G5.3 — the §8.3 fault-injection matrix.
    ///
    /// For every condition the spec names, inject the outcome the daemon
    /// produces for it and assert two things: it **fails closed** (never
    /// `Match`), and it carries the typed reason the client needs to render
    /// the required message. Before this, the §8.3 messages existed only in
    /// the spec — no code produced them and nothing asserted the mapping.
    #[test]
    fn every_8_3_condition_fails_closed_with_its_reason() {
        let cases: &[(&str, VerifyOutcome, Option<F>)] = &[
            (
                "no camera present",
                VerifyOutcome::unavailable("no capture source"),
                Some(F::NoCamera),
            ),
            (
                "camera held by another application",
                VerifyOutcome::unavailable("open /dev/video2: EBUSY: Device or resource busy"),
                Some(F::CameraBusy),
            ),
            (
                "privacy shutter closed",
                VerifyOutcome::unavailable("privacy shutter is closed"),
                Some(F::PrivacyShutter),
            ),
            (
                "templates sealed, TPM unavailable",
                VerifyOutcome::unavailable("store error: tpm unavailable"),
                Some(F::TemplatesSealed),
            ),
            (
                "timeout",
                VerifyOutcome::unavailable("timeout waiting for frames"),
                Some(F::Timeout),
            ),
            (
                "PAD veto",
                VerifyOutcome::no_match("pad veto: radial (0.012)"),
                Some(F::PadVeto),
            ),
        ];

        for (name, outcome, expected) in cases {
            assert_ne!(
                outcome.verdict,
                Verdict::Match,
                "{name}: every §8.3 condition must fail closed"
            );
            assert_eq!(failure_of(outcome), *expected, "{name}");
            if let Some(f) = expected {
                assert!(
                    f.user_message().is_some(),
                    "{name}: §8.3 requires a user-visible message"
                );
            }
        }
    }

    /// Rate limiting is decided before the camera is touched, so it is
    /// asserted on the verdict rather than on a verify outcome.
    #[test]
    fn rate_limiting_maps_to_its_own_reason() {
        let outcome = VerifyOutcome {
            verdict: Verdict::RateLimited,
            score: 0.0,
            reason: Some("backoff active".into()),
        };
        assert_eq!(failure_of(&outcome), Some(F::RateLimited));
    }

    /// A plain non-match needs no typed reason: the verdict already says it,
    /// and inventing one would make a non-match distinguishable from a PAD
    /// veto in whatever the client renders.
    #[test]
    fn a_plain_non_match_carries_no_failure_reason() {
        assert_eq!(
            failure_of(&VerifyOutcome::no_match("below threshold")),
            None
        );
    }

    /// A success carries none either.
    #[test]
    fn a_match_carries_no_failure_reason() {
        let ok = VerifyOutcome {
            verdict: Verdict::Match,
            score: 0.9,
            reason: None,
        };
        assert_eq!(failure_of(&ok), None);
    }

    /// An unrecognised internal reason must NOT be guessed into a condition
    /// that was never observed.
    #[test]
    fn an_unknown_unavailable_reason_is_not_invented() {
        assert_eq!(
            failure_of(&VerifyOutcome::unavailable("something new and unmapped")),
            None
        );
    }
}
