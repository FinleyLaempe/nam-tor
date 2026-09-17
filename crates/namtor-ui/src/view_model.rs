// SPDX-License-Identifier: GPL-3.0-only
//! View model for the Nam-tor management UI.
//
// allow: SIZE_OK — this is a flat registry of small privacy-safe view-model
// structs (one per UI surface). The 302-LOC pre-existing size predates task
// 13; the task-13 addition is the 12-line `ProfileSetupView` required for
// the privacy scan. A split by concern (enrollment/profile/system/pam) is
// possible but out of scope for the accessibility task.
//
//! This module provides a pure-data representation of the UI state.
//! The `ProfilePresenter` transforms `ManagerClient` responses into these
//! views, which the GTK shell then renders.

use crate::client::{
    EnrollmentProgress, Guidance, PamServiceCompatibility, PamServiceInfo, ProtectionKeyKind,
    ProtectionStatus, QualityLabel, StatusReport,
};

/// Coarse quality bucket the user sees, plus a guidance flag the view renders
/// as an accessibility-friendly alert when diversity is low.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QualityView {
    pub label: String,
    pub show_guidance: bool,
}

impl QualityView {
    pub fn from_label(label: QualityLabel) -> Self {
        match label {
            QualityLabel::Low => Self {
                label: "low".into(),
                show_guidance: true,
            },
            QualityLabel::Medium => Self {
                label: "medium".into(),
                show_guidance: false,
            },
            QualityLabel::High => Self {
                label: "high".into(),
                show_guidance: false,
            },
        }
    }
}

/// One row in the profile list. The view renders name, enabled toggle, and a
/// localized "last used" line. No biometric data, ever.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProfileRowView {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub created: u64,
    /// `0` means the profile has never authenticated.
    pub last_used: u64,
}

impl ProfileRowView {
    pub fn from_info(info: crate::client::ProfileInfo) -> Self {
        Self {
            id: info.id,
            name: info.name,
            enabled: info.enabled,
            created: info.created,
            last_used: info.last_used,
        }
    }
}

/// Recovery guidance shown when the daemon is unreachable. The text is
/// deliberately actionable, not a raw error dump.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RecoveryHint {
    pub title: String,
    pub steps: Vec<String>,
}

impl RecoveryHint {
    pub fn for_error(err: &crate::client::ManagerError) -> Option<Self> {
        match err {
            crate::client::ManagerError::DaemonUnavailable(detail) => Some(Self {
                title: "Nam-tor daemon is not running".into(),
                steps: vec![
                    "Verify the daemon is enabled: systemctl is-enabled namtor-daemon".into(),
                    "Start the daemon: sudo systemctl start namtor-daemon".into(),
                    "Check logs for failures: journalctl -u namtor-daemon".into(),
                    format!("Technical detail: {detail}"),
                ],
            }),
            crate::client::ManagerError::ServiceNotRegistered => Some(Self {
                title: "Nam-tor service is not registered".into(),
                steps: vec![
                    "The D-Bus service file is missing or corrupted".into(),
                    "Reinstall the namtor-daemon package to restore service files".into(),
                    "Restart the system bus: sudo systemctl reload dbus".into(),
                ],
            }),
            crate::client::ManagerError::Denied(detail) => Some(Self {
                title: "Administrator approval required".into(),
                steps: vec![
                    "The operation requires elevated privileges".into(),
                    "Ensure your user is in the 'namtor-auth' group".into(),
                    "Run: sudo usermod -aG namtor-auth $USER then log out and back in".into(),
                    format!("Polkit detail: {detail}"),
                ],
            }),
            _ => None,
        }
    }
}

