// SPDX-License-Identifier: GPL-3.0-only
//! Consent flow (spec §10.4).
//!
//! Explicit informed consent is required at collection time. This module
//! records the consent decision, timestamp, scope, the versioned purpose it
//! was granted for, and the retention policy the contributor chose. Consent
//! is revocable; revocation deletes all sessions associated with the
//! contributor. Withdrawal of a single session is handled by the store.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsentState {
    Granted,
    Revoked,
}

/// What the contributor authorises the project to do with their data
/// (spec §10.4: "Local-only by default. Nothing leaves the machine without
/// explicit upload."). Raw image export always requires a *separate* action
/// on top of [`RetentionPolicy::FullExport`] — see [`crate::research`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionPolicy {
    /// Default. Derived stats stay on the machine; nothing is uploaded.
    #[default]
    LocalOnly,
    /// Contributor permits derived differential statistics to leave the
    /// machine. Raw frames are never included.
    DerivedExport,
    /// Contributor permits raw + derived export *if* a separate raw-research
    /// authorisation is also in place. Local-only until that second action.
    FullExport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentRecord {
    pub contributor_id: String,
    pub state: ConsentState,
    pub granted_at_us: u64,
    pub revoked_at_us: Option<u64>,
    pub scope: String,
    /// Version of the data-use purpose the contributor signed up for.
    /// Bumping this invalidates older consents: a record granted for
    /// purpose v1 does not cover collection under purpose v2.
    pub purpose_version: u32,
    /// What may leave the machine. Defaults to [`RetentionPolicy::LocalOnly`].
    pub retention_policy: RetentionPolicy,
}

impl ConsentRecord {
    /// Grant consent for `scope` under purpose version 1 and the default
    /// (local-only) retention policy.
    pub fn grant(contributor_id: &str, scope: &str) -> Self {
        Self::grant_versioned(contributor_id, scope, 1, RetentionPolicy::LocalOnly)
    }

    /// Grant consent with an explicit purpose version and retention policy.
    pub fn grant_versioned(
        contributor_id: &str,
        scope: &str,
        purpose_version: u32,
        retention_policy: RetentionPolicy,
    ) -> Self {
        Self {
            contributor_id: contributor_id.into(),
            state: ConsentState::Granted,
            granted_at_us: now_us(),
            revoked_at_us: None,
            scope: scope.into(),
            purpose_version,
            retention_policy,
        }
    }

    pub fn revoke(&mut self) {
        self.state = ConsentState::Revoked;
        self.revoked_at_us = Some(now_us());
    }

    pub fn is_active(&self) -> bool {
        self.state == ConsentState::Granted
    }
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_creates_active_consent() {
        let r = ConsentRecord::grant("user1", "pad-training");
        assert_eq!(r.state, ConsentState::Granted);
        assert!(r.is_active());
        assert_eq!(r.scope, "pad-training");
        assert_eq!(r.purpose_version, 1);
        assert_eq!(r.retention_policy, RetentionPolicy::LocalOnly);
    }

    #[test]
    fn revoke_makes_inactive() {
        let mut r = ConsentRecord::grant("user1", "pad-training");
        r.revoke();
        assert_eq!(r.state, ConsentState::Revoked);
        assert!(!r.is_active());
        assert!(r.revoked_at_us.is_some());
    }

    #[test]
    fn consent_round_trip() {
        let r = ConsentRecord::grant("alice", "research");
        let json = serde_json::to_string(&r).unwrap();
        let back: ConsentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.contributor_id, "alice");
        assert!(back.is_active());
        assert_eq!(back.purpose_version, 1);
    }

    #[test]
    fn versioned_grant_records_policy_and_version() {
        let r =
            ConsentRecord::grant_versioned("alice", "research", 3, RetentionPolicy::DerivedExport);
        assert_eq!(r.purpose_version, 3);
        assert_eq!(r.retention_policy, RetentionPolicy::DerivedExport);
    }

    #[test]
    fn retention_policy_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&RetentionPolicy::LocalOnly).unwrap(),
            r#""local_only""#
        );
        assert_eq!(
            serde_json::to_string(&RetentionPolicy::FullExport).unwrap(),
            r#""full_export""#
        );
    }
}
