# G3 — Biometric Evaluation Gate Suite

SPDX-License-Identifier: GPL-3.0-only

Spec reference: §12 (G3 acceptance gates), §6.6 (recognition pipeline).
Task: 43 — Run G3 biometric evaluation.
Date: 2026-08-08.

## Gate targets

| Sub-gate | Target | Source |
|---|---|---|
| G3.1 | FAR <= 1/100,000 at FRR <= 3% | spec §12 |
| G3.2 | Reported with N and 95% confidence intervals | spec §12 |

## Status: WITHHELD

No lawful cohort dataset is available. No FAR/FRR number is published. The
recognition pipeline, z-norm calibration interface, and evaluation protocol
are implemented and tested, but the production measurement has not run.

## What is ready

### Recognition pipeline

`namtor_recog` ships the full detection-to-embedding pipeline:

- **YuNet 2023mar** face detection (5-pt landmarks, NMS, 3 strides), 232 KB
  ONNX, BSD-3 licence.
- **5-pt similarity transform** alignment to 112x112.
- **SFace 2021dec** 128-d L2-normalised embedding, 37 MB ONNX, BSD-3 licence.
- **Cosine similarity** matching against enrolled templates, max selection.
- Full pipeline: detect -> align -> embed -> match.

`VerifyEngine` in `namtor_daemon` captures On/Off frame pairs, computes
`D = On - Off`, runs analytic PAD, detects+aligns+embeds, matches against
enrolled templates, applies z-norm, and thresholds.

### Z-norm score normalisation

`namtor_recog::znorm` ships the calibration and normalisation interface
(Task 29, `docs/evidence/trust/znorm.md`):

- `CohortSource` trait with `Provenance` (licence_verified, non_production,
  consent_record). Three implementations: `SyntheticCohort` (tests),
  `FileCohort` (single file), `DirCohort` (directory of JSON shards).
- `ZNormCalibrator` with per-(model_id, device, profile) context and stale
  invalidation on context change.
- `ZNormCalibration` records mean, std, cohort_count, cohort_sha256
  (reproducibility hash), and `non_production` flag.
- `calibrate()` refuses unlicensed cohorts (`ZNormError::Withheld`) without
  touching the file/directory.
- Daemon verify wiring: `normalize_score_scalar` applied in `verify.rs`;
  absent z-norm params fall back to `DEFAULT_MATCH_THRESHOLD` (0.45).
- 31 z-norm tests + 2 daemon bridge tests, all green.

### Evaluation protocol

`namtor_eval::protocol` enforces the subject-disjoint protocol:

- No subject appears in both train and test slices (`identity_overlap_is_rejected`).
- `min_impostor_comparisons >= 100,000` (reportability floor).
- `min_genuine_per_condition >= 1,000`.
- `allow_averaged_attacks = false` (per-attack reporting enforced).
- `validate_protocol` runs all checks before any measurement.

### FAR claim eligibility (rule of three)

The FAR <= 1e-5 claim has a stricter floor than the reportability floor. At
zero observed false accepts, the 95% upper bound under the rule of three is
`3 / n`. The claim is WITHHELD until `n >= 300,000` impostor comparisons
AND zero false accepts, which makes the upper bound <= 1e-5.

A 100k run with zero false accepts publishes a point estimate of 0 with a
3e-5 upper bound and `far_claim_eligible = false`. The 100k floor is the
minimum for G3 to be reportable at all, not the floor for the claim. This
is asserted by `rule_of_three_upper_bound_at_100k_is_above_1e5` and
`rule_of_three_upper_bound_at_300k_meets_1e5`.

## What blocks the production run

| Blocker | Owner | Status |
|---|---|---|
| R5 cohort dataset (licence-verified) | human | WITHHELD: no dataset selected. Candidates in `docs/evidence/r5-cohort.md` (CASIA NIR-VIS 2.0, LAMP-HQ, BUAA-VisNir, Oulu-CASIA NIR-VIS). |
| Model licences accepted | human | WITHHELD: `NAMTOR_ACCEPT_LICENCE=1` not set |
| Python onnxruntime binding | human | WITHHELD: Python 3.14 incompat blocks INT8 eval tooling |

## What is NOT claimed

- No FAR/FRR number is published.
- No DET curve is published.
- No operating point is calibrated against real data.
- The synthetic z-norm bootstrap is explicitly `non_production` and must
  never feed a live threshold.
- SFace was selected as the production embedding model over the spec's
  AdaFace R50 target through architectural grounds (BSD-3 licence, smaller,
  CPU-friendly). The spec amendment is recorded in the R4 report. This
  selection is not a G3 pass; it is a model choice.

## Summary

| Sub-gate | Status | Evidence |
|---|---|---|
| G3.1 FAR <= 1e-5 at FRR <= 3% | WITHHELD | Protocol validated; no cohort dataset. 300k impostor comparisons needed. |
| G3.2 Reported with N + CIs | WITHHELD | `G3Report` schema ready; no production report published. |

G3 stays WITHHELD until a licence-verified cohort dataset is selected, model
licences are accepted, and the subject-disjoint evaluation runs with
>=300,000 impostor comparisons.
