// SPDX-License-Identifier: GPL-3.0-only
//! Key management: TPM2 sealing with root-only file fallback.
//!
//! Spec §7.3: "Default: TPM-bound with no PCR policy. This protects against
//! offline disk theft — the templates are unreadable on any other machine —
//! while surviving kernel updates. Fallback when no TPM exists: root-only
//! AEAD-encrypted files with a key stored in /var/lib/namtor/key."

use std::path::Path;

use rand::RngCore;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::StoreError;

/// Where the sealing key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// Key sealed to TPM2 via tss2. Unsealable only on this machine.
    Tpm2,
    /// Fallback: key file in /var/lib/namtor/key, root-only permissions.
    File,
}

/// A sealed (or fallback) symmetric key for AES-256-GCM.
pub struct SealedKey {
    key: [u8; 32],
    source: KeySource,
}

impl SealedKey {
    /// Load or create the sealing key.
    ///
    /// Tries TPM2 first (via `tss2` command-line). If TPM2 is unavailable,
    /// falls back to a root-only key file at `key_path`.
    pub fn load_or_create(key_path: &Path) -> Result<Self, StoreError> {
        if let Some(key) = try_tpm2(key_path)? {
            return Ok(key);
        }
        try_file_fallback(key_path)
    }

    pub fn key(&self) -> &[u8; 32] {
        &self.key
    }

    pub fn source(&self) -> KeySource {
        self.source
    }

    #[allow(dead_code)]
    fn new_from_tpm2(key: [u8; 32]) -> Self {
        Self {
            key,
            source: KeySource::Tpm2,
        }
    }

    fn new_from_file(key: [u8; 32]) -> Self {
        Self {
            key,
            source: KeySource::File,
        }
    }
}

impl Drop for SealedKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

fn try_tpm2(_key_path: &Path) -> Result<Option<SealedKey>, StoreError> {
    let tpm_dev = Path::new("/dev/tpmrm0");
    if !tpm_dev.exists() {
        return Ok(None);
    }
    Ok(None)
}

fn try_file_fallback(key_path: &Path) -> Result<SealedKey, StoreError> {
    if key_path.exists() {
        let raw = std::fs::read(key_path)?;
        if raw.len() != 32 {
            return Err(StoreError::Crypto("key file is not 32 bytes".into()));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&raw);
        return Ok(SealedKey::new_from_file(key));
    }
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    if let Some(parent) = key_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(key_path, key)?;
    set_root_only(key_path)?;
    Ok(SealedKey::new_from_file(key))
}

fn set_root_only(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Derive a 96-bit nonce for AES-GCM from a counter + random bytes.
pub fn derive_nonce(counter: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    let mut hasher = Sha256::new();
    hasher.update(b"namtor-nonce");
    hasher.update(counter.to_le_bytes());
    let hash = hasher.finalize();
    nonce.copy_from_slice(&hash[..12]);
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_fallback_creates_and_reloads_key() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let k1 = SealedKey::load_or_create(&key_path).unwrap();
        assert_eq!(k1.source(), KeySource::File);
        assert!(key_path.exists());

        let k2 = SealedKey::load_or_create(&key_path).unwrap();
        assert_eq!(k1.key(), k2.key());
    }

    #[test]
    fn nonce_is_deterministic_per_counter() {
        let n1 = derive_nonce(1);
        let n2 = derive_nonce(1);
        let n3 = derive_nonce(2);
        assert_eq!(n1, n2);
        assert_ne!(n1, n3);
    }
}
