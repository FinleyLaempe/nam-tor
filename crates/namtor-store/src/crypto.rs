// SPDX-License-Identifier: GPL-3.0-only
// allow: SIZE_OK — single cohesive key-management module: SealedKey +
// BackendSelector + KeyBackend trait + SealedBlob + MockTpm + FileBackend +
// mlock helpers + tests. Splitting would scatter the KeyBackend contract
// across files with no benefit. Pre-existing since M4; PCR binding extends
// MockTpm in place.
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

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key as AesKey, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::StoreError;

/// Default path for the root secret file (SEC-3). The daemon running as the
/// `namtor` user owns this file with mode 0600. Tests override via
/// [`BackendSelector::with_root_secret_path`].
const DEFAULT_ROOT_SECRET_PATH: &str = "/var/lib/namtor/root_secret";

/// Argon2id parameters for SEC-3 key wrapping (OWASP-recommended minimum):
/// m_cost = 19456 KiB (19 MiB), t_cost = 2, p_cost = 1, output = 32 bytes.
/// Note: the plan text says "64 MiB" but 19456 KiB = 19 MiB, which is the
/// actual OWASP value. We follow the numeric parameter (19456).
const ARGON2_M_COST: u32 = 19456;
const ARGON2_T_COST: u32 = 2;
const ARGON2_P_COST: u32 = 1;
const ARGON2_OUTPUT_LEN: usize = 32;
const ROOT_SECRET_LEN: usize = 32;
const SALT_LEN: usize = 16;

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

/// Typed protection state of the sealing key for status reporting (spec §7.3,
/// task 8). Carries NO key material — only the sealing kind, TPM presence, and
/// a human-readable warning. The daemon maps this to the v2 `ProtectionStatus`
/// contract type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectionState {
    pub sealing: KeySource,
    pub has_tpm: bool,
    pub warning: String,
}

/// Whether a TPM2 device is present on this system (`/dev/tpmrm0`).
pub fn tpm2_device_present() -> bool {
    Path::new("/dev/tpmrm0").exists()
}

/// Which Platform Configuration Registers (PCRs) to bind a sealed key to.
///
/// When a key is sealed with a [`PcrSelection`], it can only be unsealed
/// when the selected PCRs hold the same values they had at seal time. This
/// binds the key to a specific boot state: a reboot (which extends PCR 0
/// with a new boot measurement) or a kernel update (which changes PCR 7's
/// secureboot state) will prevent unsealing.
///
/// Spec §7.3 default is **no PCR policy** (survives kernel updates). PCR
/// binding is opt-in for deployments that want stricter boot-state binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcrSelection {
    pcrs: Vec<u8>,
}

impl PcrSelection {
    /// PCR 0 (boot measurement) + PCR 7 (secureboot state) — binds the key
    /// to the current boot state.
    pub fn boot_state() -> Self {
        Self { pcrs: vec![0, 7] }
    }

    pub fn new(pcrs: Vec<u8>) -> Self {
        Self { pcrs }
    }

    pub fn pcrs(&self) -> &[u8] {
        &self.pcrs
    }

    /// Format the PCR list for `tpm2-tools` `-l` flag, e.g. `sha256:0,7`.
    #[cfg(feature = "tpm2")]
    pub fn tpm2_list(&self) -> String {
        let ids: Vec<String> = self.pcrs.iter().map(|p| p.to_string()).collect();
        format!("sha256:{}", ids.join(","))
    }
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
            let backend = match selector.open(blob.backend_id()) {
                Ok(b) => b,
                Err(e) if is_tpm2_type_backend(blob.backend_id()) => {
                    return Err(StoreError::DowngradeRefused(format!(
                        "cannot open backend '{}' sealed the existing storage: {e}",
                        blob.backend_id()
                    )));
                }
                Err(e) => return Err(e),
            };
            let key = backend.unseal(&blob)?;
            if blob.backend_id() == FileBackend::BACKEND_ID {
                let mut new_backend = selector.open(FileBackend::BACKEND_ID_V2)?;
                let new_blob = new_backend.seal(&key)?;
                atomic_write_key(key_path, &new_blob.encode())?;
            }
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

    /// Typed protection state for status reporting. Carries no key material.
    pub fn protection_state(&self) -> ProtectionState {
        let has_tpm = tpm2_device_present();
        let warning = match self.source {
            KeySource::Tpm2 => String::new(),
            KeySource::File if has_tpm => "TPM2 device present but unusable; \
                 template storage uses a software fallback key (0600, \
                 root/namtor-owned). Restore TPM2 access to re-seal."
                .to_string(),
            KeySource::File => "No TPM2 device; template storage uses a \
                 software fallback key (0600, root/namtor-owned). Templates \
                 are not hardware-bound."
                .to_string(),
        };
        ProtectionState {
            sealing: self.source,
            has_tpm,
            warning,
        }
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
    pcr_selection: Option<PcrSelection>,
    root_secret_path: Option<PathBuf>,
}

impl BackendSelector {
    pub fn new() -> Self {
        Self {
            force_backend: None,
            mock_seed: None,
            pcr_selection: None,
            root_secret_path: None,
        }
    }

