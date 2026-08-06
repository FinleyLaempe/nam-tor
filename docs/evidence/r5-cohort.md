# R5 — Cohort + Per-Profile Score Normalisation Evidence

**Gate:** R5 — lawful cohort selected; z-norm/T-norm calibration per model/device/profile
**Status:** `partial` — calibration interface + synthetic bootstrap shipped; production cohort WITHHELD (no licence-verified dataset selected).
**Date:** 2026-08-06
**Task:** 16 — Establish lawful cohort and per-profile score normalisation
**Commit:** `feat(recog): calibrate profile scores`

## What shipped

### Cohort interface (`crates/namtor-daemon/src/znorm.rs`)

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

Two implementations:

| Implementation | `licence_verified` | `non_production` | Use |
|---|---|---|---|
| `SyntheticCohort` | `true` (no licence applies to generated data) | `true` | Unit tests, structural wiring — never a live threshold |
| `FileCohort` | from caller-supplied `Provenance` | from caller | Production, *only* when `licence_verified == true` |

`SyntheticCohort` generates deterministic L2-normalised 128-d embeddings via
an in-process xorshift64 PRNG (no external `rand` at the call site, though
`rand` remains a workspace dep for other modules).

### Calibration

`ZNormCalibrator` owns the current `(model_id, device, profile)` context and
exposes:

- `calibrate(&self, source: &dyn CohortSource) -> Result<ZNormCalibration, ZNormError>` — pure computation of per-dimension mean/std over the cohort embeddings.
- `set_model_id` / `set_device` / `set_profile` — update context; mark stale when the new value differs from the last confirmed value.
- `confirm_calibration(&mut self)` — snapshot the current context as the calibrated context and clear the stale flag.
- `invalidate(&mut self)` — force stale (manual reload / re-plug).
- `is_stale(&self) -> bool` — true if never confirmed, explicitly invalidated, or context changed since confirmation.

`ZNormCalibration` records `mean: Vec<f32>`, `std: Vec<f32>`,
`cohort_source: String`, `calibrated_at: Option<Instant>`, and a
`non_production: bool` flag so the daemon can refuse to back a live auth
threshold with a synthetic calibration.

### Score normalisation functions

- `normalize_score(raw: f32, cal: &ZNormCalibration) -> f32` — scalar z-norm using the central tendency of the per-dimension stats: `(raw - mean_of(cal.mean)) / mean_of(cal.std)`, with a degenerate-std passthrough matching `verify.rs`.
- `t_norm(scores: &[f32], cal: &ZNormCalibration) -> f32` — normalise each raw score then average.

### Legal / owner gate

`calibrate()` consults `source.provenance()` first:

- `licence_verified == false` → returns `ZNormError::Withheld` *without* reading the file. An unlicensed `FileCohort` therefore WITHHOLDS even if its path is missing — no file I/O on a refused cohort.
- `licence_verified == true` + `non_production == true` (synthetic) → calibration succeeds but `ZNormCalibration.non_production` is `true`.
- `licence_verified == true` + `non_production == false` (licensed real dataset) → production-eligible calibration.

Per-model/device/profile invalidation: any of `set_model_id`, `set_device`,
`set_profile` with a value differing from the last confirmed value sets the
stale flag; the next auth path must recompute before trusting a threshold.

## WITHHELD status

The production cohort gate is **WITHHELD**:

- No licence-verified dataset has been selected as the z-norm background cohort. `ml/DATASETS.md` is the decision ledger; until a dataset is recorded there and accepted via the licence flow (`NAMTOR_ACCEPT_LICENCE=1`), no `FileCohort` can be constructed with `licence_verified == true`.
- The synthetic bootstrap is explicitly `non_production`. It can drive the 21 unit tests in `znorm.rs` and the daemon's structural wiring, but a `ZNormCalibration` it produces carries `non_production: true` and must never feed `VerifyEngine`'s match threshold for a real user.
- Consequently `verify.rs` continues to treat z-norm parameters as optional (`record.meta.znorm_mean` / `znorm_std`), falling back to `DEFAULT_MATCH_THRESHOLD` (0.45) when absent. R5 does not promote a real user to a per-profile threshold until a lawful cohort is calibrated.

## Test coverage (21 tests, all green)

| Test | Behaviour locked |
|---|---|
| `synthetic_cohort_produces_calibration` | Synthetic cohort calibrates; mean/std dim = 128 |
| `synthetic_cohort_is_non_production` | `provenance().non_production == true` and `calibration.non_production == true` |
| `synthetic_embeddings_are_l2_normalised` | Generated embeddings are unit-norm |
| `znorm_normalizes_scores_raw_above_mean_is_positive` | raw > mean → positive |
| `znorm_raw_below_mean_is_negative` | raw < mean → negative |
| `znorm_zero_std_is_passthrough` | degenerate std → raw returned |
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

Run: `cargo test -p namtor-daemon --lib znorm` (21 passed, 0 failed).

## What unblocks the gate

1. **Owner decision:** select a licence-verified dataset as the background cohort (record in `ml/DATASETS.md`).
2. **Licence acceptance:** run the daemon with `NAMTOR_ACCEPT_LICENCE=1` once the dataset licence is recorded.
3. **Cohort build:** convert the dataset embeddings to the `FileCohort` JSON shape (`[{"vector": [f32, ...]}, ...]`).
4. **Measurement:** calibrate per `(model_id, device, profile)` and measure FAR/FRR at the resulting z-normed threshold (R3 / §6.6 Tier 3 gate).

Until those four steps land, R5 stays WITHHELD and `VerifyEngine` falls back to the global default threshold.
