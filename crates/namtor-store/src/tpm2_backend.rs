// SPDX-License-Identifier: GPL-3.0-only
// allow: SIZE_OK — single cohesive TPM2 backend module: struct + lifecycle
// (persist_srk/evict_srk) + PCR policy helpers + KeyBackend impl (seal/
// unseal) + CLI helpers + tests. Feature-gated; only compiled with --features
// tpm2. Splitting the CLI helpers from the backend would create a module
// with no independent use.
//! Real TPM2 backend using the `tpm2-tools` CLI (`tpm2_createprimary`,
//! `tpm2_create`, `tpm2_load`, `tpm2_unseal`, `tpm2_startauthsession`,
//! `tpm2_policypcr`, `tpm2_evictcontrol`).
//!
//! Sealing policy (spec §7.3):
//!   - Parent: TPM storage hierarchy primary key (`-C o`), the SRK.
//!   - PCR binding is opt-in via [`PcrSelection`]. When enabled, the sealed
//!     object is bound to the selected PCR values at seal time and can only
//!     be unsealed when those PCRs hold the same values. Default (no PCR
//!     selection): device-bound only, survives kernel/firmware updates.
//!   - Sealed object attributes: `fixedtpm|fixedparent|adminwithpolicy`,
//!     i.e. device-bound and non-migratable.
//!
//! SRK persistence: [`Tpm2Backend::persist_srk`] makes the primary key
//! persistent at a fixed NV handle via `tpm2_evictcontrol`, so the SRK
//! survives reboots without recomputation. [`Tpm2Backend::with_persistent_srk`]
//! constructs a backend that uses the persistent handle directly.
//!
//! The sealed payload stored inside the [`SealedBlob`] is:
//!   `[1 byte pcr_count] [pcr_count bytes: pcr_indices] [4B pub_len] [pub] [4B priv_len] [priv]`
//!
//! Gated by the `tpm2` cargo feature so the default build does not require
//! `tpm2-tools` at runtime. Real-TPM tests are `#[ignore]` — they need
//! `/dev/tpmrm0` and `NAMTOR_HW_TEST=1` (mirrors the camera tests'
//! convention). G5.5 cross-hardware is WITHHELD per the task brief: a
//! single-TPM machine cannot prove a blob sealed on machine A fails to
//! unseal on machine B.

use std::path::{Path, PathBuf};
use std::process::Command;

use rand::RngCore;

use crate::crypto::{KeyBackend, KeySource, PcrSelection, SealedBlob};
use crate::StoreError;

const SRK_PERSISTENT_HANDLE: u32 = 0x8100_0001;

/// Real TPM2 backend via the `tpm2-tools` CLI.
pub struct Tpm2Backend {
    /// Which hierarchy the primary key is derived from. `"o"` (owner/storage)
    /// in production.
    ///
    /// Configurable so a test can derive a primary from a DIFFERENT seed and
    /// prove a sealed blob will not open under it — see
    /// `real_tpm_blob_does_not_unseal_under_a_different_primary`.
    backend_id: String,
    tpm2_bin: PathBuf,
    pcr_selection: Option<PcrSelection>,
    persistent_srk: Option<u32>,
    hierarchy: String,
}

impl Tpm2Backend {
    pub const BACKEND_ID: &'static str = "tpm2";