    pub fn mock(seed: [u8; 32]) -> Self {
        Self {
            force_backend: Some(MockTpm::BACKEND_ID),
            mock_seed: Some(seed),
            pcr_selection: None,
            root_secret_path: None,
        }
    }

    pub fn mock_with_pcrs(seed: [u8; 32], pcrs: PcrSelection) -> Self {
        Self {
            force_backend: Some(MockTpm::BACKEND_ID),
            mock_seed: Some(seed),
            pcr_selection: Some(pcrs),
            root_secret_path: None,
        }
    }

    pub fn file() -> Self {
        Self {
            force_backend: Some(FileBackend::BACKEND_ID),
            mock_seed: None,
            pcr_selection: None,
            root_secret_path: None,
        }
    }

    pub fn with_pcrs(mut self, pcrs: PcrSelection) -> Self {
        self.pcr_selection = Some(pcrs);
        self
    }

    pub fn with_root_secret_path(mut self, path: PathBuf) -> Self {
        self.root_secret_path = Some(path);
        self
    }

    fn select_for_new(&mut self) -> Result<Box<dyn KeyBackend>, StoreError> {
        if let Some(id) = self.force_backend {
            return self.open(id);
        }
        #[cfg(feature = "tpm2")]
        {
            if Path::new("/dev/tpmrm0").exists() {
                if let Ok(b) = self.open(TPM2_BACKEND_ID) {
                    return Ok(b);
                }
            }
        }
        self.open(FileBackend::BACKEND_ID)
    }

    fn open(&self, id: &str) -> Result<Box<dyn KeyBackend>, StoreError> {
        let root_secret_path = self
            .root_secret_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_ROOT_SECRET_PATH));
        match id {
            MockTpm::BACKEND_ID => {
                let seed = self.mock_seed.ok_or_else(|| {
                    StoreError::Tpm2("mock seed not configured for this selector".into())
                })?;
                Ok(Box::new(MockTpm::new(seed, self.pcr_selection.clone())))
            }
            FileBackend::BACKEND_ID | FileBackend::BACKEND_ID_V2 => Ok(Box::new(
                FileBackend::with_root_secret_path(root_secret_path),
            )),
            #[cfg(feature = "tpm2")]
            TPM2_BACKEND_ID => Ok(Box::new(Tpm2Backend::new(self.pcr_selection.clone())?)),
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
    pcr_selection: Option<PcrSelection>,
    pcrs: BTreeMap<u8, [u8; 32]>,
    attempts: std::cell::Cell<u32>,
}

impl MockTpm {
    pub const BACKEND_ID: &'static str = "mock-tpm";

    pub fn new(seed: [u8; 32], pcr_selection: Option<PcrSelection>) -> Self {
        let pcrs = match &pcr_selection {
            Some(sel) => sel.pcrs().iter().map(|&idx| (idx, [0u8; 32])).collect(),
            None => BTreeMap::new(),
        };
        Self {
            seed,
            pcr_selection,
            pcrs,
            attempts: std::cell::Cell::new(0),
        }
    }

    fn random_seed() -> [u8; 32] {
        let mut s = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut s);
        s
    }

    pub fn fresh() -> Self {
        Self::new(Self::random_seed(), None)
    }

    /// Set a simulated PCR value. Used by tests to simulate boot measurements
    /// or PCR extension events.
    pub fn set_pcr(&mut self, idx: u8, value: [u8; 32]) {
        self.pcrs.insert(idx, value);
    }

    pub fn pcr_value(&self, idx: u8) -> Option<&[u8; 32]> {
        self.pcrs.get(&idx)
    }

