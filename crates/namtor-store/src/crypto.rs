// SPDX-License-Identifier: GPL-3.0-only
//! Key management: TPM2 sealing with root-only file fallback.
//!
//! Spec §7.3: "Default: TPM-bound with no PCR policy. This protects against
//! offline disk theft — the templates are unreadable on any other machine —
//! while surviving kernel updates. Fallback when no TPM exists: root-only
//! AEAD-encrypted files with a key stored in /var/lib/namtor/key."
//!
//! Three backends implement [`KeyBackend`]:
//! - [`MockTpm`] — software stand-in for tests; simulates device binding,
//!   corruption, and lockout. Used by default in `cargo test`.
//! - [`FileBackend`] — root-only key file. Always available. Reported as
//!   reduced protection when active (never silent).
//! - [`Tpm2Backend`] — real TPM2 via tss2 FAPI FFI. Feature-gated `tpm2`.
//!   Requires libtss2-fapi + `tss` group + `/dev/tpmrm0` at runtime.

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key as AesKey, Nonce};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::StoreError;

type HmacSha256 = Hmac<Sha256>;

const MAX_UNSEAL_ATTEMPTS: u32 = 5;
const BLOB_VERSION: u16 = 1;
const BLOB_MAGIC: &[u8; 6] = b"NMTKEY";

/// Where the sealing key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Tpm2,
    File,
}

/// A persisted, sealed symmetric key for AES-256-GCM.
///
/// On disk this is the [`SealedBlob`] bytes. In memory, [`SealedKey`] holds
/// the unsealed 32-byte key and zeroises it on drop.
#[derive(Debug)]
pub struct SealedKey {
    key: [u8; 32],
    source: KeySource,
}

impl SealedKey {
    pub fn load_or_create(key_path: &Path) -> Result<Self, StoreError> {
        Self::load_or_create_with(key_path, &mut BackendSelector::default())
    }

    pub fn load_or_create_with(
        key_path: &Path,
        selector: &mut BackendSelector,
    ) -> Result<Self, StoreError> {
        if key_path.exists() {
            let raw = std::fs::read(key_path)?;
            let blob = SealedBlob::decode(&raw)?;
            let backend = selector.open(blob.backend_id())?;
            let key = backend.unseal(&blob)?;
            let mut sk = Self::from_unsealed(key, backend.source());
            sk.lock_memory();
            return Ok(sk);
        }
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let mut backend = selector.select_for_new()?;
        let blob = backend.seal(&key)?;
        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(key_path, blob.encode())?;
        set_root_only(key_path)?;
        let mut sk = Self::from_unsealed(key, backend.source());
        sk.lock_memory();
        Ok(sk)
    }

    pub fn key(&self) -> &[u8; 32] {
        &self.key
    }

    pub fn source(&self) -> KeySource {
        self.source
    }

    /// Best-effort `mlock` + `MADV_DONTDUMP` on the key material.
    ///
    /// `mlock` requires `CAP_IPC_LOCK` or a generous `RLIMIT_MEMLOCK`; failures
    /// are silently ignored so the store still works in unprivileged tests and
    /// CI. The systemd unit grants `CAP_IPC_LOCK` to the daemon in production.
    pub fn lock_memory(&mut self) {
        lock_bytes(&self.key);
    }

    fn from_unsealed(key: [u8; 32], source: KeySource) -> Self {
        Self { key, source }
    }
}

impl Drop for SealedKey {
    fn drop(&mut self) {
        unlock_bytes(&self.key);
        self.key.zeroize();
    }
}

pub struct BackendSelector {
    force_backend: Option<&'static str>,
    mock_seed: Option<[u8; 32]>,
}

impl BackendSelector {
    pub fn new() -> Self {
        Self {
            force_backend: None,
            mock_seed: None,
        }
    }

    pub fn mock(seed: [u8; 32]) -> Self {
        Self {
            force_backend: Some(MockTpm::BACKEND_ID),
            mock_seed: Some(seed),
        }
    }

    pub fn file() -> Self {
        Self {
            force_backend: Some(FileBackend::BACKEND_ID),
            mock_seed: None,
        }
    }

