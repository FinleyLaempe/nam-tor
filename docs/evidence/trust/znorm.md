# Z-Norm — Lawful Cohort + Calibrated Score Normalisation

**Gate:** R5 / G3 — lawful cohort selected; z-norm/T-norm calibration per model/device/profile
**Status:** `partial` — calibration interface, directory cohort, reproducibility hash, and daemon verify wiring shipped; **production cohort WITHHELD** (no licence-verified dataset selected).
**Date:** 2026-08-08
**Task:** 29 — Establish lawful cohort and calibrated normalization
**Commit:** `feat(recog): calibrated z-norm with cohort`

## What shipped

### Z-norm moved to `namtor-recog` (recognition concern)

The z-norm implementation previously lived in `namtor-daemon/src/znorm.rs`.
Score normalisation is a recognition concern, so the implementation now
lives in `crates/namtor-recog/src/znorm.rs` and is re-exported by
`namtor-daemon/src/znorm.rs` as a thin shim so the daemon's `crate::znorm`
path keeps resolving.

### Cohort sources

A `CohortSource` trait carries both the embedding material and its legal
provenance:

```rust
pub trait CohortSource {
    fn embeddings(&self) -> Result<Vec<Embedding>, ZNormError>;
    fn provenance(&self) -> Provenance;
}

pub struct Provenance {
    pub source: String,
    pub licence_verified: bool,
    pub non_production: bool,
    pub consent_record: Option<String>,
}
```

Three implementations:

| Implementation | `licence_verified` | `non_production` | Use |
|---|---|---|---|
| `SyntheticCohort` | `true` (no licence applies to generated data) | `true` | Unit tests, structural wiring — never a live threshold |
| `FileCohort` | from caller-supplied `Provenance` | from caller | Single-file production cohort, *only* when `licence_verified == true` |
| `DirCohort` **(new)** | from caller-supplied `Provenance` | from caller | Directory of `*.json` files (production shape); concatenated in lexicographic filename order for deterministic hashing |

`DirCohort` loads all `*.json` files from a directory (e.g.
`var/lib/namtor/cohort/`, gitignored). Each file is an array of
`{"vector": [f32, ...]}` objects. Non-`.json` files are ignored. This is
the shape production will use: a directory of cohort shards outside git.

### Calibration + reproducibility hash

`ZNormCalibrator` owns the current `(model_id, device, profile)` context and
exposes:

- `calibrate(&self, source: &dyn CohortSource) -> Result<ZNormCalibration, ZNormError>` — pure computation of per-dimension mean/std over the cohort embeddings.
- `set_model_id` / `set_device` / `set_profile` — update context; mark stale when the new value differs from the last confirmed value.
- `confirm_calibration(&mut self)` — snapshot the current context as the calibrated context and clear the stale flag.
- `invalidate(&mut self)` — force stale (manual reload / re-plug).
- `is_stale(&self) -> bool` — true if never confirmed, explicitly invalidated, or context changed since confirmation.

`ZNormCalibration` records:

| Field | Purpose |
|---|---|
| `mean: Vec<f32>` | Per-dimension cohort mean |
| `std: Vec<f32>` | Per-dimension cohort std (degenerate dims clamped to 1.0) |
| `cohort_source: String` | Provenance source identifier |
| `cohort_count: usize` **(new)** | Number of embeddings in the cohort |
| `cohort_sha256: [u8; 32]` **(new)** | SHA-256 of the cohort embedding material (canonical f32-LE encoding, in source order). Proves reproducibility: same cohort → same hash. |
| `calibrated_at: Option<Instant>` | Wall-clock of computation |
| `non_production: bool` | True iff synthetic bootstrap — never backs a live threshold |

### Score normalisation functions

- `normalize_score(raw, cal: &ZNormCalibration) -> f32` — scalar z-norm using the central tendency of the per-dimension stats.
- `normalize_score_scalar(raw, mean: Option<f32>, std: Option<f32>) -> f32` **(new)** — scalar helper for the daemon verify path, which stores z-norm params as `Option<f32>` in per-user enrollment metadata. Absent params → passthrough → global default threshold.
- `t_norm(scores: &[f32], cal: &ZNormCalibration) -> f32` — normalise each raw score then average.

### Daemon verify wiring

`crates/namtor-daemon/src/verify.rs` now calls
`namtor_recog::normalize_score_scalar(best_score, *znorm_mean, *znorm_std)`
instead of duplicating the z-norm formula inline. Behaviour-preserving:
the scalar helper matches the previous inline math exactly (asserted by
`normalize_score_scalar_matches_inline_math` and the daemon's
`verify::tests::znorm_application_logic`).

### Legal / owner gate

`calibrate()` consults `source.provenance()` first:

- `licence_verified == false` → returns `ZNormError::Withheld` *without* reading the file/directory. An unlicensed cohort therefore WITHHOLDS even if its path is missing — no file I/O on a refused cohort.
- `licence_verified == true` + `non_production == true` (synthetic) → calibration succeeds but `ZNormCalibration.non_production` is `true`.
- `licence_verified == true` + `non_production == false` (licensed real dataset) → production-eligible calibration.

Per-model/device/profile invalidation: any of `set_model_id`, `set_device`,
`set_profile` with a value differing from the last confirmed value sets the
stale flag; the next auth path must recompute before trusting a threshold.

## WITHHELD status

The production cohort gate is **WITHHELD**:

