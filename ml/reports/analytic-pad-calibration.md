# Analytic PAD Calibration — Task 21

**Status: scaffolding landed; G4.1 WITHHELD pending owner-gated corpus.**

## Scope

Task 21 calibrates the four analytic PAD gates from spec §6.4 (differential
energy, gradient consistency, specular highlight, radial falloff) on real
device/attack data, selecting per-gate thresholds at a **predeclared BPCER ≤
5%** and reporting **APCER per attack type** (print, IR-print, screen-replay,
cutout). PAD remains a veto — thresholds tune each gate's pass/fail boundary;
they are never combined into a weighted score (spec §6.6). Unsupported attack
classes are reported as WITHHELD, never silently averaged.

## What shipped

`crates/namtor-pad/src/calibration.rs` (new, ~770 LOC incl. tests):

| Type | Purpose |
|---|---|
| `GateId` | Enum of the four analytic gates. |
| `AttackClass` | Enum: `Print`, `IrPrint`, `ScreenReplay`, `Cutout`, `Unsupported`. |
| `GateDistribution` | Per-gate summary stats: `mean`, `std`, `min`, `max`, `n`. Built from a score slice via `from_scores`; `None` for empty input (WITHHELD). |
| `Provenance` | Dataset, licence, consent, synthetic-flag — every report is self-auditing. |
| `CalibrationData` | Per-gate distributions for live + each attack class, plus provenance. |
| `GateDistributions` | The four-gate bundle for one class. |
| `GateThreshold` | Per-gate chosen threshold + empirical BPCER + n_live + 95% CI half-width. |
| `ApcerResult` | Enum: `Evaluated { apcer, n, ci_half }` or `Withheld`. |
| `ThresholdMethod` | Enum: only `PredeclaredBpcerLe5` — no post-hoc threshold shopping. |
| `CalibrationReport` | Full report: 4 thresholds, 4×5 APCER matrix, method, target, provenance. |
| `DeviceOverrides` | Per-device quirk overrides with mandatory justification (`all_justified()`). |
| `bpcer_threshold` | Smallest live-cohort score keeping empirical BPCER ≤ target. |
| `empirical_bpcer` / `empirical_apcer` | Empirical CDF estimators. |
| `ci_half` | Normal-approximation 95% half-width. |
| `calibrate` | Summary-only fallback: threshold = live mean, APCER via normal-CDF estimate. |
| `calibrate_from_scores` | **Preferred**: threshold from live empirical CDF, APCER from attack empirical CDFs. No distributional assumption. |
| `ScoreCohort` | Raw per-gate score vectors for one class. |

13 tests covering: distribution summary, empty/single-sample edges, threshold
selection (off-by-one fixed: `k = floor(target·n)` so BPCER = k/n ≤ target),
APCER counting, full `calibrate_from_scores` with withheld classes,
all-withheld path, device overrides + justification enforcement, CI half-width
edges, normal-CDF step/symmetry, enum round-trips, summary-fallback withheld.

## Threshold selection — predeclared BPCER

The rule is fixed before any data is seen: pick the smallest live-cohort score
`t` such that empirical BPCER(t) ≤ 5%. Implementation: sort live scores, take
the sample at rank `k = floor(0.05 · n)` (0-indexed) as the threshold; exactly
`k` samples fall strictly below, giving BPCER = k/n ≤ 0.05. No distributional
assumption, no optimisation over attack performance, no post-hoc adjustment.
The method is recorded in the report as `PredeclaredBpcerLe5`.

`calibrate` (summary-only) is a fallback for when only summary statistics
survive: it uses the live mean as the threshold and estimates APCER from a
normal-CDF tail. It is explicitly inferior — full-precision work uses
`calibrate_from_scores`.

## PAD is a veto

`CalibrationReport` carries per-gate thresholds and per-gate × per-attack
APCER. It does **not** combine gates. The daemon's `run_analytic_pad`
(`lib.rs`) already vetoes the verdict if any single gate fails; calibration
only tunes where each gate's pass/fail line sits. There is no weighted score,
no "average APCER", no aggregation that could hide a weak gate behind strong
ones.

## Device overrides

`DeviceOverrides` lets a quirk (e.g. an under-driven emitter producing a weaker
`D`) replace a per-gate threshold with a stated value. Each override carries a
mandatory justification string; `all_justified()` returns false if any
non-None override has an empty reason. A report that applies an unjustified
override is invalid by construction — the caller must check `all_justified()`
before applying.

## G4.1 status: WITHHELD

G4.1 requires APCER ≤ 5% per print/IR-print/replay at BPCER ≤ 5%, **measured**.
The calibration machinery is in place, but:

- **No real attack corpus has been collected.** The owner-gated inputs listed
  in `docs/evidence/status-audit.md` (consented human subjects + attack props)
  are WITHHELD.
- The 13 tests use synthetic score distributions to lock the arithmetic, not
  to claim measured APCER.
- No APCER number from this module is publishable as a G4.1 pass. The report
  structure distinguishes `Evaluated` (real cohort) from `Withheld` (no
  cohort) so a missing class cannot be mistaken for a passing one.

When a real corpus lands, the path is: collect per-gate scores per class →
populate `ScoreCohort`s → call `calibrate_from_scores` → publish the
`CalibrationReport`. The threshold-selection rule and the APCER computation do
not change.

## What this does NOT do

- Does not train or evaluate MiniFASNet (that is task 22, learned PAD).
- Does not fuse RGB+IR (task 17).
- Does not modify the existing `PadConfig` defaults in `lib.rs`; calibration
  produces a `CalibrationReport` that the daemon may consume to override
  defaults via `DeviceOverrides`, but the defaults remain the conservative
  spec-derived values.
- Does not expose thresholds in any GUI (spec hard rule).
- Does not average APCER across attack types.

## Verification

```
cargo test -p namtor-pad -- --test-threads=1   # 52 passed, 0 failed
cargo clippy -p namtor-pad -- -D warnings      # clean
cargo fmt -p namtor-pad -- --check             # clean
```

## Owner-gated inputs still required to close G4.1

1. Consent-gated collection campaign (live + print + IR-print + screen-replay
   + cutout) on the reference hardware — `namtor-collect` (M8) provides the
   consent scaffolding.
2. Licence-verified dataset for any merged background cohort.
3. Decision on whether `Unsupported` attack classes (e.g. 3D masks) are
   in-scope for G4.1 or remain permanently WITHHELD.
