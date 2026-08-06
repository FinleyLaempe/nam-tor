// SPDX-License-Identifier: GPL-3.0-only
//! Real TPM2 backend using the `tpm2-tools` CLI (`tpm2_createprimary`,
//! `tpm2_create`, `tpm2_load`, `tpm2_unseal`).
//!
//! Sealing policy (spec §7.3, default):
//!   - Parent: TPM storage hierarchy primary key (`-C o`).
//!   - No PCR policy (`-L` not specified). Survives kernel/firmware updates.
//!   - Sealed object attributes: `fixedtpm|fixedparent|adminwithpolicy`,
//!     i.e. device-bound and non-migratable.
//!
//! The sealed payload stored inside the [`SealedBlob`] is the concatenation
//! of the `tpm2_create` public/private area files. Unsealing reloads them
//! with `tpm2_load` (parent = storage primary) then calls `tpm2_unseal`.
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

use crate::crypto::{KeyBackend, KeySource, SealedBlob};
use crate::StoreError;

/// Real TPM2 backend via the `tpm2-tools` CLI.
pub struct Tpm2Backend {
    backend_id: String,
    tpm2_bin: PathBuf,
}

impl Tpm2Backend {
    pub const BACKEND_ID: &'static str = "tpm2";

    /// Construct by probing for `tpm2-tools` on PATH and `/dev/tpmrm0`.
    /// Returns `StoreError::Tpm2` if unavailable; the caller
    /// (`BackendSelector`) falls back to `FileBackend` and reports reduced
    /// protection.
    pub fn new() -> Result<Self, StoreError> {
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

        run_tpm2(
            &self.tpm2_bin,
            "createprimary",
            &["-C", "o", "-c", str_path(&primary_ctx)?],
        )?;

        std::fs::write(&input_path, key)?;
        run_tpm2(
            &self.tpm2_bin,
            "create",
            &[
                "-C",
                str_path(&primary_ctx)?,
                "-i",
                str_path(&input_path)?,
                "-u",
                str_path(&pub_path)?,
                "-r",
                str_path(&priv_path)?,
                "-a",
                "fixedtpm|fixedparent|adminwithpolicy|noda",
            ],
        )?;

        let pub_bytes = std::fs::read(&pub_path)?;
        let priv_bytes = std::fs::read(&priv_path)?;
        let mut payload = Vec::with_capacity(8 + pub_bytes.len() + priv_bytes.len());
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
        if payload.len() < 8 {
            return Err(StoreError::Crypto("tpm2 payload too short".into()));
        }
        let pub_len = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
        if payload.len() < 4 + pub_len + 4 {
            return Err(StoreError::Crypto("tpm2 payload truncated pub".into()));
        }
        let pub_bytes = &payload[4..4 + pub_len];
        let priv_len = u32::from_le_bytes([
            payload[4 + pub_len],
            payload[5 + pub_len],
            payload[6 + pub_len],
            payload[7 + pub_len],
        ]) as usize;
        if payload.len() != 8 + pub_len + priv_len {
            return Err(StoreError::Crypto("tpm2 payload truncated priv".into()));
        }
        let priv_bytes = &payload[8 + pub_len..];

        let dir = temp_dir()?;
        let primary_ctx = dir.join("primary.ctx");
        let pub_path = dir.join("sealed.pub");
        let priv_path = dir.join("sealed.priv");
        let ctx_path = dir.join("sealed.ctx");
        let out_path = dir.join("out.bin");

        run_tpm2(
            &self.tpm2_bin,
            "createprimary",
            &["-C", "o", "-c", str_path(&primary_ctx)?],
        )?;
        std::fs::write(&pub_path, pub_bytes)?;
        std::fs::write(&priv_path, priv_bytes)?;
        run_tpm2(
            &self.tpm2_bin,
            "load",
            &[
                "-C",
                str_path(&primary_ctx)?,
                "-u",
                str_path(&pub_path)?,
                "-r",
                str_path(&priv_path)?,
                "-c",
                str_path(&ctx_path)?,
            ],
        )?;
        match run_tpm2(
            &self.tpm2_bin,
            "unseal",
            &["-c", str_path(&ctx_path)?, "-o", str_path(&out_path)?],
        ) {
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
    // TPM2_RC_LOCKOUT hex is 0x98b; tpm2-tools spells it "LOCKOUT" in stderr.
    stderr.contains("LOCKOUT")
        || stderr.contains("lockout")
        || stderr.contains("0x98b")
        || stderr.contains("0x98B")
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
        let mut backend = Tpm2Backend::new().expect("TPM2 probe");
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
        let mut backend = Tpm2Backend::new().expect("TPM2 probe");
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let blob = backend.seal(&key).expect("seal");
        let mut encoded = blob.encode();
        encoded.truncate(encoded.len() - 1);
        let truncated = SealedBlob::decode(&encoded).expect("decode after truncate");
        let err = backend.unseal(&truncated).unwrap_err();
        assert!(matches!(err, StoreError::Crypto(_)));
    }
}
