# G5 Security Proof

Maps each G5 production gate to its test implementation, mutation testing
results, binary hashes, and the forbidden-data assertion receipt.

## Gate matrix

| Gate | Description | Test | Status |
|---|---|---|---|
| G5.1 | PAM < 500KB, no ML deps | ci structural test | PASS |
| G5.2 | Worker sandbox (seccomp+landlock) | namtor-worker sandbox tests | PASS |
| G5.3 | SO_PEERCRED on every connection | namtor-daemon acl tests | PASS |
| G5.4 | PAM_RHOST set → refuse | namtor-pam tests | PASS |
| G5.5 | No biometric data in logs | privacy scan test | PASS |
| G5.6 | No unwrap/expect in non-test code | g5_no_unwrap.rs | PASS |

Security invariants: PAM module contains no ML code. Worker cannot open
files/network. PAD is a veto, not a weighted term. Worker sandbox never
widens silently.

## Mutation testing

Each structural gate is mutation-tested: a targeted fault is injected, the
test must fail for the intended reason, and the mutation is fully reverted.
The full mutation log is in [`../ci-baseline.md`](../ci-baseline.md) under
"Mutation testing". The fault matrix covering security-relevant failure
modes is in [`../g5-fault-matrix.json`](../g5-fault-matrix.json).

### Fault matrix summary

`g5-fault-matrix.json` records 12 security faults with injected condition,
expected behaviour, and test name:

| Fault | Expected | Test | Status |
|---|---|---|---|
| worker cannot open files (landlock) | EPERM | `landlock_denies_open` | existing |
| worker cannot open sockets (seccomp) | SIGSYS / EPERM | `seccomp_denies_socket` | existing |
| PAM_RHOST set causes auth refusal | `Verdict::Refused` | `rhost_set_returns_refused` | existing |
| unauthorised uid connection closed | connection closed, no dispatch | `unauthorised_uid_connection_closed` | existing |
| replayed nonce rejected | `Err(Acl)` on second call | `replayed_nonce_fails` | existing |
| no biometric data in logs at any level | no embedding/template/pixel tokens | `e2e_no_biometric_data_in_logs` | existing |
| tampered model weights rejected (sha256) | fetch/manifest validation error | `fetch_rejects_tampered_sha256` | existing |
| concurrent nonce validation race-free | exactly one Ok, one Err | `concurrent_validate_same_nonce_one_wins` | pass |
| random nonce cannot forge match | all 1000 candidates fail | `random_nonces_do_not_match_issued` | pass |
| spoofed non-root/non-auth-gid rejected | non-root rejected, root accepted | `authorise_rejects_spoofed_non_root_non_auth_group` | pass |
| PAD veto detail stripped at trust boundary | `external_reason` returns None | `external_reason_strips_all_pad_veto_variants` | pass |
| MAX_PAYLOAD header boundary | `Truncated`, not `Oversized` | `max_payload_boundary_not_oversized` | pass |

### CI gate mutations (from ci-baseline.md)

- **Gate 1b (PAM no ML deps):** added `image = "0.25"` to namtor-pam
  Cargo.toml; grep test detected the dep and failed. Reverted.
- **Gate 2 (PAM size):** lowered `BUDGET` in `g5_structural.rs` below the
  actual binary size; test failed. Reverted.
- **Gate 3 (panic=abort):** removed `panic = "abort"` from workspace
  Cargo.toml; codegen flipped to unwind, test detected. Reverted.
- **Gate 4 (seccomp allowlist):** added `"socket"` to the seccomp allowlist;
  disjoint test failed. Reverted.
- **Gate 5 (log privacy):** added `embedding = "[0.05, 0.11]"` to a log
  line; privacy scan failed. Reverted.
- **Gate 6 (SPDX):** removed SPDX header from a source file; check-spdx.sh
  failed. Reverted.
- **Gate 7 (cargo audit):** added `time = "0.1"` (known RUSTSEC advisory);
  audit-check flagged `RUSTSEC-2020-0071`. Reverted.

## Binary and config hashes

Release build of the PAM module, produced by `cargo build --release -p
namtor-pam`:

| Artifact | SHA-256 | Size |
|---|---|---|
| `libnamtor_pam.so` (release cdylib) | `108ea742e06384b01075c8bdda0e0ad5d64fcb0afc2b65d1585f6722371859c2` | 457920 bytes |
| `libpam_namtor.so` | symlink to `libnamtor_pam.so` | 16 bytes (symlink) |

Size budget: 512000 bytes (500 KB). Actual: 457920 bytes (447 KB, 87.5% of
budget). The `binary_size_under_budget` test in `g5_structural.rs` asserts
this in CI.

The hash is reproducible from a clean `cargo build --release -p namtor-pam`
on the same toolchain. Reproducible-build evidence is in
[`../packaging/reproducible-build.json`](../packaging/reproducible-build.json).

## Forbidden-data assertion receipt

The following tests assert that no biometric data (embeddings, templates,
frames, scores, thresholds, PAD detail) appears in logs, UI elements, or
crosses the trust boundary:

| Test | Crate | What it asserts |
|---|---|---|
| `e2e_no_biometric_data_in_logs` | namtor-daemon e2e | Full verify cycle with capture frames produces no `embedding=`, `template=`, `vec=`, pixel, or score tokens in log output at any level. Mutation-tested: adding `embedding = "..."` to a log line fails the test. |
| `privacy_scan_no_biometric_data` | namtor-kde unit | No KCM-facing type or QML element carries biometric data. Quality labels are coarse non-numeric buckets; coverage_percent is integer 0-100. Forbidden tokens: `embedding`, `vector`, `frame`, `score`, `threshold`, `template`, `biometric`. |
| `external_reason_strips_all_pad_veto_variants` | namtor-daemon unit | PAD veto detail (energy/gradient/specular/radial) is stripped before crossing the trust boundary. `external_reason` returns `None` for every PAD veto variant. |
| `no_ml_or_image_libs_linked` | namtor-pam g5_structural | The PAM release cdylib links no ML, ONNX, or image-decoder libraries. |

Privacy invariant: no biometric data in logs at any level. This is asserted
by a test, not by review. The forbidden token list and the scan are in
`crates/namtor-daemon/tests/e2e.rs` and `crates/namtor-kde/src/tests.rs`.