    fn select_for_new(&mut self) -> Result<Box<dyn KeyBackend>, StoreError> {
        if let Some(id) = self.force_backend {
            return self.open(id);
        }
        #[cfg(feature = "tpm2")]
        {
            if Path::new("/dev/tpmrm0").exists() {
                if let Ok(b) = self.open(Tpm2Backend::BACKEND_ID) {
                    return Ok(b);
                }
            }
        }
        self.open(FileBackend::BACKEND_ID)
    }

    fn open(&self, id: &str) -> Result<Box<dyn KeyBackend>, StoreError> {
        match id {
            MockTpm::BACKEND_ID => {
                let seed = self.mock_seed.ok_or_else(|| {
                    StoreError::Tpm2("mock seed not configured for this selector".into())
                })?;
                Ok(Box::new(MockTpm::new(seed)))
            }
            FileBackend::BACKEND_ID => Ok(Box::new(FileBackend::new())),
            #[cfg(feature = "tpm2")]
            Tpm2Backend::BACKEND_ID => Ok(Box::new(Tpm2Backend::new()?)),
            _ => Err(StoreError::Crypto(format!("unknown backend id: {id}"))),
        }
    }
}

impl Default for BackendSelector {
    fn default() -> Self {
        Self::new()
    }
}

pub trait KeyBackend: Send {
    fn id(&self) -> &'static str;
    fn seal(&mut self, key: &[u8; 32]) -> Result<SealedBlob, StoreError>;
    fn unseal(&self, blob: &SealedBlob) -> Result<[u8; 32], StoreError>;
    fn source(&self) -> KeySource;
}

#[derive(Debug)]
pub struct SealedBlob {
    version: u16,
    backend_id: String,
    payload: Vec<u8>,
}

impl SealedBlob {
    pub(crate) fn new(backend_id: String, payload: Vec<u8>) -> Self {
        Self {
            version: BLOB_VERSION,
            backend_id,
            payload,
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            BLOB_MAGIC.len() + 2 + 1 + self.backend_id.len() + 4 + self.payload.len(),
        );
        out.extend_from_slice(BLOB_MAGIC);
        out.extend_from_slice(&self.version.to_le_bytes());
        out.push(self.backend_id.len() as u8);
        out.extend_from_slice(self.backend_id.as_bytes());
        out.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.payload);
        out
    }

    fn decode(raw: &[u8]) -> Result<Self, StoreError> {
        if raw.len() < BLOB_MAGIC.len() + 2 + 1 + 4 {
            return Err(StoreError::Crypto("sealed blob too short".into()));
        }
        if &raw[..BLOB_MAGIC.len()] != BLOB_MAGIC {
            return Err(StoreError::Crypto("sealed blob bad magic".into()));
        }
        let mut off = BLOB_MAGIC.len();
        let version = u16::from_le_bytes([raw[off], raw[off + 1]]);
        off += 2;
        if version != BLOB_VERSION {
            return Err(StoreError::Crypto(format!(
                "sealed blob version {version} unsupported"
            )));
        }
        let id_len = raw[off] as usize;
        off += 1;
        if raw.len() < off + id_len + 4 {
            return Err(StoreError::Crypto(
                "sealed blob truncated backend id".into(),
            ));
        }
        let backend_id = std::str::from_utf8(&raw[off..off + id_len])
            .map_err(|e| StoreError::Crypto(format!("sealed blob backend id utf8: {e}")))?
            .to_string();
        off += id_len;
        let payload_len =
            u32::from_le_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]) as usize;
        off += 4;
        if raw.len() < off + payload_len {
            return Err(StoreError::Crypto("sealed blob truncated payload".into()));
        }
        let payload = raw[off..off + payload_len].to_vec();
        Ok(Self {
            version,
            backend_id,
            payload,
        })
    }

    pub(crate) fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub(crate) fn backend_id(&self) -> &str {
        &self.backend_id
    }
}

// --- MockTpm: software stand-in that proves the KeyBackend contract ---

pub struct MockTpm {
    seed: [u8; 32],
    attempts: std::cell::Cell<u32>,
}

impl MockTpm {
    pub const BACKEND_ID: &'static str = "mock-tpm";

