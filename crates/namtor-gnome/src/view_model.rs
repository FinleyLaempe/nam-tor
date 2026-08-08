// SPDX-License-Identifier: GPL-3.0-only
//! View model for the Nam-tor GNOME management UI.
//!
//! This module provides a pure-data representation of the UI state.
//! The `ProfilePresenter` transforms `ManagerClient` responses into these
//! views, which the GTK shell then renders.

use crate::client::{EnrollmentProgress, QualityLabel, StatusInfo};

/// Coarse quality bucket the user sees, plus a guidance flag the view renders
/// as an accessibility-friendly alert when diversity is low.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRowView {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub created: u64,
    pub last_used: Option<u64>,
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
                    "Start it with: systemctl --user start namtor-daemon".into(),
                    format!("Detail: {detail}"),
                ],
            }),
            crate::client::ManagerError::ServiceNotRegistered => Some(Self {
                title: "Nam-tor service is not registered on the session bus".into(),
                steps: vec![
                    "Install the namtor-daemon D-Bus service file".into(),
                    "Then run: systemctl --user start namtor-daemon".into(),
                ],
            }),
            crate::client::ManagerError::PolkitDenied(detail) => Some(Self {
                title: "Administrator approval required".into(),
                steps: vec![format!("Polkit denied: {detail}")],
            }),
            _ => None,
        }
    }
}

/// View model rendered by the GTK window. Every field is plain data the view
/// can serialize into Atk roles and gettext strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenterModel {
    pub profiles: Vec<ProfileRowView>,
    pub status: Option<StatusInfo>,
    pub enrollment: Option<EnrollmentView>,
    pub pending_merge: Option<PendingMerge>,
    pub pending_replace: Option<PendingReplace>,
    pub error: Option<ErrorView>,
    pub message: Option<String>,
    pub is_loading: bool,
    pub reduced_protection_warning: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentView {
    pub session_id: String,
    pub frames_captured: u32,
    pub quality: QualityView,
    pub coverage_percent: u32,
}

impl EnrollmentView {
    pub fn from_progress(p: EnrollmentProgress) -> Self {
        let quality = QualityView::from_label(p.quality_label);
        Self {
            session_id: p.session_id,
            frames_captured: p.frames_captured,
            quality,
            coverage_percent: p.coverage_percent,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMerge {
    pub src_id: String,
    pub src_name: String,
    pub dst_id: String,
    pub dst_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReplace {
    pub id: String,
    pub old_name: String,
    pub new_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorView {
    pub title: String,
    pub detail: String,
    pub recovery: Option<RecoveryHint>,
}
