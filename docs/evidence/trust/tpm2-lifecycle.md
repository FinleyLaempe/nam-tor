# TPM2-Sealed Key Lifecycle Evidence

**Gate:** G5.5 — templates unreadable off-device (TPM)
**Status:** `partial` — real TPM2 sealing with PCR binding + SRK persistence implemented; cross-hardware claim WITHHELD.
**Date:** 2026-08-08
**Task:** 26 — Complete TPM2-sealed key lifecycle
**Commit:** `feat(store): complete TPM2 sealed key lifecycle`

## What shipped

### Full TPM2 key lifecycle

The `Tpm2Backend` (`crates/namtor-store/src/tpm2_backend.rs`, feature-gated
`tpm2`) now implements the complete TPM2 sealed key lifecycle via the
`tpm2-tools` CLI:

| Step | Command | Description |
|---|---|---|
| **Create SRK** | `tpm2_createprimary -C o -c primary.ctx` | Storage hierarchy primary key (Storage Root Key) |
| **Create sealed object** | `tpm2_create -C primary.ctx -i input.bin -u sealed.pub -r sealed.priv [-L policy.dat]` | Seal 32-byte AES key under SRK, optionally with PCR policy |
| **Load** | `tpm2_load -C primary.ctx -u sealed.pub -r sealed.priv -c sealed.ctx` | Load sealed object into TPM |
| **Unseal** | `tpm2_unseal -c sealed.ctx [-p session:session.ctx] -o out.bin` | Unseal to recover the key, optionally via PCR policy session |
| **Persist SRK** | `tpm2_evictcontrol -C o -c primary.ctx 0x81000001` | Make SRK persistent at fixed NV handle |
| **Evict SRK** | `tpm2_evictcontrol -C o -c 0x81000001` | Remove persistent SRK from TPM NV |

**SRK persistence:** `Tpm2Backend::persist_srk()` makes the primary key
persistent at handle `0x81000001` via `tpm2_evictcontrol`. After this,
seal/unseal operations skip `tpm2_createprimary` and use the persistent
handle directly as the parent — the SRK survives reboots without
recomputation. `Tpm2Backend::evict_srk()` removes the persistent handle.

### PCR binding (opt-in)

`PcrSelection` (in `crypto.rs`, exported from `lib.rs`) specifies which
Platform Configuration Registers to bind a sealed key to:

- `PcrSelection::boot_state()` — PCR 0 (boot measurement) + PCR 7
  (secureboot state). Binds the key to the current boot state.
- `PcrSelection::new(pcrs)` — custom PCR selection.

**Spec §7.3 default: no PCR policy.** The default `BackendSelector` does
not use PCR binding, so sealed keys survive kernel/firmware updates. PCR
binding is opt-in for deployments that want stricter boot-state binding.

**Seal with PCR:**
1. `tpm2_startauthsession -S session.ctx` (trial session)
2. `tpm2_policypcr -S session.ctx -l sha256:0,7 -L policy.dat` (compute policy digest)
3. `tpm2_flushcontext session.ctx` (flush trial session)
4. `tpm2_create ... -L policy.dat` (seal with PCR policy)

**Unseal with PCR:**
1. `tpm2_startauthsession -S session.ctx --policy-session` (policy session)
2. `tpm2_policypcr -S session.ctx -l sha256:0,7` (satisfy policy)
3. `tpm2_unseal -c sealed.ctx -p session:session.ctx -o out.bin` (unseal via session)
4. `tpm2_flushcontext session.ctx` (flush policy session — always runs, even on error)

If the PCR values have changed since seal time (e.g. after a reboot),
`tpm2_policypcr` produces a different policy digest that doesn't match
the sealed object's policy, and `tpm2_unseal` fails with
`TPM2_RC_POLICY_FAIL`. The backend maps this to `StoreError::Tpm2`.

### Blob payload format

The `SealedBlob` payload for the `tpm2` backend now includes a PCR header:

```
[1 byte: pcr_count (0 = no PCR binding)]
[pcr_count bytes: PCR indices]
[4 bytes: pub_len (LE u32)]
[pub_len bytes: public area]
[4 bytes: priv_len (LE u32)]
[priv_len bytes: private area]
```

When `pcr_count` is 0, the header is a single `0x00` byte followed by
the public/private areas. This lets the unseal path know whether to
start a PCR policy session.

### MockTpm PCR simulation

`MockTpm` (`crypto.rs`) simulates PCR binding for CI (no TPM hardware):

- `MockTpm::new(seed, pcr_selection)` — constructs with optional PCR binding.
- `set_pcr(idx, value)` — set a simulated PCR value (simulates measurement).
- `reboot()` — replaces PCR 0 with a new random value (simulates boot measurement change).
- `pcr_policy_digest()` — computes `SHA-256(concat(pcr_index || pcr_value))` over the selected PCRs, mirroring `TPM2_PolicyPCR`.