    pub fn new(seed: [u8; 32]) -> Self {
        Self {
            seed,
            attempts: std::cell::Cell::new(0),
        }
    }

    fn random_seed() -> [u8; 32] {
        let mut s = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut s);
        s
    }

    pub fn fresh() -> Self {
        Self::new(Self::random_seed())
    }

    fn wrap(&self, key: &[u8; 32]) -> Result<Vec<u8>, StoreError> {
        let cipher =
            Aes256Gcm::new_from_slice(&self.seed).map_err(|e| StoreError::Crypto(e.to_string()))?;
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = cipher
            .encrypt(nonce, key.as_ref())
            .map_err(|e| StoreError::Crypto(e.to_string()))?;
        let mut hmac = <HmacSha256 as Mac>::new_from_slice(&self.seed)
            .map_err(|e| StoreError::Crypto(e.to_string()))?;
        hmac.update(&nonce_bytes);
        hmac.update(&ct);
        let tag = hmac.finalize().into_bytes();
        let mut out = Vec::with_capacity(12 + ct.len() + tag.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        out.extend_from_slice(&tag);
        Ok(out)
    }

    fn unwrap(&self, payload: &[u8]) -> Result<[u8; 32], StoreError> {
        if payload.len() != 12 + 48 + 32 {
            return Err(StoreError::Crypto(
                "mock-tpm payload size mismatch (corruption)".into(),
            ));
        }
        let nonce_bytes = &payload[..12];
        let ct = &payload[12..60];
        let tag = &payload[60..92];

        let mut hmac = <HmacSha256 as Mac>::new_from_slice(&self.seed)
            .map_err(|e| StoreError::Crypto(e.to_string()))?;
        hmac.update(nonce_bytes);
        hmac.update(ct);
        hmac.verify_slice(tag)
            .map_err(|_| StoreError::Crypto("mock-tpm seal corrupted or wrong device".into()))?;

        let cipher =
            Aes256Gcm::new_from_slice(&self.seed).map_err(|e| StoreError::Crypto(e.to_string()))?;
        let nonce = Nonce::from_slice(nonce_bytes);
        let pt = cipher
            .decrypt(nonce, ct)
            .map_err(|_| StoreError::Crypto("mock-tpm seal wrong device".into()))?;
        if pt.len() != 32 {
            return Err(StoreError::Crypto(
                "mock-tpm unsealed key wrong length".into(),
            ));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&pt);
        Ok(key)
    }
}

impl KeyBackend for MockTpm {
    fn id(&self) -> &'static str {
        Self::BACKEND_ID
    }

    fn seal(&mut self, key: &[u8; 32]) -> Result<SealedBlob, StoreError> {
        let payload = self.wrap(key)?;
        Ok(SealedBlob::new(Self::BACKEND_ID.into(), payload))
    }

    fn unseal(&self, blob: &SealedBlob) -> Result<[u8; 32], StoreError> {
        let n = self.attempts.get().saturating_add(1);
        self.attempts.set(n);
        if n > MAX_UNSEAL_ATTEMPTS {
            return Err(StoreError::Tpm2(
                "mock-tpm lockout: too many unseal attempts".into(),
            ));
        }
        if blob.backend_id() != Self::BACKEND_ID {
            return Err(StoreError::Crypto("backend id mismatch".into()));
        }
        self.unwrap(blob.payload())
    }

    fn source(&self) -> KeySource {
        KeySource::Tpm2
    }
}

// --- FileBackend: root-only key file, always available, reduced protection ---

pub struct FileBackend {
    _priv: (),
}

impl FileBackend {
    pub const BACKEND_ID: &'static str = "file";

    pub fn new() -> Self {
        Self { _priv: () }
    }
}

impl Default for FileBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyBackend for FileBackend {
    fn id(&self) -> &'static str {
        Self::BACKEND_ID
    }

    fn seal(&mut self, key: &[u8; 32]) -> Result<SealedBlob, StoreError> {
        Ok(SealedBlob::new(Self::BACKEND_ID.into(), key.to_vec()))
    }

    fn unseal(&self, blob: &SealedBlob) -> Result<[u8; 32], StoreError> {
        if blob.backend_id() != Self::BACKEND_ID {
            return Err(StoreError::Crypto("backend id mismatch".into()));
        }
        let p = blob.payload();
        if p.len() != 32 {
            return Err(StoreError::Crypto(
                "file backend payload not 32 bytes".into(),
            ));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(p);
        Ok(key)
    }

    fn source(&self) -> KeySource {
        KeySource::File
    }
}