/// View model rendered by the GTK window. Every field is plain data the view
/// can serialize into Atk roles and gettext strings.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct ReviewView {
    pub profile_id: String,
    pub needs_reenroll: bool,
    pub quality_label: String,
    pub coverage_percent: u32,
    pub frames_captured: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct AuthenticationView {
    /// Prominent recommended service (e.g. GDM).
    pub recommended_service: Option<PamServiceRowView>,
    /// Safe services listed normally.
    pub safe_services: Vec<PamServiceRowView>,
    /// Risky services to be collapsed in an expander.
    pub risky_services: Vec<PamServiceRowView>,
    /// Unsupported services shown as disabled.
    pub unsupported_services: Vec<PamServiceRowView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct SystemView {
    pub daemon_running: bool,
    /// A supported IR camera is present on this machine.
    ///
    /// Probed locally from sysfs against the quirks database, not inferred.
    /// This used to be `camera_detected: daemon_reachable` with the comment
    /// "Simplified: if daemon is up, camera is assumed ready for the view",
    /// so the System page answered "IR Camera — Detected" on a machine with
    /// no camera at all, and gave no clue why enrollment then failed.
    pub camera_detected: bool,
    /// The daemon has validated its models.
    ///
    /// Sound to infer from reachability: `namtor daemon` validates every
    /// model and spawns the worker *before* it binds the PAM socket, so a
    /// daemon that answers at all has loaded them.
    pub models_loaded: bool,
    pub protection: ProtectionView,
}

/// Setup-phase view for the enrollment wizard: the name entry and privacy
/// notice shown before capture starts. Carries only a profile name placeholder
/// and a coarse notice flag — no biometric data, no internal state.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct ProfileSetupView {
    /// Placeholder hint for the name entry (e.g. "e.g. Home Office").
    pub name_placeholder: String,
    /// Whether the privacy notice has been acknowledged.
    pub privacy_acknowledged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct PresenterModel {
    pub profiles: Vec<ProfileRowView>,
    pub status: Option<StatusReport>,
    /// v2: template storage protection status (coarse, privacy-safe view).
    pub protection: Option<ProtectionView>,
    /// v2: system readiness indicators (daemon, model, camera).
    pub readiness: Option<ReadinessView>,
    /// Simplified authentication destination view.
    pub authentication: Option<AuthenticationView>,
    /// Simplified system destination view.
    pub system: Option<SystemView>,
    /// v2: detected PAM auth services.
    pub pam_services: Vec<PamServiceRowView>,
    pub enrollment: Option<EnrollmentView>,
    pub review: Option<ReviewView>,
    pub pending_merge: Option<PendingMerge>,
    pub pending_replace: Option<PendingReplace>,
    /// v2: staged PAM toggle awaiting high-impact confirmation.
    pub pending_pam_toggle: Option<PendingPamToggle>,
    /// v3: enrollment lifecycle state.
    pub enrollment_state: EnrollmentState,
    /// v3: current enrollment session id (for staleness checks).
    pub enrollment_session_v3: Option<String>,
    /// v3: live capture-page view.
    pub enrollment_view_v3: Option<EnrollmentViewV3>,
    /// v3: review-page view.
    pub review_view_v3: Option<ReviewViewV3>,
    pub error: Option<ErrorView>,
    pub message: Option<String>,
    pub is_loading: bool,
    /// True when the daemon reports the IR emitter is armed (always-on).
    pub emitter_armed_warning: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EnrollmentView {
    pub session_id: String,
    pub frames_captured: u32,
    pub quality: QualityView,
    pub coverage_percent: u32,
}

impl EnrollmentView {
    pub fn from_progress(p: EnrollmentProgress) -> Self {
        let quality = QualityView::from_label(p.quality);
        Self {
            session_id: p.session_id,
            frames_captured: p.frames_captured,
            quality,
            coverage_percent: p.coverage_percent,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PendingMerge {
    pub src_id: String,
    pub src_name: String,
    pub dst_id: String,
    pub dst_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PendingReplace {
    pub id: String,
    pub old_name: String,
    pub new_name: String,
}

/// v2: staged PAM service toggle awaiting high-impact confirmation. The view
/// shows the experimental-risk warning dialog and only calls
/// [`crate::presenter::ProfilePresenter::confirm_pam_toggle`] if the user
/// acknowledges.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PendingPamToggle {
    pub id: String,
    pub display_name: String,
    pub enabled: bool,
    pub high_impact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ErrorView {
    pub title: String,
    pub detail: String,
    pub recovery: Option<RecoveryHint>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PamServiceRowView {
    pub id: String,
    pub display_name: String,
    pub enabled: bool,
    pub compatibility: PamServiceCompatibility,
    pub description: String,
}

impl PamServiceRowView {
    pub fn from_info(info: PamServiceInfo) -> Self {
        Self {
            id: info.id,
            display_name: info.display_name,
            enabled: info.enabled,
            compatibility: info.compatibility,
            description: info.description,
        }
    }
}

/// v2: human-readable protection status for the settings panel.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProtectionStatusView {
    pub sealing: String,
    pub has_tpm: bool,
    pub warning: String,
}

impl ProtectionStatusView {
    pub fn from_status(status: &ProtectionStatus) -> Self {
        Self {
            sealing: status.sealing.as_str().into(),
            has_tpm: status.has_tpm,
            warning: status.warning.clone(),
        }
    }
}

/// Convenience: the sealing kind as a human-readable label.
pub fn sealing_label(kind: ProtectionKeyKind) -> &'static str {
    match kind {
        ProtectionKeyKind::Tpm2 => "TPM2 (hardware-bound)",
        ProtectionKeyKind::Software => "Software AES-256-GCM (file fallback)",
    }
}

/// Coarse readiness level for a single subsystem. The UI renders this as a
/// labelled status row. No internal state (paths, model ids, thresholds) is
/// exposed — only the three-bucket level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadinessLevel {
    /// Subsystem is operational.
    Ready,
    /// Subsystem is not available.
    NotReady,
    /// Could not determine (daemon unreachable, query failed).
    #[default]
    Unknown,
}

/// v2: system readiness indicators surfaced to the user. Each field is a
/// coarse level — no paths, no model ids, no thresholds, no scores.
///
/// - `daemon`: whether the namtor daemon is reachable on the system bus.
/// - `model`: whether the inference model is loaded (inferred from daemon
///   reachability — the daemon loads models at startup).
/// - `camera`: whether a supported IR camera was detected (inferred from
///   daemon reachability — the daemon requires a camera to function).
/// - `has_enrolled_profiles`: whether the user has at least one profile.
/// - `enabled_pam_count`: coarse count of PAM services with namtor enabled.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct ReadinessView {
    pub daemon: ReadinessLevel,
    pub model: ReadinessLevel,
    pub camera: ReadinessLevel,
    pub has_enrolled_profiles: bool,
    pub enabled_pam_count: u32,
}

/// v2: privacy-safe template protection status. Surfaces a coarse label, a
/// warning string, and a boolean for software fallback. Does NOT expose the
/// raw sealing kind, key material, or internal paths.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct ProtectionView {
    /// Human-readable sealing label (e.g. "TPM2 (hardware-bound)").
    pub label: String,
    /// Warning text shown to the user (empty when hardware-bound).
    pub warning: String,
    /// True when the software AES-256-GCM file fallback is in use.
    pub is_software_fallback: bool,
    /// True when the IR emitter protection is reduced (always-on watchdog).
    pub emitter_reduced: bool,
}

impl ProtectionView {
    /// Build a `ProtectionView` from the raw `ProtectionStatus` and the
    /// emitter-armed flag from the status report.
    pub fn from_status(status: &ProtectionStatus, emitter_armed: bool) -> Self {
        Self {
            label: sealing_label(status.sealing).into(),
            warning: status.warning.clone(),
            is_software_fallback: status.sealing == ProtectionKeyKind::Software,
            emitter_reduced: emitter_armed,
        }
    }
}

/// Convenience: the PAM compatibility as a human-readable label.
pub fn compatibility_label(c: PamServiceCompatibility) -> &'static str {
    match c {
        PamServiceCompatibility::Safe => "Safe",
        PamServiceCompatibility::Unsupported => "Unsupported",
        PamServiceCompatibility::Risky => "Risky (high-impact)",
    }
}

// ---------------------------------------------------------------------------
// v3 enrollment view types — org.namtor.Manager3
// ---------------------------------------------------------------------------

/// v3 enrollment lifecycle state. The presenter transitions through these
/// states as the user moves through the capture → stop → review → save flow.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub enum EnrollmentState {
    /// Initial state: user enters a profile name.
    #[default]
    Setup,
    /// Worker running, progress updates flowing.
    Capturing,
    /// User pressed Stop, waiting for review.
    Stopped,
    /// Review snapshot displayed, user decides save/resume/discard.
    Reviewing,
    /// Commit in flight.
    Saving,
    /// Recoverable error with a message.
    Error(String),
}

