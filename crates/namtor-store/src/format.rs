// SPDX-License-Identifier: GPL-3.0-only
//! On-disk framing for the template store.
//!
//! Two formats are supported:
//! - `NMT1` (pre-task-12): `magic(4) + nonce(12) + model_id_len(4) +
//!   model_id + ciphertext`. AAD binds `uid + model_id`. No version field,
//!   no profile_id. Read-only.
//! - `NMT2` (task 12+): `magic(4) + version(2) + profile_id_len(2) +
//!   profile_id + nonce(12) + model_id_len(4) + model_id + ciphertext`.
//!   AAD binds `uid + profile_id + model_id`. The `version` field supports
//!   future migrations.
//!
//! AAD is version-tagged so a v1 record and a v2 record with the same
//! uid/model_id/profile_id produce different AAD and cannot be swapped.

use std::path::PathBuf;

use crate::store::StoreError;

pub(crate) const FILE_MAGIC_V1: &[u8; 4] = b"NMT1";
pub(crate) const FILE_MAGIC_V2: &[u8; 4] = b"NMT2";
pub(crate) const FILE_VERSION_CURRENT: u16 = 2;

/// AAD for the legacy `NMT1` format: binds `uid + model_id`.
pub(crate) fn aad_v1(uid: u32, model_id: &str) -> Vec<u8> {
    let mut aad = Vec::new();
    aad.extend_from_slice(b"namtor-v1:");
    aad.extend_from_slice(&uid.to_le_bytes());
    aad.push(b':');
    aad.extend_from_slice(model_id.as_bytes());
    aad
}

/// AAD for the `NMT2` format: binds `uid + profile_id + model_id`.
pub(crate) fn aad_v2(uid: u32, profile_id: &str, model_id: &str) -> Vec<u8> {
    let mut aad = Vec::new();
    aad.extend_from_slice(b"namtor-v2:");
    aad.extend_from_slice(&uid.to_le_bytes());
    aad.push(b':');
    aad.extend_from_slice(profile_id.as_bytes());
    aad.push(b':');
    aad.extend_from_slice(model_id.as_bytes());
    aad
}

/// The decrypted-frame fields needed to AEAD-decrypt the ciphertext.
pub(crate) struct Decoded {
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub aad: Vec<u8>,
}

/// Encode a record as `NMT2` (the only format the writer produces).
pub(crate) fn encode_v2(
    nonce: &[u8; 12],
    profile_id: &str,
    model_id: &str,
    ciphertext: &[u8],
) -> Vec<u8> {
    let model_id_bytes = model_id.as_bytes();
    let profile_id_bytes = profile_id.as_bytes();
    let mut out = Vec::with_capacity(
        4 + 2 + 2 + profile_id_bytes.len() + 12 + 4 + model_id_bytes.len() + ciphertext.len(),
    );
    out.extend_from_slice(FILE_MAGIC_V2);
    out.extend_from_slice(&FILE_VERSION_CURRENT.to_le_bytes());
    out.extend_from_slice(&(profile_id_bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(profile_id_bytes);
    out.extend_from_slice(nonce);
    out.extend_from_slice(&(model_id_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(model_id_bytes);
    out.extend_from_slice(ciphertext);
    out
}

/// Decode either `NMT1` or `NMT2`, returning the frame fields plus the AAD
/// the reader must use to decrypt.
pub(crate) fn decode(raw: &[u8], uid: u32) -> Result<Decoded, StoreError> {
    if raw.len() < 4 {
        return Err(StoreError::Crypto("file too short for magic".into()));
    }
    if &raw[..4] == FILE_MAGIC_V2 {
        decode_v2(raw, uid)
    } else if &raw[..4] == FILE_MAGIC_V1 {
        decode_v1(raw, uid)
    } else {
        Err(StoreError::Crypto("invalid file magic".into()))
    }
}

fn decode_v2(raw: &[u8], uid: u32) -> Result<Decoded, StoreError> {
    // magic(4) + version(2) + profile_id_len(2) + profile_id +
    // nonce(12) + model_id_len(4) + model_id + ciphertext.
    if raw.len() < 4 + 2 + 2 + 12 + 4 {
        return Err(StoreError::Crypto("NMT2 file truncated header".into()));
    }
    let mut off = 4;
    let version = u16::from_le_bytes([raw[off], raw[off + 1]]);
    off += 2;
    if version != FILE_VERSION_CURRENT {
        return Err(StoreError::SchemaVersion(version));
    }
    let profile_id_len = u16::from_le_bytes([raw[off], raw[off + 1]]) as usize;
    off += 2;
    if raw.len() < off + profile_id_len + 12 + 4 {
        return Err(StoreError::Crypto("NMT2 truncated profile_id".into()));
    }
    let profile_id = std::str::from_utf8(&raw[off..off + profile_id_len])
        .map_err(|e| StoreError::Crypto(format!("profile_id utf8: {e}")))?
        .to_string();
    off += profile_id_len;
    let nonce = raw[off..off + 12].to_vec();
    off += 12;
    let model_id_len =
        u32::from_le_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]) as usize;
    off += 4;
    if raw.len() < off + model_id_len {
        return Err(StoreError::Crypto("NMT2 truncated model_id".into()));
    }
    let model_id = std::str::from_utf8(&raw[off..off + model_id_len])
        .map_err(|e| StoreError::Crypto(format!("model_id utf8: {e}")))?
        .to_string();
    off += model_id_len;
    let ciphertext = raw[off..].to_vec();
    let aad = aad_v2(uid, &profile_id, &model_id);
    Ok(Decoded {
        nonce,
        ciphertext,
        aad,
    })
}

fn decode_v1(raw: &[u8], uid: u32) -> Result<Decoded, StoreError> {
    // magic(4) + nonce(12) + model_id_len(4) + model_id + ciphertext.
    if raw.len() < 4 + 12 + 4 {
        return Err(StoreError::Crypto("NMT1 file truncated header".into()));
    }
    let nonce = raw[4..16].to_vec();
    let model_id_len = u32::from_le_bytes([raw[16], raw[17], raw[18], raw[19]]) as usize;
    if raw.len() < 20 + model_id_len {
        return Err(StoreError::Crypto("NMT1 truncated model_id".into()));
    }
    let model_id = std::str::from_utf8(&raw[20..20 + model_id_len])
        .map_err(|e| StoreError::Crypto(format!("model_id utf8: {e}")))?
        .to_string();
    let ciphertext = raw[20 + model_id_len..].to_vec();
    let aad = aad_v1(uid, &model_id);
    Ok(Decoded {
        nonce,
        ciphertext,
        aad,
    })
}

#[allow(dead_code)]
fn _unused_pathbuf() {
    let _ = PathBuf::new();
}
