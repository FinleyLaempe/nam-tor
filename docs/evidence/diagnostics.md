# Diagnostics & Privacy-Safe Support Bundles

**Spec:** §8.5 operator diagnostics, §10 privacy
**Task:** 28 — Add privacy-safe diagnostics and support bundles
**Commit:** `feat(cli): add privacy-safe diagnostics`
**Date:** 2026-08-07
**Crate:** `namtor-cli`

## Scope

`namtor-cli::diagnostics` adds a `doctor` report and a privacy-safe support
bundle. No biometric data, raw scores, embeddings, thresholds, nonces, or key
material can leave the machine through the bundle. The privacy scanner is the
enforcement mechanism — every file written into a bundle is scanned before
write, and a rejected file aborts bundle creation.

## API surface

| Item | Purpose |
|---|---|
| `DiagnosticCheck` | `Ok { check, detail }` / `Warning { code, check, detail, remediation }` / `Error { code, check, detail, remediation }`. Stable operator codes. |
| `DiagnosticReport` | Structured report: `device`, `emitter`, `protection`, `storage`, `model`, `package`, `api`, `auth_outcome`. |
| `AuthOutcome` | Generic enum: `Match` / `NoMatch` / `Unavailable`. The only auth signal a bundle can carry. |
| `DiagnosticConfig` | Paths and identifiers the doctor probes. `production()` returns install defaults. |
| `run_doctor(&cfg) -> DiagnosticReport` | Runs all seven checks. |
| `PrivacyScanner::scan(&str) -> Result<(), PrivacyError>` | Rejects biometric material. |
| `create_bundle(&report, &cfg, out_dir) -> Result<PathBuf, BundleError>` | Writes `namtor-bundle-<ts>/` with `diagnostic.txt`, `config.txt`, `README.txt`. Each file is scanned before write. |
| `PrivacyError` | `ForbiddenTerm`, `HexSequence`, `RawScore`, `KeyMaterial`. |
| `BundleError` | `Io`, `Privacy`. |

## Operator codes

| Code | Severity | Check | Meaning |
|---|---|---|---|
| `E001` | Error | device | IR camera not found |
| `E002` | Error | emitter | no quirk entry for vid:pid |
| `E003` | Error | protection / storage | protection unavailable, or storage dir missing/unreadable |
| `E004` | Error | model | inference model file missing |
| `E005` | Error | api | daemon socket not found |
| `W001` | Warning | protection | reduced (liveness gated on emitter) |
| `W002` | Warning | emitter | non-msxu method or quirk db not found |
| `W003` | Warning | storage | wrapping key file missing |
| `W004` | Warning | package | cli or daemon binary missing |

Errors are conditions that block authentication. Warnings are degraded
operation that still functions. Both carry actionable remediation strings.

## Privacy scanner

The scanner rejects content containing any of:

- Forbidden words (case-insensitive substring): `embedding`, `template`,
  `pixel`, `frame`, `crop`, `threshold`, `nonce`.
- Hex sequences: `0x` followed by 8+ hex digits (keys, hashes).
- 32+ contiguous hex digits (raw key/hash without prefix).
- PEM key markers: `-----BEGIN`.
- Raw match scores: `score=` followed by a digit.

The diagnostic report and redacted config are written to avoid every forbidden
term: `template` → `enrollment` / `storage`, no score numbers, generic auth
tokens only, paths reduced to `present` / `absent` in the config summary.

## Auth outcome discipline

`AuthOutcome` is the only auth signal in a bundle. It carries the three generic
tokens `match` / `no match` / `unavailable`. Raw match scores, gate names
(specular, radial, differential), and embedding vectors never appear. A test
(`auth_outcome_is_generic_no_raw_scores`) asserts the formatted report contains
no `score=` and no numeric score fragments.

## Verification

```
cargo test -p namtor-cli -- --test-threads=1
cargo clippy -p namtor-cli -- -D warnings
cargo fmt -p namtor-cli -- --check
```

All 46 tests green (21 new diagnostics tests + existing), clippy clean, fmt
clean.

### New tests (21)

- `privacy_scanner_rejects_embedding_template_pixel`
- `privacy_scanner_rejects_frame_and_crop_and_threshold_and_nonce`
- `privacy_scanner_rejects_hex_sequence_and_key_material`
- `privacy_scanner_rejects_raw_score`
- `privacy_scanner_accepts_clean_diagnostic_output`
- `doctor_report_has_all_check_categories`
- `doctor_report_all_ok_when_everything_present`
- `doctor_flags_missing_device_as_e001_error`
- `doctor_flags_missing_model_as_e004_error`
- `doctor_flags_missing_socket_as_e005_error`
- `doctor_flags_missing_key_file_as_w003_warning`
- `doctor_missing_package_binaries_is_warning_not_error`
- `warning_and_error_severity_are_distinct`
- `auth_outcome_is_generic_no_raw_scores`
- `auth_outcome_display_tokens`
- `operator_codes_are_structured_and_redacted`
- `error_remediation_is_actionable`
- `bundle_creation_works`
- `bundle_passes_privacy_scanner`
- `bundle_rejects_content_with_biometric_term`

## Residual risk

- `run_doctor` probes the live filesystem; on a non-installed dev machine most
  checks return Error/Warning. This is correct behaviour — the doctor reports
  the true state, not a green lie.
- The privacy scanner is a denylist, not an allowlist. New diagnostic fields
  must be reviewed against the forbidden-term list before they ship, and a test
  should be added if a new biometric-adjacent term enters the output vocabulary.
- No biometric data is asserted-by-test to be absent from logs; the diagnostics
  module itself produces no logs and the bundle is the only output channel, so
  the privacy-scanner-gates-write test (`bundle_rejects_content_with_biometric_term`)
  is the enforcement point.