    /// Simulate a reboot: PCR 0 (boot measurement) is replaced with a new
    /// random value, mimicking a fresh boot measurement. Any key sealed
    /// with [`PcrSelection::boot_state`] before the reboot will fail to
    /// unseal afterwards.
    pub fn reboot(&mut self) {
        let mut new_pcr0 = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut new_pcr0);
        self.pcrs.insert(0, new_pcr0);
    }

    /// Compute a SHA-256 policy digest over the selected PCR values.
    /// This mirrors how TPM2 `TPM2_PolicyPCR` computes its policy hash.
    fn pcr_policy_digest(&self) -> Result<[u8; 32], StoreError> {
        let sel = self
            .pcr_selection
            .as_ref()
            .ok_or_else(|| StoreError::Crypto("no pcr selection on this backend".into()))?;
        let mut hasher = Sha256::new();
        for &idx in sel.pcrs() {
            let val = self.pcrs.get(&idx).copied().unwrap_or([0u8; 32]);
            hasher.update([idx]);
            hasher.update(val);
        }
        let result = hasher.finalize();
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&result);
        Ok(digest)
    }

    fn wrap(&self, key: &[u8; 32]) -> Result<Vec<u8>, StoreError> {
        let pcr_count = self
            .pcr_selection
            .as_ref()
            .map(|s| s.pcrs().len() as u8)
            .unwrap_or(0);
        let pcr_indices: Vec<u8> = self
            .pcr_selection
            .as_ref()
            .map(|s| s.pcrs().to_vec())
            .unwrap_or_default();
        let pcr_digest = if pcr_count > 0 {
            self.pcr_policy_digest()?
        } else {
            [0u8; 32]
        };

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

        let mut out = Vec::with_capacity(1 + pcr_count as usize + 32 + 12 + ct.len() + tag.len());
        out.push(pcr_count);
        out.extend_from_slice(&pcr_indices);
        if pcr_count > 0 {
            out.extend_from_slice(&pcr_digest);
        }
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        out.extend_from_slice(&tag);
        Ok(out)
    }

    fn unwrap(&self, payload: &[u8]) -> Result<[u8; 32], StoreError> {
        if payload.is_empty() {
            return Err(StoreError::Crypto("mock-tpm payload too short".into()));
        }
        let pcr_count = payload[0] as usize;
        let pcr_header_len = 1 + pcr_count + if pcr_count > 0 { 32 } else { 0 };
        if payload.len() < pcr_header_len + 12 + 48 + 32 {
            return Err(StoreError::Crypto(
                "mock-tpm payload size mismatch (corruption)".into(),
            ));
        }

        if pcr_count > 0 {
            let stored_digest = &payload[1 + pcr_count..1 + pcr_count + 32];
            let current_digest = self.pcr_policy_digest()?;
            if stored_digest != current_digest {
                return Err(StoreError::Tpm2(
                    "PCR policy mismatch: boot state changed since seal".into(),
                ));
            }
        }

        let ct_start = pcr_header_len;
        let nonce_bytes = &payload[ct_start..ct_start + 12];
        let ct = &payload[ct_start + 12..ct_start + 12 + 48];
        let tag = &payload[ct_start + 60..ct_start + 92];

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

// --- FileBackend: argon2id-wrapped key file (SEC-3, file-v2 format) ---
//
// Old format ("file"): payload = raw 32-byte AES key (no protection).
// New format ("file-v2"): payload = salt(16) || nonce(12) || AES-256-GCM
//   ciphertext+tag(48). The KEK is argon2id(root_secret, salt, m=19456,
//   t=2, p=1, out=32). The root_secret is a 32-byte random file at
//   /var/lib/namtor/root_secret (namtor-owned, 0600), separate from the
//   key file. An attacker who steals the key file alone cannot decrypt
//   templates; they also need the root_secret file (defense-in-depth, not
//   a full disk-theft mitigation — only TPM2 sealing defeats disk theft).
//
// Migration: old "file" blobs auto-migrate on load. The raw key is read,
// re-wrapped with argon2id, and written back atomically (temp + fsync +
// rename). If any step fails, the old blob remains on disk.

/// The root secret used to derive argon2id KEKs for FileBackend key
/// wrapping. Loaded from disk with mlock; zeroised on drop.
struct RootSecret {
    bytes: [u8; ROOT_SECRET_LEN],
}

/// Attach the path and the operation to an I/O error, preserving its kind.
///
/// A daemon that refuses to start is the worst place to lose this. The root
/// secret lives at a fixed path and every access here used a bare `?`, so a
/// permission problem surfaced as "store: i/o error: Permission denied (os
/// error 13)" — no path, no operation, nothing for an operator to act on. The
/// project holds its PAM failures to a specific, actionable message in spec
/// §8.3; a startup failure deserves the same.
fn io_context(op: &str, path: &Path, e: std::io::Error) -> StoreError {
    StoreError::Io(std::io::Error::new(
        e.kind(),
        format!("{op} {}: {e}", path.display()),
    ))
}

impl RootSecret {
    fn load_or_create(path: &Path) -> Result<Self, StoreError> {
        if path.exists() {
            let raw = std::fs::read(path).map_err(|e| io_context("read root secret", path, e))?;
            if raw.len() != ROOT_SECRET_LEN {
                return Err(StoreError::Crypto(format!(
                    "root secret at {} is {} bytes, expected {ROOT_SECRET_LEN}",
                    path.display(),
                    raw.len()
                )));
            }
            let mut bytes = [0u8; ROOT_SECRET_LEN];
            bytes.copy_from_slice(&raw);
            let mut s = Self { bytes };
            s.lock_memory();
            return Ok(s);
        }
        let mut bytes = [0u8; ROOT_SECRET_LEN];
        rand::thread_rng().fill_bytes(&mut bytes);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                io_context("create the state directory for the root secret", parent, e)
            })?;
        }
        std::fs::write(path, bytes).map_err(|e| io_context("write root secret", path, e))?;
        set_root_only(path)?;
        let mut s = Self { bytes };
        s.lock_memory();
        Ok(s)
    }

    fn lock_memory(&mut self) {
        lock_bytes(&self.bytes);
    }

    fn derive_kek(&self, salt: &[u8]) -> Result<[u8; ARGON2_OUTPUT_LEN], StoreError> {
        let params = Params::new(
            ARGON2_M_COST,
            ARGON2_T_COST,
            ARGON2_P_COST,
            Some(ARGON2_OUTPUT_LEN),
        )
        .map_err(|e| StoreError::Crypto(format!("argon2 params: {e}")))?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut out = [0u8; ARGON2_OUTPUT_LEN];
        argon2
            .hash_password_into(&self.bytes, salt, &mut out)
            .map_err(|e| StoreError::Crypto(format!("argon2id derive: {e}")))?;
        Ok(out)
    }
}

