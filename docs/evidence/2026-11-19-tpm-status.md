# TPM2 unavailable status investigation (#38)

## Observed status

The installed v0.3.8 daemon's read-only `GetProtectionStatus` returned
`sealing="software"`, `has_tpm=true`, with the warning “TPM2 device present
but unusable”. This reports the **storage key backend**, not the IR emitter
protection level. No keys or template contents were read during diagnosis.

## Root cause

`namtor-store` declares the `tpm2` Cargo feature, disabled by default. Its
`BackendSelector::select_for_new()` attempts the TPM backend only inside
`#[cfg(feature = "tpm2")]`. None of the three distro build recipes enables
that feature. The installed binary lacks diagnostic strings exclusive to
`tpm2_backend.rs`, consistent with that backend being compiled out.

The reference system has `tpm2-tools` installed, `/dev/tpmrm0` accessible to
group `tss`, and the running hardened daemon belongs to that group. These
permissions cannot enable code absent from the executable. Installing tools
alone does not fix this build configuration.

The warning is too broad: `SealedKey::protection_state()` treats any
software-backed key on a machine with `/dev/tpmrm0` as “device unusable”,
including a build without TPM2 support and an existing software key loaded
by a later TPM-capable build. Its re-seal advice is also ahead of the tools:
there is no shipped File-to-TPM2 migration operation. The existing automatic
format upgrade (`file` to `file-v2`) does not change the backend to TPM2.

## Safe follow-up

Issue #38 tracks enabling the backend in release builds, testing both build
configurations, reporting the actual fallback reason, and providing a safe,
explicit re-seal operation. Such an operation should preserve the existing
data-encryption key, atomically replace only its wrapped representation, and
verify recovery without logging or exposing key material.

**Do not delete the key file to clear this warning.** Existing encrypted
templates depend on it. Enabling a Cargo feature does not migrate an existing
software-backed key, and no key migration was performed in this session.

No TPM reset, key regeneration, unsealing, biometric capture, host service
change, or enrollment deletion was performed. TPM command execution under
the hardened unit and fresh hardware sealing remain unverified by this
read-only diagnosis.