    pub fn new(pcr_selection: Option<PcrSelection>) -> Result<Self, StoreError> {
        let tpm2_bin = which_tpm2()?;
        if !Path::new("/dev/tpmrm0").exists() {
            return Err(StoreError::Tpm2("/dev/tpmrm0 not present".into()));
        }
        let probe = Command::new(&tpm2_bin)
            .arg("pcrread")
            .arg("sha256:0")
            .output();
        match probe {
            Ok(o) if o.status.success() => Ok(Self {
                backend_id: Self::BACKEND_ID.into(),
                tpm2_bin,
                pcr_selection,
                persistent_srk: None,
                hierarchy: "o".into(),
            }),
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
                Err(StoreError::Tpm2(format!(
                    "tpm2_pcrread probe failed: {stderr}"
                )))
            }
            Err(e) => Err(StoreError::Tpm2(format!("could not exec tpm2: {e}"))),
        }
    }

    /// Derive the primary key from a different hierarchy.
    ///
    /// Production always uses the owner/storage hierarchy. This exists so a
    /// test can obtain a primary derived from a DIFFERENT seed on the same
    /// TPM, which is the nearest executable stand-in for "a different TPM":
    /// the blob's parent key is simply not available, which is exactly why a
    /// sealed template does not travel between machines.
    pub fn with_hierarchy(mut self, hierarchy: &str) -> Self {
        self.hierarchy = hierarchy.to_string();
        self
    }

    pub fn with_persistent_srk(mut self, handle: u32) -> Self {
        self.persistent_srk = Some(handle);
        self
    }

    /// Make the SRK (storage primary key) persistent at the fixed handle
    /// `0x81000001` via `tpm2_evictcontrol`. After this, subsequent seal/
    /// unseal operations skip `tpm2_createprimary` and use the persistent
    /// handle directly.
    pub fn persist_srk(&mut self) -> Result<(), StoreError> {
        if self.persistent_srk.is_some() {
            return Ok(());
        }
        let dir = temp_dir()?;
        let primary_ctx = dir.join("primary.ctx");
        self.create_primary(&primary_ctx)?;
        let handle_str = format!("0x{:08x}", SRK_PERSISTENT_HANDLE);
        run_tpm2(
            &self.tpm2_bin,
            "evictcontrol",
            &["-C", "o", "-c", str_path(&primary_ctx)?, &handle_str],
        )?;
        self.persistent_srk = Some(SRK_PERSISTENT_HANDLE);
        Ok(())
    }

    /// Remove the persistent SRK handle from the TPM.
    pub fn evict_srk(&mut self) -> Result<(), StoreError> {
        let handle = self
            .persistent_srk
            .ok_or_else(|| StoreError::Tpm2("no persistent SRK to evict".into()))?;
        let handle_str = format!("0x{:08x}", handle);
        run_tpm2(
            &self.tpm2_bin,
            "evictcontrol",
            &["-C", "o", "-c", &handle_str],
        )?;
        self.persistent_srk = None;
        Ok(())
    }

    fn create_primary(&self, ctx_path: &Path) -> Result<(), StoreError> {
        if self.persistent_srk.is_some() {
            return Ok(());
        }
        Ok(run_tpm2(
            &self.tpm2_bin,
            "createprimary",
            &["-C", &self.hierarchy, "-c", str_path(ctx_path)?],
        )?)
    }

    fn parent_handle_or_ctx(&self, primary_ctx: &Path) -> Result<String, StoreError> {
        if let Some(handle) = self.persistent_srk {
            Ok(format!("0x{:08x}", handle))
        } else {
            Ok(str_path(primary_ctx)?.to_string())
        }
    }

    fn create_pcr_policy(&self, dir: &Path, policy_path: &Path) -> Result<(), StoreError> {
        let sel = self
            .pcr_selection
            .as_ref()
            .ok_or_else(|| StoreError::Tpm2("no pcr selection".into()))?;
        let session_path = dir.join("session.ctx");
        let pcr_list = sel.tpm2_list();

        run_tpm2(
            &self.tpm2_bin,
            "startauthsession",
            &["-S", str_path(&session_path)?],
        )?;
        let policy_result = run_tpm2(
            &self.tpm2_bin,
            "policypcr",
            &[
                "-S",
                str_path(&session_path)?,
                "-l",
                &pcr_list,
                "-L",
                str_path(policy_path)?,
            ],
        );
        let _ = run_tpm2(&self.tpm2_bin, "flushcontext", &[str_path(&session_path)?]);
        policy_result?;
        Ok(())
    }

    fn satisfy_pcr_policy(&self, session_path: &Path) -> Result<(), StoreError> {
        let sel = self
            .pcr_selection
            .as_ref()
            .ok_or_else(|| StoreError::Tpm2("no pcr selection".into()))?;
        let pcr_list = sel.tpm2_list();

        run_tpm2(
            &self.tpm2_bin,
            "startauthsession",
            &["-S", str_path(session_path)?, "--policy-session"],
        )?;
        run_tpm2(
            &self.tpm2_bin,
            "policypcr",
            &["-S", str_path(session_path)?, "-l", &pcr_list],
        )?;
        Ok(())
    }
}

