# Model Acquisition & Licensing — Production-Safety Evidence

**Task:** 35 — Make model acquisition and licensing production-safe
**Date:** 2026-08-08
**Status:** Implemented. Live network fetch is WITHHELD (no operator run with
`NAMTOR_ACCEPT_LICENCE=1` against the real upstream URLs in this session); all
logic is proven by unit tests.

## Scope

Three ONNX models are required at daemon startup. None are committed to git;
all are acquired out-of-band and validated at every boot. This document records
which models, where from, under what licence, and the gates that make
acquisition production-safe.

## Models

| Model id | Purpose | Upstream | Licence | Size | SHA-256 (fp32) |
|---|---|---|---|---|---|
| `yunet_2023mar` | Face detection (5-pt landmarks, NMS) | OpenCV Zoo | MIT | 232 KB | `8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4` |
| `sface_2021dec` | Face embedding (128-d, L2-normalised) | OpenCV Zoo | Apache-2.0 | 37 MB | `0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79` |
| `minifasnet_pad` | Strobe-differential liveness (PAD) | Self-trained on physics-based synthetic differentials | Apache-2.0 (code ref: Silent-Face) | 49 KB | `3f86c8e4bb1138098ed2f8dc1682c6e1e1d6c5fffcf65dd6955c0cd60579a155` |

Source of truth: [`packaging/model-manifest.toml`](../../../packaging/model-manifest.toml).
The daemon loads this at startup and validates every deployed model against it
(`namtor_daemon::models::ModelManifest::parse`).

## On-disk layout

```
/var/lib/namtor/models/<model_id>/fp32.onnx
/var/lib/namtor/models/<model_id>/fp32.sha256
/var/lib/namtor/models/<model_id>/int8.onnx      (optional)
/var/lib/namtor/models/<model_id>/int8.sha256    (optional)
/var/lib/namtor/models/<model_id>/licence.accepted
```

## Acquisition flow

```
Operator (explicit, one-time)             Daemon (every boot)
─────────────────────────────             ─────────────────────
NAMTOR_ACCEPT_LICENCE=1                   load model-manifest.toml
  ↓                                       for each required ModelSpec:
namtor-cli / packaging script              validate_against_manifest()
  fetch(spec, source_url)                    1. sha256(on-disk) == manifest
  → download to <id>/fp32.onnx.tmp           2. licence.accepted exists &
  → verify sha256 == spec.sha256                sha256 matches manifest
  → rename .tmp → fp32.onnx                  3. restricted → allow_restricted
  → write fp32.sha256                      if all pass → load; else fail-closed
```

**No silent downloads.** The daemon's startup path (`validate_against_manifest`)
never touches the network — it validates already-present files. `fetch()` is an
explicit operator action (CLI / packaging script), not invoked automatically by
the daemon at boot. A missing model blocks startup with an error.

## Production-safety gates

### 1. SHA-256 verification (tamper detection)

Every downloaded model is hashed before the `.tmp` is renamed to the final
path. A mismatch removes the `.tmp` and returns an error — no partial or
tampered file ever reaches `fp32.onnx`.

At boot, `validate_against_manifest` re-hashes the on-disk file and compares
against the manifest variant's sha256. A mismatch (tampered, corrupted, wrong
model) blocks startup with `DaemonError::State("tampered model ...")`.

**Test:** `fetch_rejects_tampered_sha256`, `fetch_redownloads_on_hash_mismatch`,
`manifest_validate_blocks_tampered_sha256_mismatch`.

### 2. Licence acceptance (explicit, fail-closed)

A model is never loaded unless `licence.accepted` exists on disk. The record is
written only when `NAMTOR_ACCEPT_LICENCE=1` is set in the environment at the
time of acquisition. The record binds `model_id + licence_url + licence_sha256
+ accepted_by + accepted_at_us`.

At boot, `ensure_licence_accepted` checks the record's `licence_sha256`
matches the manifest — a licence text change forces re-acceptance. Without the
record and without the env var, the daemon returns
`DaemonError::State("licence not accepted for model ...; run with
NAMTOR_ACCEPT_LICENCE=1 to accept")` and refuses to load the model.

**Test:** `no_record_no_env_errors`, `env_set_writes_record`,
`existing_record_is_idempotent`, `mismatched_licence_sha256_errors`,
`manifest_validate_blocks_unaccepted_licence`.

### 3. Fetch-failure cleanup (no partial state)

If `HttpFetch::fetch_to` fails (network drop, HTTP error, disk full), the
`.tmp` file is removed before the error propagates. A truncated download
cannot masquerade as a valid model on the next start — the idempotent
re-download path would re-fetch, but only because the file is absent, not
because a partial file exists with a wrong hash.

**Test:** `fetch_failure_cleans_partial_tmp` — a `FailingFetcher` writes a
partial body then errors; the test asserts neither `.tmp` nor `.onnx` remains.

### 4. Idempotent re-fetch (offline cache)

If `fp32.onnx` exists and its sha256 matches `spec.sha256`, `fetch()` returns
the path without downloading. This makes a valid offline cache (models
present + licence accepted) proceed with zero network access.

**Test:** `fetch_is_idempotent`, `manifest_validate_ok_offline_cache`.

### 5. Redistribution policy (restricted models)

Models with `redistribution_policy = "restricted"` block startup unless
`ManifestGate { allow_restricted: true }` is set. All three current models are
`redistributable`, but the gate is enforced for future restricted-licence
models (e.g. weights derived from a research-only dataset).

**Test:** `manifest_validate_blocks_restricted_without_authorisation`,
`manifest_validate_allows_restricted_with_authorisation`.

## Verification

```
$ cargo test -p namtor-daemon --lib models:: -- --test-threads=1
running 17 tests
... 17 passed; 0 failed; 0 ignored
```

Relevant licence tests:

```
$ cargo test -p namtor-daemon --lib licence:: -- --test-threads=1
running 4 tests
... 4 passed; 0 failed; 0 ignored
```

Full daemon suite:

```
$ cargo test -p namtor-daemon --lib -- --test-threads=1
... 288 passed; 0 failed; 0 ignored
```

## WITHHELD

- **Live network fetch** against the real OpenCV Zoo URLs (GitHub) was not
  performed in this session — it requires operator decision to accept the
  upstream MIT/Apache-2.0 licences (`NAMTOR_ACCEPT_LICENCE=1`) and network
  egress. The fetch path is exercised by `FileCopyFetcher` (local file) and
  `FailingFetcher` (error path) in unit tests.
- **Signature verification** of `model-manifest.toml` itself (GPG/cosign) is
  task 6 — this task defines the manifest format and daemon-side sha256 +
  licence + redistribution validation. The fields required for signing are
  present so task 6 only adds verification.
- **INT8 quantization** (`ensure_int8`) requires Python + onnxruntime; the
  error path is tested (`ensure_int8_errors_without_python`,
  `ensure_int8_errors_without_fp32_source`) but the happy path is owner-gated.

## Files

| File | Role |
|---|---|
| `crates/namtor-daemon/src/models.rs` | `ModelFetcher`, `ModelManifest`, `validate_against_manifest`, SHA-256 verification, fetch-failure cleanup |
| `crates/namtor-daemon/src/licence.rs` | `LicenceRecord`, `ensure_licence_accepted` (fail-closed, `NAMTOR_ACCEPT_LICENCE=1`) |
| `packaging/model-manifest.toml` | Signed source of truth: model ids, licence URLs, licence + variant sha256, redistribution policy, tensor contracts |
