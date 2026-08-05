// SPDX-License-Identifier: GPL-3.0-only
//! Consent-gated PAD data collection (spec §10.4).
//!
//! `namtor-collect` records labelled bonafide and attack sessions with
//! strobe-differential frame pairs for MiniFASNet PAD training (§10.3).
//!
//! Rules (spec §10.4):
//! - Local-only by default. Nothing leaves the machine without explicit upload.
//! - Explicit informed consent at collection time — not a checkbox in a licence.
//! - Prefer derived features and differential statistics over raw face images
//!   wherever they suffice; most PAD signal survives the reduction.
//! - Per-session identifiers, so withdrawal and deletion are possible.
//! - Contributor-facing documentation explains exactly what is stored and why.

pub mod consent;
pub mod session;
pub mod store;

pub use consent::{ConsentRecord, ConsentState};
pub use session::{CaptureSession, SessionConfig, SessionKind, SessionMeta};
pub use store::{CollectionStore, StoreError};