impl KeyBackend for Tpm2Backend {
    fn id(&self) -> &'static str {
        Self::BACKEND_ID
    }

    fn source(&self) -> KeySource {
        KeySource::Tpm2
    }

    fn seal(&mut self, key: &[u8; 32]) -> Result<SealedBlob, StoreError> {
        let dir = temp_dir()?;
        let primary_ctx = dir.join("primary.ctx");
        let pub_path = dir.join("sealed.pub");
        let priv_path = dir.join("sealed.priv");
        let input_path = dir.join("input.bin");
        let policy_path = dir.join("policy.dat");

        self.create_primary(&primary_ctx)?;
        let parent = self.parent_handle_or_ctx(&primary_ctx)?;

        std::fs::write(&input_path, key)?;

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

        let mut create_args: Vec<&str> = vec![
            "-C",
            &parent,
            "-i",
            str_path(&input_path)?,
            "-u",
            str_path(&pub_path)?,
            "-r",
            str_path(&priv_path)?,
            "-a",
            "fixedtpm|fixedparent|adminwithpolicy|noda",
        ];

        if pcr_count > 0 {
            self.create_pcr_policy(&dir, &policy_path)?;
            create_args.push("-L");
            create_args.push(str_path(&policy_path)?);
        }

        run_tpm2(&self.tpm2_bin, "create", &create_args)?;

        let pub_bytes = std::fs::read(&pub_path)?;
        let priv_bytes = std::fs::read(&priv_path)?;
        let pcr_header_len = 1 + pcr_count as usize;
        let mut payload =
            Vec::with_capacity(pcr_header_len + 8 + pub_bytes.len() + priv_bytes.len());
        payload.push(pcr_count);
        payload.extend_from_slice(&pcr_indices);
        payload.extend_from_slice(&(pub_bytes.len() as u32).to_le_bytes());
        payload.extend_from_slice(&pub_bytes);
        payload.extend_from_slice(&(priv_bytes.len() as u32).to_le_bytes());
        payload.extend_from_slice(&priv_bytes);
        Ok(SealedBlob::new(self.backend_id.clone(), payload))
    }

    fn unseal(&self, blob: &SealedBlob) -> Result<[u8; 32], StoreError> {
        if blob.backend_id() != Self::BACKEND_ID {
            return Err(StoreError::Crypto("backend id mismatch".into()));
        }
        let payload = blob.payload();
        if payload.is_empty() {
            return Err(StoreError::Crypto("tpm2 payload too short".into()));
        }
        let pcr_count = payload[0] as usize;
        if payload.len() < 1 + pcr_count + 8 {
            return Err(StoreError::Crypto(
                "tpm2 payload truncated pcr header".into(),
            ));
        }
        let pcr_indices = &payload[1..1 + pcr_count];
        let rest = &payload[1 + pcr_count..];

        if pcr_count > 0 && self.pcr_selection.is_none() {
            return Err(StoreError::Tpm2(
                "blob has PCR policy but backend has no pcr selection".into(),
            ));
        }
        if pcr_count > 0 {
            if let Some(ref sel) = self.pcr_selection {
                if sel.pcrs() != pcr_indices {
                    return Err(StoreError::Tpm2(
                        "blob PCR selection does not match backend".into(),
                    ));
                }
            }
        }

        if rest.len() < 8 {
            return Err(StoreError::Crypto(
                "tpm2 payload too short for pub/priv".into(),
            ));
        }
        let pub_len = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        if rest.len() < 4 + pub_len + 4 {
            return Err(StoreError::Crypto("tpm2 payload truncated pub".into()));
        }
        let pub_bytes = &rest[4..4 + pub_len];
        let priv_len = u32::from_le_bytes([
            rest[4 + pub_len],
            rest[5 + pub_len],
            rest[6 + pub_len],
            rest[7 + pub_len],
        ]) as usize;
        if rest.len() != 8 + pub_len + priv_len {
            return Err(StoreError::Crypto("tpm2 payload truncated priv".into()));
        }
        let priv_bytes = &rest[8 + pub_len..];

        let dir = temp_dir()?;
        let primary_ctx = dir.join("primary.ctx");
        let pub_path = dir.join("sealed.pub");
        let priv_path = dir.join("sealed.priv");
        let ctx_path = dir.join("sealed.ctx");
        let out_path = dir.join("out.bin");
        let session_path = dir.join("session.ctx");

        self.create_primary(&primary_ctx)?;
        let parent = self.parent_handle_or_ctx(&primary_ctx)?;

        std::fs::write(&pub_path, pub_bytes)?;
        std::fs::write(&priv_path, priv_bytes)?;
        run_tpm2(
            &self.tpm2_bin,
            "load",
            &[
                "-C",
                &parent,
                "-u",
                str_path(&pub_path)?,
                "-r",
                str_path(&priv_path)?,
                "-c",
                str_path(&ctx_path)?,
            ],
        )?;

        let unseal_result = if pcr_count > 0 {
            self.satisfy_pcr_policy(&session_path)?;
            let result = run_tpm2(
                &self.tpm2_bin,
                "unseal",
                &[
                    "-c",
                    str_path(&ctx_path)?,
                    "-p",
                    &format!("session:{}", str_path(&session_path)?),
                    "-o",
                    str_path(&out_path)?,
                ],
            );
            let _ = run_tpm2(&self.tpm2_bin, "flushcontext", &[str_path(&session_path)?]);
            result
        } else {
            run_tpm2(
                &self.tpm2_bin,
                "unseal",
                &["-c", str_path(&ctx_path)?, "-o", str_path(&out_path)?],
            )
        };

        match unseal_result {
            Ok(()) => {
                let out = std::fs::read(&out_path)?;
                if out.len() != 32 {
                    return Err(StoreError::Crypto(format!(
                        "tpm2 unsealed output is {} bytes, expected 32",
                        out.len()
                    )));
                }
                let mut key = [0u8; 32];
                key.copy_from_slice(&out);
                Ok(key)
            }
            Err(Tpm2CliError::Exit { stderr, .. }) => {
                if is_lockout(&stderr) {
                    Err(StoreError::Tpm2(format!("TPM2 lockout: {stderr}")))
                } else if is_pcr_mismatch(&stderr) {
                    Err(StoreError::Tpm2(format!(
                        "TPM2 PCR policy mismatch: {stderr}"
                    )))
                } else {
                    Err(StoreError::Crypto(format!(
                        "TPM2 corruption: sealed blob invalid or wrong device: {stderr}"
                    )))
                }
            }
            Err(Tpm2CliError::Io(e)) => Err(StoreError::Io(e)),
        }
    }
}

