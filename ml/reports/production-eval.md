# Production evaluation: G2-G4 gate evidence

**Status: PROTOCOL DEFINED, GATES WITHHELD.** This document records the
evaluation protocol implemented in `crates/namtor-eval/src/protocol.rs` and
the status of each production gate. No gate number is published here because
no production run has been executed against the enrolled reference hardware
yet. The protocol is the contract the harness enforces before any number is
published; the reports are the artefacts a run produces.

Reference: `docs/superpowers/specs/2026-08-04-nam-tor-design.md` §12.

## Provenance

| Field | Value |
|---|---|
| Commit at protocol definition | `2acd9b4e96a2697015dd619ba3fad5840aa42780` |
| Model hashes | YuNet 2023mar (232 KB, MIT), SFace 2021dec (37 MB, Apache-2.0), MiniFASNet PAD (49 KB, self-trained). Full SHA-256 recorded at run time in `EvalConfig.model_hashes`. |
| Hardware hash | Computed at run time from sysfs USB descriptors; recorded in `EvalConfig.hardware_hash`. Reference device: ASUS ROG Flow X16 GV601VI, Sonix `3277:0018`. |

## Protocol (enforced by `validate_protocol`)

1. **Subject-disjoint partition.** No subject may appear in both the train and
   test slices. The validator collects the union of `subject_ids` across all
   `IdentitySlice`s and rejects any overlap. Enrolled subjects are not reused
   as impostors.
2. **Minimum impostor comparisons.** `min_impostor_comparisons >= 100_000`
   (`G3_MIN_IMPOSTOR_COMPARISONS`). This is the floor at which G3 is
   *reportable*; it is **not** the floor at which the FAR ≤ 1e-5 *claim* is
   publishable (see FAR rule below).
3. **Minimum genuine comparisons per condition.**
   `min_genuine_per_condition >= 1_000` (`G3_MIN_GENUINE_PER_CONDITION`).
4. **Device slices.** Each capture condition is tagged with the physical
   device that produced it, so a report can be partitioned by device.
5. **No averaged attacks.** `allow_averaged_attacks` must be `false`. G4
   requires per-attack-type APCER; averaging across attack types is forbidden
   by spec §12 G4 and rejected by the validator.

## Gate G2 — latency

**Target:** median ≤ 500 ms, p95 ≤ 1000 ms.

`G2Report` carries the overall `median_ms` and `p95_ms` plus a
`first_boot`/`steady_state` `LatencyBand` breakdown. The split exists because
cold model load dominates the first authentication of a session and must not
be hidden inside an overall median.

**Status: WITHHELD.** No production run yet. The harness records
`LatencyBreakdown` per attempt (see `score.rs`); the G2 report aggregates
those records.

## Gate G3 — biometric accuracy

**Target:** FAR ≤ 1/100,000 at FRR ≤ 3%.

`G3Report` carries:
- `det_points`: the DET curve as a list of `(threshold, FAR, FRR)` operating
  points, so the curve can be redrawn without re-running the harness.
- `far_at_frr_3pct`: FAR at the operating point where FRR = 3%.
- `far_ci_upper_95`, `frr_ci_upper_95`: 95% Wilson score upper bounds. CI
  presence is enforced by `validate_g3_report`.
- `n_impostor_comparisons`, `n_genuine_comparisons`.
- `far_claim_eligible`: `true` iff `n_impostor_comparisons >= 300_000`.

### FAR ≤ 1e-5 rule (the rule of three)

At zero observed false accepts, the 95% upper bound on FAR under the rule of
three is `3 / n`. The FAR ≤ 1e-5 *claim* is **WITHHELD** until the sample
size makes that upper bound ≤ 1e-5, i.e. `n >= 300_000`
(`FAR_CLAIM_MIN_N`).

| `n` impostor comparisons | 0 false accepts → 95% upper bound | FAR ≤ 1e-5 claim |
|---|---|---|
| 100,000 | 3.0e-5 | WITHHELD (upper bound > 1e-5) |
| 300,000 | 1.0e-5 | eligible (point estimate still required) |

The 100k floor is the minimum for G3 to be *reportable at all*; it is not the
floor for the FAR claim. A run with 100k impostor comparisons and zero false
accepts publishes a point estimate of 0 with a 3e-5 upper bound and the
`far_claim_eligible` flag set to `false`.

**Status: WITHHELD.** No production run yet. Blocks (per
`docs/evidence/status-audit.md`): R5 z-norm cohort dataset selection
(owner-gated), Python `onnxruntime` binding compatibility for INT8 vs FP32
evaluation (owner-gated).

## Gate G4 — presentation attack detection

**Target:** APCER ≤ 5% per attack type, reported per type, never averaged.

`G4Report` carries:
- `per_attack`: one `AttackRate` per attack type with `apcer`,
  `apcer_ci_upper_95`, and `n_attacks`. The per-type split is mandatory; the
  protocol rejects `allow_averaged_attacks = true`.
- `bpcer`, `bpcer_ci_upper_95`, `n_bona_fide`.
- `evaluated_3d_mask`: always `false` on the reference hardware. Recorded so
  the report is self-describing about what was *not* tested.

### 3D masks

3D mask attacks are **out of scope** for the reference hardware, which has no
depth sensor. The analytic PAD gates (differential energy, gradient
consistency, specular highlight, radial falloff; see `namtor-pad`) are
designed against 2D presentations (screen replay, print, cutout). A 3D mask
that reproduces the strobe-differential signal would defeat analytic PAD, and
no claim is made against it. `G4Report.evaluated_3d_mask` is `false` and the
report explicitly states this.

**Status: WITHHELD.** Analytic PAD gates are uncalibrated (task 21); learned
MiniFASNet PAD is synthetic-only (task 22). A live-attack cohort with
recorded consent is required before any G4 number is published.

## CI-enforced structural gates

The following are asserted by tests, not by review, and pass on `master`:

- G5.1: `pam_namtor.so` contains no ML code and is < 500 KB.
- G5.2: worker cannot open files/network/unauthorised syscalls (seccomp +
  landlock).
- G5.4: no biometric data in logs at any level (asserted by a test).

These are structural gates independent of the G2-G4 evaluation. They do not
substitute for the measured gates above.

## How to run a production evaluation

1. Enroll subjects under consent. Record subject IDs in the
   `subject_disjoint` partition; no subject may appear in both slices.
2. Configure `EvalConfig` with the current `commit_hash`, the SHA-256 of each
   model file, and the hardware hash from sysfs.
3. Run the harness. It emits `ScoreRecord` JSONL (see `score.rs`).
4. Aggregate into `G2Report`, `G3Report`, `G4Report`.
5. Call `validate_protocol` and `validate_g3_report`. If either fails, the
   run is not publishable.
6. Attach the reports and the `EvalConfig` to this document as a dated
   appendix. Only then is a gate number published.