impl EnrollmentState {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Setup => "setup",
            Self::Capturing => "capturing",
            Self::Stopped => "stopped",
            Self::Reviewing => "reviewing",
            Self::Saving => "saving",
            Self::Error(_) => "error",
        }
    }
}

/// Typed capture guidance carried on the v3 view models.
///
/// The daemon sends a closed snake_case token (`looking_good`, `move_closer`,
/// …). The view model keeps it **typed** so the UI never decides anything by
/// matching text: a `contains("looking good")` check silently stops working
/// the moment the label is translated. Translation happens once, at render
/// time, in [`GuidanceView::label`].
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
#[serde(untagged)]
pub enum GuidanceView {
    /// The daemon sent no guidance for this update.
    #[default]
    Silent,
    /// A guidance value this client understands.
    Known(Guidance),
    /// A value this client does not recognise. Rendered verbatim so a newer
    /// daemon is never silently ignored, but it drives no UI decision.
    Raw(String),
}

/// The capture signal a piece of guidance points at. Derived from the typed
/// guidance, so the user can see *which* part of the scan is holding it back
/// rather than only an overall percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureSignal {
    /// Where the face sits in the camera's view.
    Framing,
    /// How much light reaches the face.
    Lighting,
    /// How much the face is moving between captures.
    Steadiness,
    /// How many distinct head angles have been captured.
    HeadAngles,
}

