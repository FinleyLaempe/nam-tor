# G4 — Presentation Attack Detection Gate Suite

SPDX-License-Identifier: GPL-3.0-only

Spec reference: §12 (G4 acceptance gates), §6.4 (analytic PAD), §6.5 (learned PAD).
Task: 44 — Run G4 presentation-attack evaluation.
Date: 2026-08-08.

## Gate targets

| Sub-gate | Target | Source |
|---|---|---|
| G4.1 | APCER <= 5% per attack type (print, IR-print, replay) at BPCER <= 5% | spec §12 |
| G4.2 | Per attack type, never averaged | spec §12 |
| G4.3 | 3D mask documented as not evaluated | spec §12 |

## Status: WITHHELD

No real presentation-attack corpus has been evaluated. Analytic PAD gates
are calibrated on synthetic data only. Learned PAD (MiniFASNet) is
synthetic-only. Both are deployed as vetoes, not weighted terms.

## Analytic PAD (four gates on D = On - Off)

`namtor_pad` ships four analytic gates on the strobe-differential signal
`D = On - Off`:

1. **Differential energy** — live skin has higher IR reflectance than
   screens/print; `D` energy exceeds a floor.
2. **Gradient consistency** — real faces have consistent spatial gradients
   across on/off; flat prints produce inconsistent gradients.
3. **Specular highlight** — 850nm specular hotspot from the emitter
   appears on live skin, not on matte prints or screens.
4. **Radial falloff** — inverse-square (1/r^2) falloff across a real face
   gives a crude depth signal; flat prints and screens have uniform D.

PAD is a **veto**, not a weighted term. If any gate fails, authentication
fails. The veto contract is asserted by tests: PAD veto is
indistinguishable from NoMatch in the external reason
(`failure-contract.md`).

**Calibration status:** thresholds are conservative defaults, not tuned
against a real attack corpus. The radial gate is explicitly labelled
UNCALIBRATED in source. Calibration on real consented data is task 31
(completed structurally, `ml/reports/analytic-pad-calibration.md`). No
real-attack APCER/BPCER measurement exists.

37-38 `namtor_pad` tests green.

## Learned PAD (MiniFASNet)

`ml/reports/learned-pad.md` and `docs/evidence/trust/learned-pad.md`:

- 49 KB ONNX model, ~80K params, trained on physics-based synthetic
  strobe-differential data encoding §6.4 signals.
- 100% validation accuracy on the synthetic distribution.
- Deployed to `var/lib/namtor/models/minifasnet_pad.onnx`.
- **Synthetic training data is explicitly non-production.** The model
  cannot be production-selected: task 32 requires consented
  subject/device-disjoint real data. The manifest rejects synthetic-only
  weights for production selection.
- Integrated as an additional veto alongside the analytic gates.

## 3D mask exclusion

3D masks are explicitly NOT evaluated. The reference hardware has no depth
sensor. `G4Report.evaluated_3d_mask` is `false` and the report states this.
A 3D mask that reproduces the strobe-differential signal would defeat
analytic PAD, and no claim is made against it. The depth path
(`docs/evidence/depth-status.md`) is structural only; `DEPTH_HIL_VALIDATED`
is `false`.

## Data collection infrastructure

`namtor_collect` ships consent-gated PAD data collection (§10.4):

- `ConsentRecord` (grant/revoke), per-session IDs for withdrawal.
- `CollectionStore` writes raw frame pairs + derived differential stats.
- 15 tests green.
- Production collection campaign (consented subjects, attack props) has not
  run. Owner-gated.

## What blocks the production run

| Blocker | Owner | Status |
|---|---|---|
| Consented human subjects + attack props | human | WITHHELD: no collection campaign |
| Real print/IR-print/replay/cutout corpus | human | WITHHELD: no attack corpus |
| Model licences accepted | human | WITHHELD |

## Summary

| Sub-gate | Status | Evidence |
|---|---|---|
| G4.1 APCER <= 5% per type at BPCER <= 5% | WITHHELD | Analytic gates uncalibrated; learned PAD synthetic-only. No real corpus. |
| G4.2 Per attack type, never averaged | implemented (harness) | `allow_averaged_attacks = false` enforced by protocol. No production report. |
| G4.3 3D mask documented as not evaluated | implemented | `G4Report.evaluated_3d_mask = false` by construction. |

G4 stays WITHHELD until a consented live-attack cohort with recorded
consent is collected and the analytic + learned PAD gates are calibrated
against real print, IR-print, replay, and cutout presentations.
