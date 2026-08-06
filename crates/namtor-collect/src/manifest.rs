// SPDX-License-Identifier: GPL-3.0-only
//! Retention and export manifests (spec §10.4).
//!
//! Manifests describe *what would leave the machine* if the contributor
//! chose to export. They never contain raw frame bytes — only derived
//! differential statistics and per-session summaries. Raw image export is
//! gated separately by [`crate::research::ResearchCapture`].

use serde::{Deserialize, Serialize};

use crate::consent::RetentionPolicy;
use crate::session::{SessionKind, SessionMeta};
use crate::store::DiffStats;

/// Summary of one session as it appears in a retention manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionManifest {
    pub session_id: String,
    pub contributor_id: String,
    pub kind: SessionKind,
    pub pair_count: u32,
    /// Number of raw frame pairs still on disk for this session. Always 0
    /// in the product path; only [`crate::research::ResearchCapture`] ever
    /// writes raw pairs, and it writes them to a separate research tree.
    pub raw_pair_count: u32,
    pub withdrawn: bool,
}

impl SessionManifest {
    pub fn from_meta(meta: &SessionMeta, pair_count: u32, raw_pair_count: u32) -> Self {
        Self {
            session_id: meta.session_id.clone(),
            contributor_id: meta.contributor_id.clone(),
            kind: meta.kind,
            pair_count,
            raw_pair_count,
            withdrawn: meta.withdrawn_at_us.is_some(),
        }
    }
}

/// A full export of a single session: metadata plus all derived differential
/// statistics. Raw frames are never embedded here — they live in the
/// research tree and only ship under [`crate::research::ResearchCapture`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionManifest {
    pub session: SessionManifest,
    pub derived: Vec<DiffStats>,
    /// `true` only if raw pairs exist on disk for this session *and* the
    /// contributor's retention policy permits raw export. The product path
    /// always reports `false`.
    pub raw_eligible: bool,
}

/// Aggregate retention state across all sessions in the store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionManifest {
    pub sessions: Vec<SessionManifest>,
    pub total_raw_pairs: u64,
    /// `true` only if every contributor has consented to at least
    /// [`RetentionPolicy::DerivedExport`] and no session is withdrawn.
    /// A `false` value means the store is not in a state that may be
    /// exported as a dataset without further action.
    pub export_eligible: bool,
}

/// Whether `policy` permits derived statistics to leave the machine.
pub fn permits_derived_export(policy: RetentionPolicy) -> bool {
    matches!(
        policy,
        RetentionPolicy::DerivedExport | RetentionPolicy::FullExport
    )
}

/// Whether `policy` permits *raw* frames to leave the machine. Raw export
/// always requires a second authorisation (the `NAMTOR_RESEARCH_CAPTURE`
/// env var) on top of this policy.
pub fn permits_raw_export(policy: RetentionPolicy) -> bool {
    matches!(policy, RetentionPolicy::FullExport)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_manifest_marks_withdrawn() {
        let mut meta = SessionMeta {
            session_id: "s1".into(),
            contributor_id: "alice".into(),
            kind: SessionKind::Bonafide,
            started_at_us: 0,
            device: "test".into(),
            ambient_notes: None,
            metadata: crate::session::CollectionMetadata::default(),
            withdrawn_at_us: Some(123),
        };
        let m = SessionManifest::from_meta(&meta, 5, 0);
        assert!(m.withdrawn);
        assert_eq!(m.raw_pair_count, 0);
        meta.withdrawn_at_us = None;
        let m2 = SessionManifest::from_meta(&meta, 5, 0);
        assert!(!m2.withdrawn);
    }

    #[test]
    fn permits_derived_export_table() {
        assert!(!permits_derived_export(RetentionPolicy::LocalOnly));
        assert!(permits_derived_export(RetentionPolicy::DerivedExport));
        assert!(permits_derived_export(RetentionPolicy::FullExport));
    }

    #[test]
    fn permits_raw_export_table() {
        assert!(!permits_raw_export(RetentionPolicy::LocalOnly));
        assert!(!permits_raw_export(RetentionPolicy::DerivedExport));
        assert!(permits_raw_export(RetentionPolicy::FullExport));
    }
}