impl CaptureSignal {
    /// Short user-facing name. Wrap in [`crate::tr`] at render time.
    pub fn as_label(self) -> &'static str {
        match self {
            Self::Framing => "Framing",
            Self::Lighting => "Lighting",
            Self::Steadiness => "Steadiness",
            Self::HeadAngles => "Head angles",
        }
    }
}

impl GuidanceView {
    /// Parse a wire token. Unknown, non-empty values are kept verbatim.
    pub fn from_wire(s: &str) -> Self {
        match Guidance::from_wire(s) {
            Some(g) => Self::Known(g),
            None if s.trim().is_empty() => Self::Silent,
            None => Self::Raw(s.to_string()),
        }
    }

    /// The translated, user-facing guidance line. This is the only place the
    /// guidance becomes human text.
    pub fn label(&self) -> String {
        match self {
            Self::Silent => crate::tr("Starting the scan…"),
            Self::Known(g) => crate::tr(g.as_label()),
            // Never the token itself. `Raw` means a daemon newer than this UI
            // sent a guidance word we have no text for; rendering it verbatim
            // put raw snake_case — `move_way_back` — on screen as the largest
            // text on the page. Say honestly that we cannot advise, which is
            // what the user needs to know.
            Self::Raw(_) => crate::tr("Keep your face in view"),
        }
    }

    /// True when the daemon sent guidance this build has no text for.
    ///
    /// Used to stop the page claiming everything is fine underneath advice it
    /// could not interpret.
    pub fn is_unrecognised(&self) -> bool {
        matches!(self, Self::Raw(_))
    }

