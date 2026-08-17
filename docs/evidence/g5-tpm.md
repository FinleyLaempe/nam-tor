# G5.5 — TPM2-Sealed Key Management Evidence

**Gate:** G5.5 — templates unreadable off-device (TPM)
**Status:** `partial` — real TPM2 backend implemented and feature-gated; cross-hardware claim WITHHELD.
**Date:** 2026-08-06
**Task:** 11 — Implement real TPM2-sealed key management
**Commit:** `feat(store): seal keys with TPM2`

## What shipped

### KeyBackend abstraction (`crates/namtor-store/src/crypto.rs`)

The stub file-fallback at `crypto.rs:74-80` (the seam identified in the
status audit) is replaced by a `KeyBackend` trait with three implementations:

| Backend | id | `KeySource` | Feature | When used |
|---|---|---|---|---|
| `MockTpm` | `mock-tpm` | `Tpm2` | always (test) | Unit tests; simulates device binding, corruption, lockout |
| `FileBackend` | `file` | `File` | always | Production default when no TPM; reduced protection |
| `Tpm2Backend` | `tpm2` | `Tpm2` | `tpm2` (opt-in) | Real TPM2 via `tpm2-tools` CLI; requires `/dev/tpmrm0` + `tss` group |

The AES-256 key is sealed (wrapped) by the backend into a versioned
`SealedBlob` (magic `NMTKEY` + u16 version + backend-id + opaque payload).
The blob is persisted at `key_path`. The AES-256-GCM encryption of templates
(`store.rs`) is unchanged — only the key wrapping layer changed.

### Device-bound, no PCR policy (spec §7.3)

- `Tpm2Backend` uses `tpm2_createprimary -C o` (storage hierarchy) as the
  parent, then `tpm2_create` with attributes `fixedtpm|fixedparent|
  adminwithpolicy|noda`. No `-L` PCR policy is specified. The sealed object
  is non-migratable and bound to this TPM's storage hierarchy. It survives
  reboot (no PCR state dependency) but cannot be unsealed on another machine.
- `MockTpm` models the same contract: a per-instance 32-byte seed is the
  "device". Two seeds = two machines. A blob sealed by seed A fails to
  unseal with seed B (HMAC-SHA256 mismatch → distinct `Crypto` error).

### File fallback reports reduced protection

When no TPM is available (no `tpm2` feature, or `tpm2-tools` missing, or
`/dev/tpmrm0` absent), `BackendSelector::select_for_new()` falls back to
`FileBackend` and the resulting `SealedKey` reports `KeySource::File`. The
existing `namtor-cli enroll` already prints `key source: File` vs `Tpm2`,
so the fallback is never silent (spec §7.3 "degraded protection is never
silent").

### Distinct corruption vs lockout errors

- **Corruption** (tampered blob, wrong device, truncated payload): returns
  `StoreError::Crypto(...)`. MockTpm distinguishes this via HMAC-SHA256
  verification of the payload before AES-GCM decryption.
- **Lockout** (too many unseal attempts): returns `StoreError::Tpm2(...)` with
  a message containing `"lockout"`. MockTpm enforces `MAX_UNSEAL_ATTEMPTS = 5`;
  `Tpm2Backend` maps TPM2_RC_LOCKOUT (0x98b / stderr containing "LOCKOUT") to
  `StoreError::Tpm2`.

### Version/migration

`SealedBlob` carries a `u16 version` (currently 1). Migration = re-seal with
a new TPM key (new seed / new primary). The old blob's payload differs from
the new blob's payload; the old blob is rejected by the new backend (device
binding). The `mock_migrate_reseal_new_seed_old_blob_invalid` test proves
this: old blob unseal fails after re-sealing with a new seed.

## Test results

### MockTpm contract tests (CI, no hardware)

All run under `cargo test -p namtor-store -- --test-threads=1` (no `tpm2`
feature, no hardware). 21 tests pass, 0 fail.

