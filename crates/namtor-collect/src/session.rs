// SPDX-License-Identifier: GPL-3.0-only
//! Session metadata and configuration for data collection.

use serde::{Deserialize, Serialize};

/// Whether a session is a live face (bonafide) or an attack presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Bonafide,
    Print,
    ScreenReplay,
    CutoutMask,
}

/// Per-session metadata (spec §10.4: per-session identifiers for withdrawal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    pub contributor_id: String,
    pub kind: SessionKind,
    pub started_at_us: u64,
    pub device: String,
    pub ambient_notes: Option<String>,
}

/// Configuration for a collection session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub contributor_id: String,
    pub kind: SessionKind,
    pub num_pairs: u32,
    pub device: String,
}

impl SessionConfig {
    pub fn new(contributor_id: &str, kind: SessionKind, num_pairs: u32) -> Self {
        Self {
            contributor_id: contributor_id.into(),
            kind,
            num_pairs,
            device: "sonix_3277_0018".into(),
        }
    }
}

/// A capture session that collects frame pairs and computes differentials.
pub struct CaptureSession {
    config: SessionConfig,
    session_id: String,
    started_at_us: u64,
}

impl CaptureSession {
    pub fn new(config: SessionConfig) -> Self {
        let session_id = format!(
            "{}_{:x}",
            config.contributor_id,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros() as u64)
                .unwrap_or(0)
        );
        let started_at_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        Self {
            config,
            session_id,
            started_at_us,
        }
    }

    pub fn meta(&self) -> SessionMeta {
        SessionMeta {
            session_id: self.session_id.clone(),
            contributor_id: self.config.contributor_id.clone(),
            kind: self.config.kind,
            started_at_us: self.started_at_us,
            device: self.config.device.clone(),
            ambient_notes: None,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn num_pairs(&self) -> u32 {
        self.config.num_pairs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_is_unique() {
        let cfg1 = SessionConfig::new("alice", SessionKind::Bonafide, 10);
        let cfg2 = SessionConfig::new("alice", SessionKind::Bonafide, 10);
        let s1 = CaptureSession::new(cfg1);
        std::thread::sleep(std::time::Duration::from_micros(100));
        let s2 = CaptureSession::new(cfg2);
        assert_ne!(s1.session_id(), s2.session_id());
    }

    #[test]
    fn meta_carries_kind_and_contributor() {
        let cfg = SessionConfig::new("bob", SessionKind::Print, 5);
        let session = CaptureSession::new(cfg);
        let meta = session.meta();
        assert_eq!(meta.contributor_id, "bob");
        assert_eq!(meta.kind, SessionKind::Print);
    }

    #[test]
    fn session_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&SessionKind::Bonafide).unwrap(),
            r#""bonafide""#
        );
        assert_eq!(
            serde_json::to_string(&SessionKind::ScreenReplay).unwrap(),
            r#""screen_replay""#
        );
    }
}
