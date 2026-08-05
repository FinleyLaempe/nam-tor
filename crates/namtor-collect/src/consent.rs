// SPDX-License-Identifier: GPL-3.0-only
//! Consent flow (spec §10.4).
//!
//! Explicit informed consent is required at collection time. This module
//! records the consent decision, timestamp, and scope. Consent is revocable;
//! revocation deletes all sessions associated with the contributor.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsentState {
    Granted,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentRecord {
    pub contributor_id: String,
    pub state: ConsentState,
    pub granted_at_us: u64,
    pub revoked_at_us: Option<u64>,
    pub scope: String,
}

impl ConsentRecord {
    pub fn grant(contributor_id: &str, scope: &str) -> Self {
        Self {
            contributor_id: contributor_id.into(),
            state: ConsentState::Granted,
            granted_at_us: now_us(),
            revoked_at_us: None,
            scope: scope.into(),
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
    }
}