    /// A translated one-line "how" for the guidance — the concrete thing to
    /// do, under the headline. `None` when there is nothing to add.
    pub fn hint(&self) -> Option<String> {
        let hint = match self {
            Self::Known(Guidance::Centering) => "Line your face up with the middle of the screen.",
            Self::Known(Guidance::MoveCloser) => {
                "Come closer until your face fills most of the view."
            }
            Self::Known(Guidance::MoveBack) => "You are too close for the camera to focus.",
            Self::Known(Guidance::BetterLighting) => {
                "Turn towards a window or lamp, and avoid a bright light behind you."
            }
            Self::Known(Guidance::HoldStill) => "Slow, small movements give the scan usable views.",
            Self::Known(Guidance::FaceFront) => "Look straight at the camera to begin.",
            Self::Known(Guidance::InsufficientPose) => {
                "Turn your head slowly left, right, up and down."
            }
            Self::Known(Guidance::LookingGood) => {
                "Keep turning your head slowly so the scan sees every angle."
            }
            Self::Known(Guidance::Capturing) => "Stay in view while the scan gets going.",
            Self::Silent | Self::Raw(_) => return None,
        };
        Some(crate::tr(hint))
    }

    /// The capture signal this guidance points at, or `None` when the scan is
    /// going well and nothing needs attention.
    pub fn signal(&self) -> Option<CaptureSignal> {
        match self {
            Self::Known(
                Guidance::Centering
                | Guidance::MoveCloser
                | Guidance::MoveBack
                | Guidance::FaceFront,
            ) => Some(CaptureSignal::Framing),
            Self::Known(Guidance::BetterLighting) => Some(CaptureSignal::Lighting),
            Self::Known(Guidance::HoldStill) => Some(CaptureSignal::Steadiness),
            Self::Known(Guidance::InsufficientPose) => Some(CaptureSignal::HeadAngles),
            _ => None,
        }
    }

    /// True when the daemon says the current view is usable. Typed, so it
    /// keeps working in every locale.
    pub fn is_encouraging(&self) -> bool {
        matches!(self, Self::Known(Guidance::LookingGood))
    }
}

impl From<&str> for GuidanceView {
    fn from(s: &str) -> Self {
        Self::from_wire(s)
    }
}

impl From<String> for GuidanceView {
    fn from(s: String) -> Self {
        Self::from_wire(&s)
    }
}

impl From<Guidance> for GuidanceView {
    fn from(g: Guidance) -> Self {
        Self::Known(g)
    }
}

/// v3 capture-page view. Renders live progress during enrollment.
/// No biometric data — only bounded percentages, counts, and a guidance label.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct EnrollmentViewV3 {
    pub phase: String,
    pub quality_percent: u32,
    pub diversity_percent: u32,
    pub frames_seen: u32,
    pub frames_accepted: u32,
    pub frames_rejected: u32,
    pub quality_bucket: u8,
    pub guidance: GuidanceView,
}

impl EnrollmentViewV3 {
    pub fn from_progress(p: &crate::client::EnrollmentProgressV3) -> Self {
        Self {
            phase: p.phase.clone(),
            quality_percent: p.quality_percent,
            diversity_percent: p.diversity_percent,
            frames_seen: p.frames_seen,
            frames_accepted: p.frames_accepted,
            frames_rejected: p.frames_rejected,
            quality_bucket: p.quality_bucket,
            guidance: GuidanceView::from_wire(&p.guidance),
        }
    }

    /// Share of observed captures the daemon kept, as a percentage. `None`
    /// when nothing has been seen yet, so the view can say "waiting" instead
    /// of showing a misleading 0%.
    pub fn accept_rate_percent(&self) -> Option<u32> {
        if self.frames_seen == 0 {
            return None;
        }
        Some(((u64::from(self.frames_accepted) * 100) / u64::from(self.frames_seen)) as u32)
    }

    /// True once the daemon has actually looked at a frame.
    ///
    /// Before that there is no measurement, and the view must say so rather
    /// than render the zeros it was seeded with. `start_enrollment_v3` fills
    /// the view with literal `0`s and `quality_bucket: 0` so the page has
    /// something to draw, and bucket 0 is "Poor" — so the scan opened by
    /// announcing "0% — Poor" and tinting the silhouette amber before the
    /// camera had returned anything. `accept_rate_percent` already had the
    /// right shape for this; quality and diversity did not.
    pub fn has_observation(&self) -> bool {
        self.frames_seen > 0
    }

