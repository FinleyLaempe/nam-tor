# Privacy-Safe Diagnostics & Support Bundle (Task 40)

**Spec:** §8.5 operator diagnostics, §10 privacy
**Task:** 40 — Finish privacy-safe diagnostics and support bundle
**Commit:** `feat(cli): privacy-safe diagnostics bundle`
**Date:** 2026-08-08
**Crate:** `namtor-cli`

## Scope

`namtor-cli diagnostics` generates a privacy-safe support bundle safe to
share publicly. The bundle carries operator-coded diagnostics, redacted
config, version info, systemd status, model info (name/size/SHA-256), and
filtered log excerpts. No biometric data, raw scores, embeddings,
thresholds, enrollment images, or template data can appear.

## CLI

```
namtor-cli diagnostics [--out-dir <path>]
```

Writes a `namtor-bundle-<unix-ts>/` directory containing seven files.
Default output directory is `.` (current working directory).

## Bundle contents

| File | Content | Privacy gate |
|---|---|---|
| `diagnostic.txt` | Doctor report: device, emitter, protection, storage, model, package, API checks with operator codes (E001–E005, W001–W004) | PrivacyScanner before write |
| `config.txt` | Redacted config: path presence flags, vid:pid, auth outcome token | PrivacyScanner before write |
| `README.txt` | Bundle description, privacy notice | PrivacyScanner before write |
| `version.txt` | CLI version (`CARGO_PKG_VERSION`), daemon version (`namtor-daemon --version`), systemd unit status (`systemctl is-active/is-enabled`) | PrivacyScanner before write |
| `models.txt` | Per-model: filename, size in bytes, SHA-256 (colon-separated byte pairs) | PrivacyScanner before write |
| `logs.txt` | Filtered journalctl excerpts (last 200 lines, biometric lines removed) | PrivacyScanner before write |

## Privacy enforcement: two layers

### Layer 1 — Log line filter

`filter_log_lines` removes any log line whose lowercase form contains a
forbidden term: `embedding`, `template`, `pixel`, `frame`, `crop`,
`threshold`, `nonce`, `score`, `biometric`. This is a first-pass removal
so clean diagnostic lines survive while biometric lines are stripped.

### Layer 2 — PrivacyScanner (write gate)

Every file is scanned by `PrivacyScanner` **before** it is written to
disk. The scanner rejects:

- Forbidden terms (`embedding`, `template`, `pixel`, `frame`, `crop`,
  `threshold`, `nonce`)
- Hex sequences (`0x` + 8+ hex digits — keys, hashes, nonces)
- 32+ contiguous hex digits (raw key/hash without prefix)
- PEM key markers (`-----BEGIN ... KEY-----`)
- Raw score patterns (`score=` + digit)

If any file fails the scan, bundle creation aborts with a
`BundleError::Privacy` and the partially-written bundle directory is left
for the caller to clean up. The scan runs before `fs::File::create`, so
no rejected content reaches disk.

### SHA-256 formatting

Model file SHA-256 hashes are formatted as colon-separated byte pairs
(`aa:bb:cc:dd:...`) rather than contiguous hex. This prevents the
privacy scanner from mistaking a 64-char model hash for raw key material
(the scanner rejects 32+ contiguous hex digits). Colons break the
contiguous run to a maximum of 2 hex chars per segment.

## What the bundle EXCLUDES

- Embeddings or feature vectors
- Raw match scores or numeric threshold values
- Enrollment images, crops, or template data
- Frame pixel data
- Nonces, session tokens, or key material
- Biometric identifiers of any kind

## API surface

| Item | Purpose |
|---|---|
| `BundleInfo` | Runtime-collected extras: CLI/daemon version, systemd status, model info, log excerpts |
| `ModelInfo` | Per-model: name, size_bytes, sha256 (colon-separated) |
| `collect_model_info(&Path) -> Vec<ModelInfo>` | Reads `.onnx` files, computes size + SHA-256 |
| `filter_log_lines(&str) -> String` | Removes lines containing biometric terms |
| `collect_systemd_status(&str) -> String` | Queries `systemctl is-active/is-enabled` |
| `get_daemon_version(&Path) -> String` | Runs `namtor-daemon --version` |
| `create_bundle(&report, &cfg, &info, &out_dir)` | Writes 7-file bundle, all files privacy-scanned |

## Test coverage

| Test | Asserts |
|---|---|
| `bundle_creation_works` | All 7 files created |
| `bundle_passes_privacy_scanner` | Every file in a default bundle passes the scanner |
| `bundle_rejects_content_with_biometric_term` | Forbidden term in config → `BundleError::Privacy` |
| `bundle_with_full_extras_passes_privacy_scan` | Bundle with model info + filtered logs passes scanner; logs.txt has clean lines only |
| `bundle_rejects_unfiltered_log_with_biometric_data` | Unfiltered log with "embedding" → `BundleError::Privacy` |
| `filter_log_lines_removes_biometric_terms` | Embedding/score/threshold/template lines removed; clean lines kept |
| `filter_log_lines_preserves_clean_logs` | Clean log passes through unchanged |
| `collect_model_info_reads_onnx_files` | Only `.onnx` files collected; sorted by name; size + sha256 populated |
| `collect_model_info_missing_dir_returns_empty` | Missing dir → empty vec, no panic |
| `model_sha256_does_not_trigger_privacy_scanner` | Colon-separated SHA-256 passes scanner |

**Test count:** 10 new tests (60 total in `namtor-cli` unit suite). All
passing. Clippy clean, fmt clean.