#[cfg(feature = "tpm2")]
#[path = "tpm2_backend.rs"]
mod tpm2_backend;

#[cfg(feature = "tpm2")]
pub use tpm2_backend::Tpm2Backend;

fn set_root_only(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

pub fn derive_nonce(counter: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    let mut hasher = Sha256::new();
    hasher.update(b"namtor-nonce");
    hasher.update(counter.to_le_bytes());
    let hash = hasher.finalize();
    nonce.copy_from_slice(&hash[..12]);
    nonce
}

/// Best-effort `mlock` + `MADV_DONTDUMP` on a contiguous byte region.
///
/// Used by [`SealedKey`] and [`crate::template::Template`] to keep
/// sensitive material out of swap and core dumps. Failures are silently
/// ignored: `mlock` requires `CAP_IPC_LOCK` (or a raised `RLIMIT_MEMLOCK`)
/// which is absent in CI and unprivileged tests; the systemd unit grants it
/// to the daemon in production. `MADV_DONTDUMP` is Linux-specific.
pub(crate) fn lock_bytes<T>(bytes: &T) {
    let ptr = (bytes as *const T).cast::<u8>() as *mut libc::c_void;
    let len = std::mem::size_of::<T>();
    unsafe {
        let _ = libc::mlock(ptr, len);
        #[cfg(target_os = "linux")]
        let _ = libc::madvise(ptr, len, libc::MADV_DONTDUMP);
    }
}

pub(crate) fn lock_slice(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let ptr = bytes.as_ptr() as *mut libc::c_void;
    unsafe {
        let _ = libc::mlock(ptr, bytes.len());
        #[cfg(target_os = "linux")]
        let _ = libc::madvise(ptr, bytes.len(), libc::MADV_DONTDUMP);
    }
}

pub(crate) fn unlock_slice(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let ptr = bytes.as_ptr() as *mut libc::c_void;
    unsafe {
        let _ = libc::munlock(ptr, bytes.len());
    }
}

pub(crate) fn unlock_bytes<T>(bytes: &T) {
    let ptr = (bytes as *const T).cast::<u8>() as *mut libc::c_void;
    let len = std::mem::size_of::<T>();
    unsafe {
        let _ = libc::munlock(ptr, len);
    }
}

#[allow(dead_code)]
fn _unused_links() {
    let _ = AesKey::<Aes256Gcm>::from_slice(&[0u8; 32]);
    let _ = PathBuf::new();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_seeds() -> ([u8; 32], [u8; 32]) {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut a);
        rand::thread_rng().fill_bytes(&mut b);
        assert_ne!(a, b);
        (a, b)
    }

    fn round_trip_key() -> [u8; 32] {
        let mut k = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut k);
        k
    }

    #[test]
    fn mock_seal_unseal_round_trip() {
        let seed = [7u8; 32];
        let mut tpm = MockTpm::new(seed);
        let key = round_trip_key();
        let blob = tpm.seal(&key).unwrap();
        assert_eq!(blob.backend_id(), MockTpm::BACKEND_ID);
        let recovered = tpm.unseal(&blob).unwrap();
        assert_eq!(recovered, key);
    }

    #[test]
    fn mock_restart_unseal_same_seed() {
        let seed = [9u8; 32];
        let key = round_trip_key();
        let blob = {
            let mut tpm = MockTpm::new(seed);
            tpm.seal(&key).unwrap()
        };
        let tpm2 = MockTpm::new(seed);
        let recovered = tpm2.unseal(&blob).unwrap();
        assert_eq!(recovered, key);
    }

    #[test]
    fn mock_migrate_reseal_new_seed_old_blob_invalid() {
        let old_seed = [1u8; 32];
        let new_seed = [2u8; 32];
        let key = round_trip_key();
        let old_blob = {
            let mut old = MockTpm::new(old_seed);
            old.seal(&key).unwrap()
        };
        let new_blob = {
            let mut new = MockTpm::new(new_seed);
            new.seal(&key).unwrap()
        };
        assert_ne!(old_blob.payload(), new_blob.payload());
        let new = MockTpm::new(new_seed);
        assert_eq!(new.unseal(&new_blob).unwrap(), key);
        let err = new.unseal(&old_blob).unwrap_err();
        assert!(matches!(err, StoreError::Crypto(_)));
    }

    #[test]
    fn mock_reject_moved_blob() {
        let (seed_a, seed_b) = two_seeds();
        let key = round_trip_key();
        let blob = {
            let mut a = MockTpm::new(seed_a);
            a.seal(&key).unwrap()
        };
        let b = MockTpm::new(seed_b);
        let err = b.unseal(&blob).unwrap_err();
        assert!(
            matches!(err, StoreError::Crypto(_)),
            "expected corruption/wrong-device error, got {err:?}"
        );
    }

    #[test]
    fn mock_corruption_detected() {
        let seed = [4u8; 32];
        let mut tpm = MockTpm::new(seed);
        let key = round_trip_key();
        let blob = tpm.seal(&key).unwrap();
        let mut tampered = blob.encode();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        let bad = SealedBlob::decode(&tampered).unwrap();
        let err = tpm.unseal(&bad).unwrap_err();
        assert!(
            matches!(err, StoreError::Crypto(_)),
            "expected corruption error, got {err:?}"
        );
    }

    #[test]
    fn mock_lockout_after_max_attempts() {
        let seed = [5u8; 32];
        let mut tpm = MockTpm::new(seed);
        let key = round_trip_key();
        let blob = tpm.seal(&key).unwrap();
        for _ in 0..MAX_UNSEAL_ATTEMPTS {
            tpm.unseal(&blob).unwrap();
        }
        let err = tpm.unseal(&blob).unwrap_err();
        assert!(
            matches!(err, StoreError::Tpm2(ref m) if m.contains("lockout")),
            "expected lockout error, got {err:?}"
        );
    }

    #[test]
    fn file_backend_seal_unseal_round_trip() {
        let mut fb = FileBackend::new();
        let key = round_trip_key();
        let blob = fb.seal(&key).unwrap();
        assert_eq!(blob.backend_id(), FileBackend::BACKEND_ID);
        let recovered = fb.unseal(&blob).unwrap();
        assert_eq!(recovered, key);
    }

    #[test]
    fn sealed_key_mock_round_trip_via_selector() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let seed = [42u8; 32];
        let mut selector = BackendSelector::mock(seed);
        let k1 = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        assert_eq!(k1.source(), KeySource::Tpm2);
        let k2 = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        assert_eq!(k1.key(), k2.key());
        assert_eq!(k2.source(), KeySource::Tpm2);
    }

    #[test]
    fn sealed_key_file_round_trip_via_selector() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let mut selector = BackendSelector::file();
        let k1 = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        assert_eq!(k1.source(), KeySource::File);
        let k2 = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        assert_eq!(k1.key(), k2.key());
    }

    #[test]
    fn sealed_key_default_selector_uses_file() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let k1 = SealedKey::load_or_create(&key_path).unwrap();
        assert_eq!(k1.source(), KeySource::File);
    }

    #[test]
    fn sealed_key_rejects_moved_blob_across_machines() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let seed_a = [1u8; 32];
        let seed_b = [2u8; 32];
        let mut sel_a = BackendSelector::mock(seed_a);
        let _ = SealedKey::load_or_create_with(&key_path, &mut sel_a).unwrap();
        let mut sel_b = BackendSelector::mock(seed_b);
        let err = SealedKey::load_or_create_with(&key_path, &mut sel_b).unwrap_err();
        assert!(
            matches!(err, StoreError::Crypto(_)),
            "expected wrong-device error, got {err:?}"
        );
    }

    #[test]
    fn blob_decode_rejects_bad_magic() {
        let bad = SealedBlob::decode(b"NOPE00").unwrap_err();
        assert!(matches!(bad, StoreError::Crypto(_)));
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