    /// The quality percentage and the daemon's bucket for it, or `None` when
    /// nothing has been measured. The bucket travels with the percentage so
    /// the tint and the number can never be taken from different frames.
    pub fn measured_quality(&self) -> Option<(u32, u8)> {
        self.has_observation()
            .then_some((self.quality_percent, self.quality_bucket))
    }

    /// The diversity percentage, or `None` when nothing has been measured.
    pub fn measured_diversity_percent(&self) -> Option<u32> {
        self.has_observation().then_some(self.diversity_percent)
    }
}

/// Render a Unix timestamp as a coarse, relative "last used" line.
///
/// The profile list rendered `format!("epoch {}", row.last_used)`, so a user
/// saw "last used: epoch 1726550400" — while `ProfileRowView`'s own doc
/// promised "a localized 'last used' line".
///
/// Deliberately coarse. An exact clock time is not what the question "when
/// did I last use this?" wants, and a relative phrase needs no locale data,
/// which matters because this crate's `tr()` has no catalog behind it yet.
pub fn last_used_label(last_used: u64, now: u64) -> String {
    if last_used == 0 {
        return crate::tr("never");
    }
    // A timestamp in the future means a clock change, not a use. Say the
    // honest thing rather than "in -3 days".
    let Some(delta) = now.checked_sub(last_used) else {
        return crate::tr("just now");
    };
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;
    const MONTH: u64 = 30 * DAY;
    const YEAR: u64 = 365 * DAY;

    let plural = |n: u64, one: &str, many: &str| {
        if n == 1 {
            crate::tr(one)
        } else {
            format!("{n} {}", crate::tr(many))
        }
    };
    match delta {
        d if d < MINUTE => crate::tr("just now"),
        d if d < HOUR => plural(d / MINUTE, "a minute ago", "minutes ago"),
        d if d < DAY => plural(d / HOUR, "an hour ago", "hours ago"),
        d if d < WEEK => plural(d / DAY, "yesterday", "days ago"),
        d if d < MONTH => plural(d / WEEK, "last week", "weeks ago"),
        d if d < YEAR => plural(d / MONTH, "last month", "months ago"),
        d => plural(d / YEAR, "last year", "years ago"),
    }
}

/// [`last_used_label`] against the current clock.
pub fn last_used_label_now(last_used: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    last_used_label(last_used, now)
}

/// Bucket boundaries, mirrored from the daemon's `observation` module so the
/// word beside a percentage means the same thing on both sides of the wire.
/// A drift test in `enrollment_guidance.rs` pins them together.
pub const QUALITY_LOW_MAX: u32 = 33;
pub const QUALITY_MEDIUM_MAX: u32 = 66;

/// Map a 0-100 quality percentage to the coarse bucket the UI tints and
/// labels with. Same boundaries as the daemon's `QualityBucket::from_percent`.
pub fn quality_bucket_from_percent(percent: u32) -> u8 {
    if percent <= QUALITY_LOW_MAX {
        0
    } else if percent <= QUALITY_MEDIUM_MAX {
        1
    } else {
        2
    }
}

/// v3 review-page view. Renders the stopped enrollment summary.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct ReviewViewV3 {
    pub ready_to_save: bool,
    pub quality_percent: u32,
    pub diversity_percent: u32,
    pub frames_accepted: u32,
    pub guidance: GuidanceView,
}

impl ReviewViewV3 {
    pub fn from_review(r: &crate::client::EnrollmentReviewV3) -> Self {
        Self {
            ready_to_save: r.ready_to_save,
            quality_percent: r.quality_percent,
            diversity_percent: r.diversity_percent,
            frames_accepted: r.frames_accepted,
            guidance: GuidanceView::from_wire(&r.guidance),
        }
    }
}