fn str_path(p: &Path) -> Result<&str, StoreError> {
    p.to_str()
        .ok_or_else(|| StoreError::Crypto("non-utf8 path".into()))
}

enum Tpm2CliError {
    Exit { stderr: String },
    Io(std::io::Error),
}

impl From<Tpm2CliError> for StoreError {
    fn from(e: Tpm2CliError) -> Self {
        match e {
            Tpm2CliError::Io(io) => StoreError::Io(io),
            Tpm2CliError::Exit { stderr } => {
                if is_lockout(&stderr) {
                    StoreError::Tpm2(format!("TPM2 lockout: {stderr}"))
                } else {
                    StoreError::Tpm2(format!("TPM2 command failed: {stderr}"))
                }
            }
        }
    }
}

fn run_tpm2(bin: &Path, sub: &str, args: &[&str]) -> Result<(), Tpm2CliError> {
    let out = Command::new(bin)
        .arg(sub)
        .args(args)
        .output()
        .map_err(Tpm2CliError::Io)?;
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(Tpm2CliError::Exit { stderr })
    }
}

fn which_tpm2() -> Result<PathBuf, StoreError> {
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    for dir in path.split(':') {
        let p = Path::new(dir).join("tpm2");
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(StoreError::Tpm2("tpm2-tools not on PATH".into()))
}

fn temp_dir() -> Result<PathBuf, StoreError> {
    let mut nonce = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut nonce);
    let dir = std::env::temp_dir().join(format!(
        "namtor-tpm-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        nonce[0], nonce[1], nonce[2], nonce[3], nonce[4], nonce[5], nonce[6], nonce[7],
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn is_lockout(stderr: &str) -> bool {
    stderr.contains("LOCKOUT")
        || stderr.contains("lockout")
        || stderr.contains("0x98b")
        || stderr.contains("0x98B")
}

fn is_pcr_mismatch(stderr: &str) -> bool {
    stderr.contains("TPM2_RC_POLICY_FAIL") || stderr.contains("policy") || stderr.contains("POLICY")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw_enabled() -> bool {
        std::env::var("NAMTOR_HW_TEST")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    #[test]
    #[ignore = "requires the reference TPM; run with NAMTOR_HW_TEST=1"]
    fn real_tpm_seal_unseal_round_trip() {
        if !hw_enabled() {
            return;
        }
        let mut backend = Tpm2Backend::new(None).expect("TPM2 probe");
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let blob = backend.seal(&key).expect("seal");
        let unsealed = backend.unseal(&blob).expect("unseal");
        assert_eq!(unsealed, key);
    }

    #[test]
    #[ignore = "requires the reference TPM; run with NAMTOR_HW_TEST=1"]
    fn real_tpm_rejects_corrupted_blob() {
        if !hw_enabled() {
            return;
        }
        let mut backend = Tpm2Backend::new(None).expect("TPM2 probe");
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let blob = backend.seal(&key).expect("seal");
        let mut encoded = blob.encode();
        encoded.truncate(encoded.len() - 1);
        let truncated = SealedBlob::decode(&encoded).expect("decode after truncate");
        let err = backend.unseal(&truncated).unwrap_err();
        assert!(matches!(err, StoreError::Crypto(_)));
    }

    #[test]
    #[ignore = "requires the reference TPM; run with NAMTOR_HW_TEST=1"]
    fn real_tpm_pcr_seal_unseal_round_trip() {
        if !hw_enabled() {
            return;
        }
        let mut backend = Tpm2Backend::new(Some(PcrSelection::boot_state())).expect("TPM2 probe");
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let blob = backend.seal(&key).expect("seal");
        let unsealed = backend.unseal(&blob).expect("unseal");
        assert_eq!(unsealed, key);
    }

    /// Gate **G5.5** — a sealed template must not open under a different key.
    ///
    /// The gate is written as "cross-hardware unseal", which reads as needing
    /// a second TPM-equipped machine. What actually makes a template
    /// device-bound is narrower: the blob is sealed under a primary key
    /// derived from the TPM's storage seed, and a blob cannot be loaded under
    /// a parent that is not that key. That is testable on one TPM, by
    /// deriving a primary from a *different* hierarchy — a different seed,
    /// hence a genuinely different key.
    ///
    /// **What this does and does not establish.** It establishes that the
    /// binding is real: move the blob to a machine whose storage seed differs
    /// and it will not open, for the same reason it does not open here. It
    /// does NOT establish that two physical TPMs have different seeds — that
    /// is a property of the TPM specification and the manufacturer, not of
    /// this code — so the literal two-machine run stays WITHHELD. This is the
    /// half that is ours to prove.
    #[test]
    #[ignore = "requires a TPM and tpm2-tools; run with NAMTOR_HW_TEST=1"]
    fn real_tpm_blob_does_not_unseal_under_a_different_primary() {
        if !hw_enabled() {
            return;
        }
        let mut owner = Tpm2Backend::new(None).expect("TPM2 probe");
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let blob = owner.seal(&key).expect("seal under the storage primary");

        // Sanity: it opens under the key it was sealed to. Without this a
        // broken seal would make the negative assertion below pass for the
        // wrong reason.
        assert_eq!(
            owner.unseal(&blob).expect("unseal under its own primary"),
            key,
            "the blob must open under the key it was sealed to"
        );

        // A primary from the endorsement hierarchy: same TPM, different seed,
        // therefore a different key — the situation a different machine puts
        // the blob in.
        let mut other = Tpm2Backend::new(None)
            .expect("TPM2 probe")
            .with_hierarchy("e");
        let result = other.unseal(&blob);
        assert!(
            result.is_err(),
            "a template sealed under the storage primary must NOT open under a \
             primary derived from a different seed; got {} bytes back",
            result.map(|v| v.len()).unwrap_or(0)
        );
    }

    #[test]
    #[ignore = "requires the reference TPM; run with NAMTOR_HW_TEST=1"]
    fn real_tpm_srk_persist_and_evict() {
        if !hw_enabled() {
            return;
        }
        let mut backend = Tpm2Backend::new(None).expect("TPM2 probe");
        backend.persist_srk().expect("persist SRK");
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let blob = backend.seal(&key).expect("seal with persistent SRK");
        let unsealed = backend.unseal(&blob).expect("unseal with persistent SRK");
        assert_eq!(unsealed, key);
        backend.evict_srk().expect("evict SRK");
    }
}