The PCR policy digest is stored in the sealed payload. On unseal, the
digest is recomputed from the current PCR state and compared. Mismatch →
`StoreError::Tpm2("PCR policy mismatch: boot state changed")`.

### BackendSelector PCR support

`BackendSelector` carries an optional `PcrSelection`:

- `BackendSelector::mock_with_pcrs(seed, pcrs)` — mock with PCR binding.
- `BackendSelector::with_pcrs(pcrs)` — builder method to add PCR binding to any selector.

## Test results

### MockTpm PCR tests (CI, no hardware)

All run under `cargo test -p namtor-store` (no `tpm2` feature, no hardware).

| Test | Scenario | Result |
|---|---|---|
| `mock_pcr_seal_unseal_round_trip` | seal with PCR binding, unseal on same PCR state | PASS |
| `mock_pcr_mismatch_fails_after_reboot` | seal, reboot (change PCR 0), unseal fails | PASS |
| `mock_pcr_mismatch_fails_after_pcr_change` | seal, change PCR 7, unseal fails | PASS |
| `mock_no_pcr_binding_survives_reboot` | seal without PCR, reboot, unseal succeeds | PASS |
| `mock_pcr_persistent_across_restart_same_state` | seal, new MockTpm with same seed + same PCR state, unseal succeeds | PASS |
| `mock_pcr_persistent_fails_different_state` | seal, new MockTpm with same seed but different PCR state, unseal fails | PASS |
| `mock_pcr_sealed_key_via_selector` | end-to-end via `SealedKey` + `BackendSelector::mock_with_pcrs` | PASS |

### Existing MockTpm contract tests (unchanged, all PASS)

All 12 pre-existing MockTpm/FileBackend/SealedKey tests pass unchanged
after updating `MockTpm::new` to accept `Option<PcrSelection>`. The
no-PCR path (`None`) exercises the same code as before.

### Real-TPM tests (hardware-gated, `#[ignore]`)

`cargo test -p namtor-store --features tpm2 -- --ignored`:

| Test | Status |
|---|---|
| `real_tpm_seal_unseal_round_trip` | `#[ignore]` — requires `NAMTOR_HW_TEST=1` + `tss` group + `/dev/tpmrm0` |
| `real_tpm_rejects_corrupted_blob` | `#[ignore]` — same |
| `real_tpm_pcr_seal_unseal_round_trip` | `#[ignore]` — same; tests PCR-bound seal/unseal |
| `real_tpm_srk_persist_and_evict` | `#[ignore]` — same; tests SRK persistence lifecycle |

**Not run in CI.** The build user is not in the `tss` group and cannot
open `/dev/tpmrm0`. These tests are documented and ready for a
hardware-equipped CI runner.

### Test counts

- `cargo test -p namtor-store`: 44 lib tests + 9 integration tests = 53 PASS
- `cargo test -p namtor-store --features tpm2`: 44 lib (4 ignored) + 9 integration = 53 PASS

### Build verification

- `cargo build -p namtor-store` — PASS (default, no `tpm2` feature)
- `cargo build -p namtor-store --features tpm2` — PASS
- `cargo clippy -p namtor-store -- -D warnings` — clean
- `cargo clippy -p namtor-store --features tpm2 -- -D warnings` — clean
- `cargo fmt -p namtor-store -- --check` — clean
- `cargo build -p namtor-daemon -p namtor-cli -p namtor-gnome` — PASS (dependents unchanged)

## G5.5 cross-hardware status: WITHHELD

**Claim NOT made.** The G5.5 gate requires proof that a blob sealed on
machine A fails to unseal on machine B. Only one TPM-equipped machine is
available (the ASUS ROG Flow X16, `/dev/tpmrm0`). Without a second
TPM-equipped machine, the cross-hardware unseal procedure cannot be
demonstrated. This is unchanged from the prior G5.5 evidence (`g5-tpm.md`).

## Runtime requirements for real TPM2

To use the real TPM2 backend (`--features tpm2`):

1. `tpm2-tools` installed (`tpm2` binary on PATH).
2. `/dev/tpmrm0` present (TPM 2.0 resource manager).
3. The `namtor` system user in the `tss` group.
4. For PCR binding: the selected PCRs must be readable (`tpm2_pcrread`).
5. For SRK persistence: `tpm2_evictcontrol` requires storage hierarchy
   authorization (owner: `-C o`).

## What is NOT claimed

- No claim that real-TPM sealing has been measured on this machine (the
  `tss` group is not available to the build user).
- No claim that cross-hardware unseal fails (G5.5 WITHHELD — one machine).
- No claim that PCR-bound keys survive reboot on real hardware (PCR values
  change on reboot by design; the key is intentionally unsealable after
  reboot — this is the security property, not a failure).
- No change to the AES-256-GCM template encryption — only the key wrapping
  layer changed.
