// SPDX-License-Identifier: GPL-3.0-only
//! Polkit authorization error handling for the Nam-tor UI.
//!
//! USA-11: distinguishes bus/check errors from denied/cancelled errors so
//! the user sees a meaningful message instead of a raw D-Bus error string.

use crate::client::ManagerError;

/// The category of polkit authorization outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolkitAuthError {
    /// The polkit service (or the D-Bus daemon itself) is unavailable.
    Bus,
    /// The authorization check failed for a non-denial reason.
    Check,
    /// The user authenticated but the action was denied by policy.
    Denied(String),
    /// The user dismissed the authentication dialog.
    Cancelled,
}

impl PolkitAuthError {
    /// Map a [`ManagerError`] to a polkit auth error when it represents an
    /// authorization failure. Returns `None` for errors that are not
    /// polkit-related.
    pub fn from_manager_error(err: &ManagerError) -> Option<Self> {
        match err {
            ManagerError::ServiceNotRegistered | ManagerError::Transport(_) => Some(Self::Bus),
            ManagerError::Denied(detail) => {
                if detail.to_lowercase().contains("cancel")
                    || detail.to_lowercase().contains("dismiss")
                {
                    Some(Self::Cancelled)
                } else {
                    Some(Self::Denied(detail.clone()))
                }
            }
            ManagerError::Challenge(_) => Some(Self::Check),
            _ => None,
        }
    }

    /// USA-11: a user-facing message that distinguishes the error type.
    pub fn user_message(&self) -> &str {
        match self {
            Self::Bus => "Polkit service unavailable",
            Self::Check => "Authorization check failed",
            Self::Cancelled => "Authentication dismissed",
            Self::Denied(_) => "Authentication denied",
        }
    }
}
