// SPDX-License-Identifier: GPL-3.0-only
//! Template and enrollment record types.

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::crypto::{lock_slice, unlock_slice};

/// A single face embedding template (L2-normalised vector).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    /// L2-normalised embedding vector (e.g. 128-d for SFace).
    pub vector: Vec<f32>,
    /// Quality score from enrollment (0.0–1.0).
    pub quality: f32,
    /// Whether this is an anchor (original enrollment) template.
    pub is_anchor: bool,
}

impl Template {
    /// Best-effort `mlock` + `MADV_DONTDUMP` on the embedding bytes.
    pub fn lock_memory(&mut self) {
        // SAFETY: f32 has no internal padding relative to itself, so the
        // Vec's byte buffer is a contiguous, properly-aligned region.
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                self.vector.as_ptr().cast(),
                self.vector.len() * std::mem::size_of::<f32>(),
            )
        };
        lock_slice(bytes);
    }

    fn unlock_memory(&mut self) {
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                self.vector.as_ptr().cast(),
                self.vector.len() * std::mem::size_of::<f32>(),
            )
        };
        unlock_slice(bytes);
    }
}

impl Drop for Template {
    fn drop(&mut self) {
        self.unlock_memory();
        self.vector.zeroize();
    }
}

/// Per-user enrollment metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentMeta {
    pub uid: u32,
    pub username: String,
    pub model_id: String,
    pub model_quant: namtor_proto::worker::Quant,
    pub created_at_us: u64,
    pub num_templates: u32,
    /// Normalisation parameters (z-norm): mean and std of impostor scores.
    pub znorm_mean: Option<f32>,
    pub znorm_std: Option<f32>,
}

/// The full on-disk record: metadata + templates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateRecord {
    pub meta: EnrollmentMeta,
    pub templates: Vec<Template>,
}

impl TemplateRecord {
    /// Best-effort `mlock` + `MADV_DONTDUMP` on every template's embedding.
    pub fn lock_memory(&mut self) {
        for t in &mut self.templates {
            t.lock_memory();
        }
    }
}

/// Identifier of a named face profile within a per-user container.
///
/// The default profile id (`"default"`) is used when migrating a legacy
/// single-record container to the multi-profile schema.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileId(pub String);

impl Default for ProfileId {
    fn default() -> Self {
        Self("default".into())
    }
}

/// Metadata for a single named face profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileRecord {
    pub profile_id: ProfileId,
    pub display_name: String,
    pub enabled: bool,
    pub uid: u32,
    pub model_id: String,
    pub created_at_us: u64,
    pub schema_version: u16,
    /// Wall-clock microseconds of the last successful authentication using
    /// this profile. Zero means "never used" (enrolled but not yet matched
    /// against). Updated by the daemon's verify path; surfaced to the UI as
    /// `ProfileInfo.last_used` (seconds).
    #[serde(default)]
    pub last_used_us: u64,
}

/// A profile plus the templates enrolled under it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub profile: ProfileRecord,
    pub templates: Vec<Template>,
}

/// On-disk container holding every profile for a single uid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserContainer {
    pub schema_version: u16,
    pub profiles: Vec<ProfileEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_round_trip() {
        let t = Template {
            vector: vec![0.1, 0.2, 0.3],
            quality: 0.85,
            is_anchor: true,
        };
        let json = serde_json::to_string(&t).unwrap();
        let t2: Template = serde_json::from_str(&json).unwrap();
        assert_eq!(t.vector, t2.vector);
        assert_eq!(t.quality, t2.quality);
        assert!(t2.is_anchor);
    }

    #[test]
    fn record_round_trip() {
        let r = TemplateRecord {
            meta: EnrollmentMeta {
                uid: 1000,
                username: "test".into(),
                model_id: "sface".into(),
                model_quant: namtor_proto::worker::Quant::Fp32,
                created_at_us: 1234567890,
                num_templates: 8,
                znorm_mean: Some(0.3),
                znorm_std: Some(0.1),
            },
            templates: vec![Template {
                vector: vec![1.0; 128],
                quality: 0.9,
                is_anchor: true,
            }],
        };
        let json = serde_json::to_string(&r).unwrap();
        let r2: TemplateRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(r.meta.uid, r2.meta.uid);
        assert_eq!(r.templates.len(), r2.templates.len());
        assert_eq!(r.templates[0].vector.len(), 128);
    }
}
