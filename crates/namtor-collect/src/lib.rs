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
//!
//! Productization (Task 20):
//! - Consent carries a `purpose_version` and a `RetentionPolicy` (default
//!   `LocalOnly`).
//! - The product path stores **only** derived differential statistics — no
//!   raw face images. Raw capture is a separately authorised research path
//!   ([`research::ResearchCapture`], gated by `NAMTOR_RESEARCH_CAPTURE=1`).
//! - Sessions can be withdrawn (soft delete) or deleted (hard delete), and
//!   exported as a [`manifest::CollectionManifest`]. A
//!   [`manifest::RetentionManifest`] aggregates the whole store.

pub mod consent;
pub mod manifest;
pub mod research;
pub mod session;
pub mod store;

pub use consent::{ConsentRecord, ConsentState, RetentionPolicy};
pub use manifest::{
    permits_derived_export, permits_raw_export, CollectionManifest, RetentionManifest,
    SessionManifest,
};
pub use research::{raw_pair_count, ResearchCapture, RESEARCH_ENV_VAR};
pub use session::{CaptureSession, CollectionMetadata, SessionConfig, SessionKind, SessionMeta};
pub use store::{CollectionStore, DiffStats, StoreError};