impl Drop for RootSecret {
    fn drop(&mut self) {
        unlock_bytes(&self.bytes);
        self.bytes.zeroize();
    }
}

pub struct FileBackend {
    root_secret_path: PathBuf,
}

impl FileBackend {
    pub const BACKEND_ID: &'static str = "file";
    pub const BACKEND_ID_V2: &'static str = "file-v2";

    pub fn new() -> Self {
        Self {
            root_secret_path: PathBuf::from(DEFAULT_ROOT_SECRET_PATH),
        }
    }

    pub fn with_root_secret_path(path: PathBuf) -> Self {
        Self {
            root_secret_path: path,
        }
    }

    fn seal_v2(&mut self, key: &[u8; 32]) -> Result<SealedBlob, StoreError> {
        let root_secret = RootSecret::load_or_create(&self.root_secret_path)?;
        let mut salt = [0u8; SALT_LEN];
        rand::thread_rng().fill_bytes(&mut salt);
        let kek = root_secret.derive_kek(&salt)?;
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let cipher =
            Aes256Gcm::new_from_slice(&kek).map_err(|e| StoreError::Crypto(e.to_string()))?;
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = cipher
            .encrypt(nonce, key.as_ref())
            .map_err(|e| StoreError::Crypto(format!("aes-gcm seal: {e}")))?;
        let mut payload = Vec::with_capacity(SALT_LEN + 12 + ct.len());
        payload.extend_from_slice(&salt);
        payload.extend_from_slice(&nonce_bytes);
        payload.extend_from_slice(&ct);
        Ok(SealedBlob::new(Self::BACKEND_ID_V2.into(), payload))
    }

    fn unseal_v2(&self, payload: &[u8]) -> Result<[u8; 32], StoreError> {
        if payload.len() < SALT_LEN + 12 + 32 + 16 {
            return Err(StoreError::Crypto(
                "file-v2 payload too short (expected salt+nonce+ct)".into(),
            ));
        }
        let salt = &payload[..SALT_LEN];
        let nonce_bytes = &payload[SALT_LEN..SALT_LEN + 12];
        let ct = &payload[SALT_LEN + 12..];
        let root_secret = RootSecret::load_or_create(&self.root_secret_path)?;
        let kek = root_secret.derive_kek(salt)?;
        let cipher =
            Aes256Gcm::new_from_slice(&kek).map_err(|e| StoreError::Crypto(e.to_string()))?;
        let nonce = Nonce::from_slice(nonce_bytes);
        let pt = cipher.decrypt(nonce, ct).map_err(|_| {
            StoreError::Crypto("file-v2 unseal failed (wrong root secret or corrupted)".into())
        })?;
        if pt.len() != 32 {
            return Err(StoreError::Crypto(format!(
                "file-v2 unsealed key is {} bytes, expected 32",
                pt.len()
            )));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&pt);
        Ok(key)
    }

    fn unseal_v1(&self, payload: &[u8]) -> Result<[u8; 32], StoreError> {
        if payload.len() != 32 {
            return Err(StoreError::Crypto(
                "file backend payload not 32 bytes".into(),
            ));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(payload);
        Ok(key)
    }
}

impl Default for FileBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyBackend for FileBackend {
    fn id(&self) -> &'static str {
        Self::BACKEND_ID_V2
    }

    fn seal(&mut self, key: &[u8; 32]) -> Result<SealedBlob, StoreError> {
        self.seal_v2(key)
    }

