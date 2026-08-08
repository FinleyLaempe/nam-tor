// SPDX-License-Identifier: GPL-3.0-only
//! View model for the Nam-tor GNOME management UI.
//!
//! This module provides a pure-data representation of the UI state.
//! The `ProfilePresenter` transforms `ManagerClient` responses into these
//! views, which the GTK shell then renders.

use crate::client::{
    EnrollmentProgress, PamServiceCompatibility, PamServiceInfo, ProtectionKeyKind,
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
pub struct PresenterModel {
    pub profiles: Vec<ProfileRowView>,
    pub status: Option<StatusReport>,
    /// v2: template storage protection status (coarse, privacy-safe view).
    pub protection: Option<ProtectionView>,
    /// v2: system readiness indicators (daemon, model, camera).
    pub readiness: Option<ReadinessView>,
    /// v2: detected PAM auth services.
    pub pam_services: Vec<PamServiceRowView>,
    pub enrollment: Option<EnrollmentView>,
    pub review: Option<ReviewView>,
    pub pending_merge: Option<PendingMerge>,
    pub pending_replace: Option<PendingReplace>,
    /// v2: staged PAM toggle awaiting high-impact confirmation.
    pub pending_pam_toggle: Option<PendingPamToggle>,
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
