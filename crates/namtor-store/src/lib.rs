// SPDX-License-Identifier: GPL-3.0-only
//! Template storage: TPM2-sealed AEAD containers for face embedding templates.
//!
//! Templates are unrotatable biometric data. They are encrypted at rest with
//! AES-256-GCM, the key sealed to TPM2 (or a fallback root-only key file when
//! no TPM is available). AAD binds uid + model_id to prevent template transplant.
//!
//! Spec §7: "TPM2-sealed with graceful fallback. Templates are unrotatable
//! biometric data, and offline disk theft is the primary threat."

pub mod crypto;
pub mod store;
pub mod template;

#[cfg(feature = "tpm2")]
pub use crypto::Tpm2Backend;
pub use crypto::{BackendSelector, FileBackend, KeyBackend, KeySource, MockTpm, SealedKey};
pub use store::{StoreError, TemplateStore};
pub use template::{EnrollmentMeta, Template, TemplateRecord};