| Test | Scenario | Result |
|---|---|---|
| `mock_seal_unseal_round_trip` | seal then unseal same key | PASS |
| `mock_restart_unseal_same_seed` | drop backend, recreate with same seed, unseal | PASS |
| `mock_migrate_reseal_new_seed_old_blob_invalid` | re-seal with new seed; old blob rejected | PASS |
| `mock_reject_moved_blob` | seal with seed A, unseal with seed B → fails | PASS |
| `mock_corruption_detected` | tamper payload byte → distinct `Crypto` error | PASS |
| `mock_lockout_after_max_attempts` | >5 unseal attempts → distinct `Tpm2` lockout error | PASS |
| `file_backend_seal_unseal_round_trip` | file backend round-trip | PASS |
| `sealed_key_mock_round_trip_via_selector` | end-to-end via `SealedKey` + `BackendSelector` | PASS |
| `sealed_key_file_round_trip_via_selector` | end-to-end file path | PASS |
| `sealed_key_default_selector_uses_file` | default (no TPM) → `KeySource::File` | PASS |
| `sealed_key_rejects_moved_blob_across_machines` | two selectors, different seeds → reject | PASS |
| `blob_decode_rejects_bad_magic` | malformed blob → `Crypto` error | PASS |

### Real-TPM tests (hardware-gated, `#[ignore]`)

`cargo test -p namtor-store --features tpm2 -- --ignored`:

| Test | Status |
|---|---|
| `real_tpm_seal_unseal_round_trip` | `#[ignore]` — requires `NAMTOR_HW_TEST=1` + `tss` group + `/dev/tpmrm0` |
| `real_tpm_rejects_corrupted_blob` | `#[ignore]` — same |

**Not run in CI.** The build user (`finley`) is not in the `tss` group and
cannot open `/dev/tpmrm0`. `swtpm` is not installed. These tests are
documented and ready for a hardware-equipped CI runner or manual execution
by a `tss`-group user.

### Build verification

- `cargo build -p namtor-store` — PASS (default, no `tpm2` feature)
- `cargo build -p namtor-store --features tpm2` — PASS
- `cargo clippy -p namtor-store -- -D warnings` — clean
- `cargo clippy -p namtor-store --features tpm2 -- -D warnings` — clean
- `cargo fmt -p namtor-store -- --check` — clean
- `cargo build -p namtor-cli -p namtor-gnome -p namtor-daemon` — PASS (dependents unchanged)

## G5.5 cross-hardware status: WITHHELD

**Claim NOT made.** The G5.5 gate requires proof that a blob sealed on
machine A fails to unseal on machine B. Only one TPM-equipped machine is
available (the ASUS ROG Flow X16, `/dev/tpmrm0`). Without a second
TPM-equipped machine, the cross-hardware unseal procedure cannot be
demonstrated.

Evidence is limited to:
1. **MockTpm contract**: two different seeds (simulating two machines)
   produce mutually-unsealable blobs (`mock_reject_moved_blob`,
   `sealed_key_rejects_moved_blob_across_machines`).
2. **Same-device unseal**: the `real_tpm_seal_unseal_round_trip` test (when
   run by a `tss`-group user) proves seal→unseal on the same TPM.

The cross-hardware claim is **WITHHELD** per the task brief: "If no second
TPM-equipped machine is available, G5.5 cross-hardware claim is WITHHELD and
evidence is limited to software-TPM plus same-device unseal — never asserted
from one machine."

## Runtime requirements for real TPM2

To use the real TPM2 backend (`--features tpm2`):

1. `tpm2-tools` installed (`tpm2` binary on PATH).
2. `/dev/tpmrm0` present (TPM 2.0 resource manager).
3. The `namtor` system user in the `tss` group (per the project's hardware
   notes: "desktop TPM access works via logind seat ACL, which does NOT
   extend to system daemons. The `namtor` system user needs real `tss`
   group membership").
4. Run `namtor-cli enroll` — it will print `key source: Tpm2` when the
   backend is active, or `key source: File` (reduced protection) when it is
   not.

## What is NOT claimed

- No claim that real-TPM sealing has been measured on this machine (the
  `tss` group is not available to the build user).
- No claim that cross-hardware unseal fails (G5.5 WITHHELD — one machine).
- No claim that the `tpm2` feature is CI-tested (it compiles clean; real-TPM
  tests are `#[ignore]`).
- No change to the AES-256-GCM template encryption — only the key wrapping
  layer changed.