    fn unseal(&self, blob: &SealedBlob) -> Result<[u8; 32], StoreError> {
        match blob.backend_id() {
            Self::BACKEND_ID => self.unseal_v1(blob.payload()),
            Self::BACKEND_ID_V2 => self.unseal_v2(blob.payload()),
            _ => Err(StoreError::Crypto("backend id mismatch".into())),
        }
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

const TPM2_BACKEND_ID: &str = "tpm2";

fn is_tpm2_type_backend(backend_id: &str) -> bool {
    backend_id == TPM2_BACKEND_ID || backend_id == MockTpm::BACKEND_ID
}

fn set_root_only(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn atomic_write_key(path: &Path, data: &[u8]) -> Result<(), StoreError> {
    let stem = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "key".to_string());
    let mut tmp = path.to_path_buf();
    tmp.set_file_name(format!(".{stem}.tmp"));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
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
    use std::os::unix::fs::MetadataExt;

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
        let mut tpm = MockTpm::new(seed, None);
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
            let mut tpm = MockTpm::new(seed, None);
            tpm.seal(&key).unwrap()
        };
        let tpm2 = MockTpm::new(seed, None);
        let recovered = tpm2.unseal(&blob).unwrap();
        assert_eq!(recovered, key);
    }

    #[test]
    fn mock_migrate_reseal_new_seed_old_blob_invalid() {
        let old_seed = [1u8; 32];
        let new_seed = [2u8; 32];
        let key = round_trip_key();
        let old_blob = {
            let mut old = MockTpm::new(old_seed, None);
            old.seal(&key).unwrap()
        };
        let new_blob = {
            let mut new = MockTpm::new(new_seed, None);
            new.seal(&key).unwrap()
        };
        assert_ne!(old_blob.payload(), new_blob.payload());
        let new = MockTpm::new(new_seed, None);
        assert_eq!(new.unseal(&new_blob).unwrap(), key);
        let err = new.unseal(&old_blob).unwrap_err();
        assert!(matches!(err, StoreError::Crypto(_)));
    }

    #[test]
    fn mock_reject_moved_blob() {
        let (seed_a, seed_b) = two_seeds();
        let key = round_trip_key();
        let blob = {
            let mut a = MockTpm::new(seed_a, None);
            a.seal(&key).unwrap()
        };
        let b = MockTpm::new(seed_b, None);
        let err = b.unseal(&blob).unwrap_err();
        assert!(
            matches!(err, StoreError::Crypto(_)),
            "expected corruption/wrong-device error, got {err:?}"
        );
    }

    #[test]
    fn mock_corruption_detected() {
        let seed = [4u8; 32];
        let mut tpm = MockTpm::new(seed, None);
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
        let mut tpm = MockTpm::new(seed, None);
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

    // --- PCR binding tests ---

    #[test]
    fn mock_pcr_seal_unseal_round_trip() {
        let seed = [11u8; 32];
        let mut tpm = MockTpm::new(seed, Some(PcrSelection::boot_state()));
        let key = round_trip_key();
        let blob = tpm.seal(&key).unwrap();
        let recovered = tpm.unseal(&blob).unwrap();
        assert_eq!(recovered, key);
    }

    #[test]
    fn mock_pcr_mismatch_fails_after_reboot() {
        let seed = [12u8; 32];
        let mut tpm = MockTpm::new(seed, Some(PcrSelection::boot_state()));
        let key = round_trip_key();
        let blob = tpm.seal(&key).unwrap();
        tpm.reboot();
        let err = tpm.unseal(&blob).unwrap_err();
        assert!(
            matches!(err, StoreError::Tpm2(ref m) if m.contains("PCR policy mismatch")),
            "expected PCR mismatch error, got {err:?}"
        );
    }

    #[test]
    fn mock_pcr_mismatch_fails_after_pcr_change() {
        let seed = [13u8; 32];
        let mut tpm = MockTpm::new(seed, Some(PcrSelection::boot_state()));
        let key = round_trip_key();
        let blob = tpm.seal(&key).unwrap();
        tpm.set_pcr(7, [0xff; 32]);
        let err = tpm.unseal(&blob).unwrap_err();
        assert!(
            matches!(err, StoreError::Tpm2(ref m) if m.contains("PCR policy mismatch")),
            "expected PCR mismatch error, got {err:?}"
        );
    }

    #[test]
    fn mock_no_pcr_binding_survives_reboot() {
        let seed = [14u8; 32];
        let mut tpm = MockTpm::new(seed, None);
        let key = round_trip_key();
        let blob = tpm.seal(&key).unwrap();
        tpm.reboot();
        let recovered = tpm.unseal(&blob).unwrap();
        assert_eq!(recovered, key);
    }

    #[test]
    fn mock_pcr_persistent_across_restart_same_state() {
        let seed = [15u8; 32];
        let pcrs = PcrSelection::boot_state();
        let key = round_trip_key();
        let blob = {
            let mut tpm = MockTpm::new(seed, Some(pcrs.clone()));
            let mut pcr0 = [0u8; 32];
            pcr0[0] = 0xAB;
            tpm.set_pcr(0, pcr0);
            tpm.seal(&key).unwrap()
        };
        let mut tpm2 = MockTpm::new(seed, Some(pcrs));
        let mut pcr0 = [0u8; 32];
        pcr0[0] = 0xAB;
        tpm2.set_pcr(0, pcr0);
        let recovered = tpm2.unseal(&blob).unwrap();
        assert_eq!(recovered, key);
    }

    #[test]
    fn mock_pcr_persistent_fails_different_state() {
        let seed = [16u8; 32];
        let pcrs = PcrSelection::boot_state();
        let key = round_trip_key();
        let blob = {
            let mut tpm = MockTpm::new(seed, Some(pcrs.clone()));
            let mut pcr0 = [0u8; 32];
            pcr0[0] = 0xAB;
            tpm.set_pcr(0, pcr0);
            tpm.seal(&key).unwrap()
        };
        let tpm2 = MockTpm::new(seed, Some(pcrs));
        let err = tpm2.unseal(&blob).unwrap_err();
        assert!(
            matches!(err, StoreError::Tpm2(ref m) if m.contains("PCR policy mismatch")),
            "expected PCR mismatch, got {err:?}"
        );
    }

    #[test]
    fn mock_pcr_sealed_key_via_selector() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let seed = [42u8; 32];
        let mut selector = BackendSelector::mock_with_pcrs(seed, PcrSelection::boot_state());
        let k1 = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        assert_eq!(k1.source(), KeySource::Tpm2);
        let k2 = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        assert_eq!(k1.key(), k2.key());
        assert_eq!(k2.source(), KeySource::Tpm2);
    }