- No licence-verified dataset has been selected as the z-norm background cohort. `ml/DATASETS.md` is the decision ledger; until a dataset is recorded there and accepted via the licence flow (`NAMTOR_ACCEPT_LICENCE=1`), no `FileCohort`/`DirCohort` can be constructed with `licence_verified == true` for production.
- The synthetic bootstrap is explicitly `non_production`. It can drive the 31 unit tests in `znorm.rs` and the daemon's structural wiring, but a `ZNormCalibration` it produces carries `non_production: true` and must never feed `VerifyEngine`'s match threshold for a real user.
- Consequently `verify.rs` continues to treat z-norm parameters as optional (`record.meta.znorm_mean` / `znorm_std`), falling back to `DEFAULT_MATCH_THRESHOLD` (0.45) when absent. R5 does not promote a real user to a per-profile threshold until a lawful cohort is calibrated.

## Reproducibility proof

The cohort SHA-256 makes calibration reproducible: two `calibrate()` calls
on the same cohort source produce identical `mean`, `std`, `cohort_count`,
and `cohort_sha256` (asserted by `calibration_is_reproducible_same_hash`).
Different cohort inputs produce different hashes (asserted by
`cohort_hash_differs_for_different_cohorts`). `DirCohort` sorts filenames
lexicographically before reading, so the hash is deterministic regardless of
directory iteration order (asserted by `dir_cohort_hash_is_deterministic`).

## Centering proof

`znorm_centers_cohort_scores` proves the calibration produces centered
scores: normalising the cohort's own mean yields ~0 (`z ≈ 0` when
`raw == mean`), which is the z-norm contract — calibrated scores are
portable across environments because they are centered on the cohort.

## Test coverage (31 tests in `namtor-recog/src/znorm.rs`, all green)

| Test | Behaviour locked |
|---|---|
| `synthetic_cohort_produces_calibration` | Synthetic cohort calibrates; mean/std dim = 128, count recorded |
| `synthetic_cohort_is_non_production` | `provenance().non_production == true` and `calibration.non_production == true` |
| `synthetic_embeddings_are_l2_normalised` | Generated embeddings are unit-norm |
| `znorm_normalizes_scores_raw_above_mean_is_positive` | raw > mean → positive |
| `znorm_raw_below_mean_is_negative` | raw < mean → negative |
| `znorm_zero_std_is_passthrough` | degenerate std → raw returned |
| `znorm_centers_cohort_scores` **(new)** | normalising the cohort mean yields ~0 |
| `t_norm_averages_normalised_scores` | T-norm = mean of normalised scores |
| `t_norm_empty_is_zero` | empty input → 0.0 |
| `invalidation_on_model_change` | `set_model_id` with new value → stale |
| `invalidation_on_device_change` | `set_device` with new value → stale |
| `invalidation_on_profile_change` | `set_profile` with new value → stale |
| `same_value_setter_does_not_invalidate` | setting the same value keeps fresh |
| `explicit_invalidate_marks_stale` | `invalidate()` → stale |
| `calibrator_starts_stale` | fresh calibrator with no calibration is stale |
| `unlicensed_file_cohort_is_withheld` | `licence_verified == false` → `Withheld` |
| `path_unused_when_unlicensed` | Withheld returned without touching the file |
| `licensed_file_cohort_calibrates` | licensed file cohort → production calibration |
| `empty_cohort_errors` | `[]` → `EmptyCohort` |
| `inconsistent_dimension_errors` | mixed dims → `InconsistentDim` |
| `calibration_is_reproducible_same_hash` **(new)** | same cohort → same hash + stats |
| `cohort_hash_differs_for_different_cohorts` **(new)** | different cohorts → different hashes |
| `normalize_score_scalar_matches_inline_math` **(new)** | scalar helper == canonical formula |
| `normalize_score_scalar_zero_std_is_passthrough` **(new)** | degenerate std → raw |
| `normalize_score_scalar_missing_params_is_passthrough` **(new)** | None params → raw |
| `dir_cohort_loads_multiple_files` **(new)** | DirCohort concatenates *.json, ignores non-json |
| `dir_cohort_empty_dir_errors` **(new)** | empty dir → `EmptyCohort` |
| `dir_cohort_unlicensed_is_withheld_without_io` **(new)** | unlicensed dir → Withheld, no I/O |
| `dir_cohort_hash_is_deterministic` **(new)** | same dir contents → same hash |

Daemon verify bridge (2 tests in `namtor-daemon/src/verify.rs`):

| Test | Behaviour locked |
|---|---|
| `znorm_application_logic` | `normalize_score_scalar(0.5, 0.3, 0.1) == 2.0` |
| `znorm_with_zero_std_is_passthrough` | `normalize_score_scalar(0.5, 0.3, 0.0) == 0.5` |

Run: `cargo test -p namtor-recog` (39 passed: 31 znorm + 8 existing recog).
`cargo test -p namtor-daemon --lib -- --test-threads=1` (278 passed, including the 2 verify z-norm bridge tests).

## Cohort data never committed

- `.gitignore` excludes `var/lib/namtor/cohort/` (the production cohort directory).
- `var/` is already gitignored; the explicit line documents intent.
- No biometric data or cohort embeddings are committed. Tests use `SyntheticCohort` (generated) or `tempfile::tempdir()` ( ephemeral).
- No biometric data in logs at any level (asserted by existing privacy tests).

## What unblocks the gate

1. **Owner decision:** select a licence-verified dataset as the background cohort (record in `ml/DATASETS.md`).
2. **Licence acceptance:** run the daemon with `NAMTOR_ACCEPT_LICENCE=1` once the dataset licence is recorded.
3. **Cohort build:** convert the dataset embeddings to the `DirCohort` JSON shape (`[{"vector": [f32, ...]}, ...]` per file) under `var/lib/namtor/cohort/`.
4. **Measurement:** calibrate per `(model_id, device, profile)` and measure FAR/FRR at the resulting z-normed threshold (R3 / §6.6 Tier 3 gate).

Until those four steps land, R5 stays WITHHELD and `VerifyEngine` falls back to the global default threshold.
