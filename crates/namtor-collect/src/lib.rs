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

/// Serialises the tests that touch `NAMTOR_RESEARCH_CAPTURE`.
///
/// That variable is the real authorisation gate for raw research capture, not
/// a test hook, so it cannot be replaced with a parameter. But six tests
/// across `research` and `store` were mutating it while the default test
/// runner ran them in parallel: `research::tests` took a private `TEST_LOCK`
/// and `store::tests` took nothing, so a `remove_var` from one landed between
/// another's `set_var` and its `authorize()` call and the test failed with
/// `ResearchNotAuthorised`. The two modules must share one lock, and the
/// guard restores the variable on drop so a panicking test cannot leave it
/// set for whatever runs next.
#[cfg(test)]
pub(crate) mod test_env {
    use std::sync::{Mutex, MutexGuard};

    static RESEARCH_ENV_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) struct ResearchEnv {
        _guard: MutexGuard<'static, ()>,
    }

    impl ResearchEnv {
        fn acquire() -> MutexGuard<'static, ()> {
            // A test that panics while holding the lock poisons it; the next
            // test should still run rather than fail for an unrelated reason.
            RESEARCH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
        }

        /// Hold the lock with raw capture authorised.
        pub(crate) fn authorised() -> Self {
            let guard = Self::acquire();
            std::env::set_var(crate::RESEARCH_ENV_VAR, "1");
            Self { _guard: guard }
        }

        /// Hold the lock with raw capture NOT authorised.
        pub(crate) fn unauthorised() -> Self {
            let guard = Self::acquire();
            std::env::remove_var(crate::RESEARCH_ENV_VAR);
            Self { _guard: guard }
        }
    }

    impl Drop for ResearchEnv {
        fn drop(&mut self) {
            std::env::remove_var(crate::RESEARCH_ENV_VAR);
        }
    }
}

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