    #[test]
    fn file_backend_seal_unseal_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut fb = FileBackend::with_root_secret_path(dir.path().join("root_secret"));
        let key = round_trip_key();
        let blob = fb.seal(&key).unwrap();
        assert_eq!(blob.backend_id(), FileBackend::BACKEND_ID_V2);
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
        let mut selector =
            BackendSelector::file().with_root_secret_path(dir.path().join("root_secret"));
        let k1 = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        assert_eq!(k1.source(), KeySource::File);
        let k2 = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        assert_eq!(k1.key(), k2.key());
    }

    #[test]
    fn sealed_key_default_selector_uses_file() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let mut selector =
            BackendSelector::new().with_root_secret_path(dir.path().join("root_secret"));
        let k1 = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
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

    #[test]
    fn protection_state_tpm2_has_no_warning() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let mut selector = BackendSelector::mock([42u8; 32]);
        let sk = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        let state = sk.protection_state();
        assert_eq!(state.sealing, KeySource::Tpm2);
        assert!(state.warning.is_empty(), "TPM2 warning must be empty");
    }

    #[test]
    fn protection_state_software_has_visible_warning() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let mut selector =
            BackendSelector::file().with_root_secret_path(dir.path().join("root_secret"));
        let sk = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        let state = sk.protection_state();
        assert_eq!(state.sealing, KeySource::File);
        assert!(
            !state.warning.is_empty(),
            "software fallback must carry a visible warning"
        );
    }

    #[test]
    fn protection_state_has_tpm_reflects_device_presence() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let mut selector =
            BackendSelector::file().with_root_secret_path(dir.path().join("root_secret"));
        let sk = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        let state = sk.protection_state();
        assert_eq!(state.has_tpm, tpm2_device_present());
    }

    #[test]
    fn protection_state_contains_no_key_material() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let mut selector = BackendSelector::mock([99u8; 32]);
        let sk = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        let raw_key = sk.key();
        let state = sk.protection_state();
        // Only check the `warning` String field — the sealing enum and has_tpm bool
        // cannot carry key data, and the struct/field names in Debug output contain
        // static substrings (e.g. "ec" in "ProtectionState") that false-positive against
        // arbitrary key bytes like 0xec.
        let warning = &state.warning;
        for byte in raw_key.iter() {
            let hex_lower = format!("{byte:02x}");
            let hex_upper = format!("{byte:02X}");
            assert!(
                !warning.contains(&hex_lower) && !warning.contains(&hex_upper),
                "protection state warning leaks key byte {byte:#x}"
            );
        }
        assert!(
            !warning.contains(key_path.to_str().unwrap()),
            "protection state warning must not expose the key path"
        );
    }

    #[test]
    fn fallback_key_file_is_0600() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let mut selector =
            BackendSelector::file().with_root_secret_path(dir.path().join("root_secret"));
        let _sk = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        let mode = std::fs::metadata(&key_path).unwrap().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "fallback key file must be 0600, got {:o}",
            mode & 0o777
        );
    }

    #[test]
    fn downgrade_refused_when_tpm2_blob_cannot_be_opened() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let mut tpm_selector = BackendSelector::mock([7u8; 32]);
        let _sk = SealedKey::load_or_create_with(&key_path, &mut tpm_selector).unwrap();
        assert_eq!(_sk.source(), KeySource::Tpm2);
        let mut file_selector =
            BackendSelector::file().with_root_secret_path(dir.path().join("root_secret"));
        let err = SealedKey::load_or_create_with(&key_path, &mut file_selector).unwrap_err();
        assert!(
            matches!(err, StoreError::DowngradeRefused(ref m) if !m.is_empty()),
            "expected DowngradeRefused, got {err:?}"
        );
    }

    #[test]
    fn downgrade_refused_for_real_tpm2_backend_id_without_tpm() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let fake_payload = vec![0u8; 32];
        let blob = SealedBlob::new("tpm2".into(), fake_payload);
        std::fs::write(&key_path, blob.encode()).unwrap();
        let mut selector = BackendSelector::new();
        let err = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap_err();
        assert!(
            matches!(err, StoreError::DowngradeRefused(_)),
            "expected DowngradeRefused for tpm2 blob without tpm, got {err:?}"
        );
    }

    #[test]
    fn file_blob_reloads_without_downgrade_error() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let root_secret = dir.path().join("root_secret");
        let mut file_selector = BackendSelector::file().with_root_secret_path(root_secret.clone());
        let k1 = SealedKey::load_or_create_with(&key_path, &mut file_selector).unwrap();
        assert_eq!(k1.source(), KeySource::File);
        let mut default_selector = BackendSelector::new().with_root_secret_path(root_secret);
        let k2 = SealedKey::load_or_create_with(&key_path, &mut default_selector).unwrap();
        assert_eq!(k2.source(), KeySource::File);
        assert_eq!(k1.key(), k2.key());
    }

    #[test]
    fn pcr_mismatch_denied_after_reboot() {
        let seed = [55u8; 32];
        let mut tpm = MockTpm::new(seed, Some(PcrSelection::boot_state()));
        let key = round_trip_key();
        let blob = tpm.seal(&key).unwrap();
        tpm.reboot();
        let err = tpm.unseal(&blob).unwrap_err();
        assert!(
            matches!(err, StoreError::Tpm2(ref m) if m.contains("PCR policy mismatch")),
            "expected PCR mismatch denial, got {err:?}"
        );
    }

    // --- SEC-3: argon2id-wrapped FileBackend (file-v2) tests ---

    #[test]
    fn file_v2_seal_produces_file_v2_backend_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut fb = FileBackend::with_root_secret_path(dir.path().join("root_secret"));
        let key = round_trip_key();
        let blob = fb.seal(&key).unwrap();
        assert_eq!(blob.backend_id(), FileBackend::BACKEND_ID_V2);
    }

    #[test]
    fn file_v2_payload_format_is_salt_nonce_ciphertext() {
        let dir = tempfile::tempdir().unwrap();
        let mut fb = FileBackend::with_root_secret_path(dir.path().join("root_secret"));
        let key = round_trip_key();
        let blob = fb.seal(&key).unwrap();
        let payload = blob.payload();
        assert_eq!(
            payload.len(),
            SALT_LEN + 12 + 32 + 16,
            "file-v2 payload must be salt(16)+nonce(12)+ct(32+16)=76 bytes"
        );
    }

    #[test]
    fn file_v2_tamper_wrong_root_secret_fails() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let key = round_trip_key();
        let blob = {
            let mut fb = FileBackend::with_root_secret_path(dir_a.path().join("root_secret"));
            fb.seal(&key).unwrap()
        };
        let other_key = round_trip_key();
        let _ = {
            let mut fb = FileBackend::with_root_secret_path(dir_b.path().join("root_secret"));
            fb.seal(&other_key).unwrap()
        };
        let fb_wrong = FileBackend::with_root_secret_path(dir_b.path().join("root_secret"));
        let err = fb_wrong.unseal(&blob).unwrap_err();
        assert!(
            matches!(err, StoreError::Crypto(_)),
            "expected crypto error with wrong root secret, got {err:?}"
        );
    }

    #[test]
    fn file_v2_tamper_corrupted_payload_fails() {
        let dir = tempfile::tempdir().unwrap();
        let mut fb = FileBackend::with_root_secret_path(dir.path().join("root_secret"));
        let key = round_trip_key();
        let blob = fb.seal(&key).unwrap();
        let mut tampered = blob.encode();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        let bad = SealedBlob::decode(&tampered).unwrap();
        let err = fb.unseal(&bad).unwrap_err();
        assert!(
            matches!(err, StoreError::Crypto(_)),
            "expected crypto error on corrupted payload, got {err:?}"
        );
    }

    #[test]
    fn file_v2_migrates_old_file_blob_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let root_secret = dir.path().join("root_secret");
        let key = round_trip_key();
        let old_blob = SealedBlob::new("file".into(), key.to_vec());
        std::fs::write(&key_path, old_blob.encode()).unwrap();
        let mut selector = BackendSelector::file().with_root_secret_path(root_secret.clone());
        let sk = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        assert_eq!(sk.key(), &key, "migrated key must match original");
        assert_eq!(sk.source(), KeySource::File);
        let raw = std::fs::read(&key_path).unwrap();
        let new_blob = SealedBlob::decode(&raw).unwrap();
        assert_eq!(new_blob.backend_id(), FileBackend::BACKEND_ID_V2);
        let mut selector2 = BackendSelector::file().with_root_secret_path(root_secret);
        let sk2 = SealedKey::load_or_create_with(&key_path, &mut selector2).unwrap();
        assert_eq!(sk2.key(), &key, "reloaded key must match original");
    }

    #[test]
    fn file_v2_old_file_blob_does_not_trigger_downgrade_refused() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let key = round_trip_key();
        let old_blob = SealedBlob::new("file".into(), key.to_vec());
        std::fs::write(&key_path, old_blob.encode()).unwrap();
        let mut selector =
            BackendSelector::new().with_root_secret_path(dir.path().join("root_secret"));
        let sk = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        assert_eq!(sk.key(), &key);
        assert_eq!(sk.source(), KeySource::File);
    }

    #[test]
    fn file_v2_root_secret_file_is_0600() {
        let dir = tempfile::tempdir().unwrap();
        let root_secret_path = dir.path().join("root_secret");
        let mut fb = FileBackend::with_root_secret_path(root_secret_path.clone());
        let key = round_trip_key();
        let _blob = fb.seal(&key).unwrap();
        let mode = std::fs::metadata(&root_secret_path).unwrap().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "root_secret file must be 0600, got {:o}",
            mode & 0o777
        );
    }

    #[test]
    fn file_v2_sealed_key_round_trip_via_selector() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let mut selector =
            BackendSelector::file().with_root_secret_path(dir.path().join("root_secret"));
        let k1 = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        assert_eq!(k1.source(), KeySource::File);
        let k2 = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        assert_eq!(k1.key(), k2.key());
    }

    #[test]
    fn file_v2_raw_key_not_in_payload_as_substring() {
        let dir = tempfile::tempdir().unwrap();
        let mut fb = FileBackend::with_root_secret_path(dir.path().join("root_secret"));
        let key = round_trip_key();
        let blob = fb.seal(&key).unwrap();
        let payload = blob.payload();
        assert!(
            !payload.windows(32).any(|w| w == &key[..]),
            "raw 32-byte key must not appear as a contiguous substring in the payload"
        );
    }

    #[test]
    fn file_v2_migration_preserves_key_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let root_secret = dir.path().join("root_secret");
        let key = round_trip_key();
        let old_blob = SealedBlob::new("file".into(), key.to_vec());
        std::fs::write(&key_path, old_blob.encode()).unwrap();
        let mut selector = BackendSelector::file().with_root_secret_path(root_secret.clone());
        let sk1 = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        let mut selector2 = BackendSelector::file().with_root_secret_path(root_secret);
        let sk2 = SealedKey::load_or_create_with(&key_path, &mut selector2).unwrap();
        assert_eq!(sk1.key(), sk2.key(), "key must survive migration + reload");
        assert_eq!(sk1.key(), &key);
    }

    #[test]
    fn file_v2_backend_id_distinguishable_from_old() {
        assert_ne!(FileBackend::BACKEND_ID, FileBackend::BACKEND_ID_V2);
        assert_eq!(FileBackend::BACKEND_ID, "file");
        assert_eq!(FileBackend::BACKEND_ID_V2, "file-v2");
    }

    #[test]
    fn file_v2_can_unseal_old_file_blob_for_migration() {
        let dir = tempfile::tempdir().unwrap();
        let fb = FileBackend::with_root_secret_path(dir.path().join("root_secret"));
        let key = round_trip_key();
        let old_blob = SealedBlob::new("file".into(), key.to_vec());
        let recovered = fb.unseal(&old_blob).unwrap();
        assert_eq!(
            recovered, key,
            "FileBackend must unseal old 'file' blobs for migration"
        );
    }

    #[test]
    fn file_v2_no_temp_file_remains_after_migration() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("key");
        let root_secret = dir.path().join("root_secret");
        let key = round_trip_key();
        let old_blob = SealedBlob::new("file".into(), key.to_vec());
        std::fs::write(&key_path, old_blob.encode()).unwrap();
        let mut selector = BackendSelector::file().with_root_secret_path(root_secret);
        let _sk = SealedKey::load_or_create_with(&key_path, &mut selector).unwrap();
        let tmp = key_path.with_file_name(format!(
            ".{}.tmp",
            key_path.file_name().unwrap().to_string_lossy()
        ));
        assert!(
            !tmp.exists(),
            "temp file must not remain after successful migration"
        );
    }
}
