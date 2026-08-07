# RC production gate certification: G2-G5 + desktop acceptance

**Status: structural gates PASSED, measured gates WITHHELD, no failed gates.**
**Date:** 2026-08-07
**Task:** 34 — Execute final G2-G5 and desktop acceptance suite
**Commit message:** `test(release): certify production gates`

This is the release-candidate gate ledger. Every gate below is either
PASSED (asserted by a CI test on `master`), WITHHELD (needs human-in-the-loop
measurement, a cohort dataset, or RC packages on reference hardware), or
PARTIAL (real-TPM single-device only). No gate is failed. No claim is
published without evidence. The companion machine-readable file is
`docs/evidence/rc-g2-g5.json`.

Reference: `docs/superpowers/specs/2026-08-04-nam-tor-design.md` §12. The
gate definitions and the rule-of-three FAR bound live in
`ml/reports/production-eval.md`. The TPM evidence lives in
`docs/evidence/g5-tpm.md`. The G5 fault matrix is
`docs/evidence/g5-fault-matrix.json`.

## G2 — Latency

**Target:** median ≤ 500 ms, p95 ≤ 1000 ms.
**Status: WITHHELD.**

The eval harness is ready. `crates/namtor-eval/src/protocol.rs` defines the
`G2Report` carrying `median_ms`, `p95_ms`, and a `first_boot` /
`steady_state` `LatencyBand` breakdown, so cold model load cannot hide
inside an overall median. `crates/namtor-eval/src/score.rs` records a
`LatencyBreakdown` per attempt; the G2 report aggregates those records.

No production run has been executed. Measuring G2 needs RC packages installed
on the reference hardware (ASUS ROG Flow X16 GV601VI, Sonix `3277:0018`,
IR `/dev/video2`). The harness records `EvalConfig.hardware_hash` and
`EvalConfig.commit_hash` at run time, so a number published here is bound to
a specific build on a specific device.

## G3 — Biometric accuracy

**Target:** FAR ≤ 1/100,000 at FRR ≤ 3%.
**Status: WITHHELD.**

The subject-disjoint protocol is validated. `validate_protocol` enforces
that no subject appears in both train and test slices, that
`min_impostor_comparisons >= 100_000`, that
`min_genuine_per_condition >= 1_000`, and that
`allow_averaged_attacks = false`.

The FAR ≤ 1e-5 claim has a stricter floor than the reportability floor. At
zero observed false accepts the 95% upper bound under the rule of three is
`3 / n`. The claim is WITHHELD until `n >= 300_000` impostor comparisons
*and* zero false accepts, which makes the upper bound ≤ 1e-5. A 100k run
with zero false accepts publishes a point estimate of 0 with a 3e-5 upper
bound and `far_claim_eligible = false`. The 100k floor is the minimum for
G3 to be reportable at all, not the floor for the claim.

No lawful cohort dataset is available. R5 z-norm cohort selection is
owner-gated (`docs/evidence/r5-cohort.md`). The Python `onnxruntime` binding
needed for the INT8 vs FP32 evaluation is also owner-gated. Both block any
production run.

## G4 — Presentation attack detection

**Target:** APCER ≤ 5% per attack type, BPCER ≤ 5%.
**Status: WITHHELD.**

Analytic PAD gates are calibrated on synthetic data only
(`ml/reports/analytic-pad-calibration.md`). The four gates on the
strobe-differential signal `D = On - Off` (differential energy, gradient
consistency, specular highlight, radial falloff) are a veto, not a weighted
term, and they are not calibrated against live-attack presentations.

Learned MiniFASNet PAD is synthetic-only
(`ml/reports/learned-pad.md`). The 49 KB ONNX model was trained on
physics-based synthetic strobe-differential data and reaches 100% validation
accuracy on that synthetic distribution. No claim is made about its
performance on real attacks.

3D masks are explicitly NOT evaluated. The reference hardware has no depth
sensor. `G4Report.evaluated_3d_mask` is `false` and the report states this.
A 3D mask that reproduces the strobe-differential signal would defeat
analytic PAD, and no claim is made against it.

A live-attack cohort with recorded consent is required before any G4 number
is published.

## G5.1 — PAM no-ML structural gate

**Target:** `pam_namtor.so` contains no ML code and is < 500 KB.
**Status: PASSED** (CI gate).

`namtor-pam` is 457 KB. It has no ONNX, no image decoder, no embedding code.
The PAM module drives the socket client through Prepare, Verify, Release and
maps every error path to `PAM_AUTHINFO_UNAVAIL`. The structural test runs in
CI on `master`.

## G5.2 — Sandbox escape blocked

**Target:** worker cannot open files, open sockets, or make unauthorised
syscalls.
**Status: PASSED** (CI gate).

`namtor_worker::sandbox::apply_sandbox` applies seccomp and landlock. The
fault matrix (`docs/evidence/g5-fault-matrix.json`) records
`landlock_denies_open`, `seccomp_denies_socket`, and the
`MAX_PAYLOAD` boundary test as `existing` / `pass`. The worker runs as
`nobody` with no filesystem and no network. The sandbox never widens
silently.

## G5.3 — Fail-closed

**Target:** every error path returns `PAM_AUTHINFO_UNAVAIL`.
**Status: PASSED** (CI gate).

The PAM module maps capture failure, daemon unreachable, timeout, PAD veto,
no match, and rate-limit to `PAM_AUTHINFO_UNAVAIL` or `PAM_AUTH_ERR` per the
spec, never to `PAM_SUCCESS` on an error. The fail-closed contract is
asserted by tests on `master`.

## G5.4 — No biometric logs

**Target:** no biometric data in logs at any level.
**Status: PASSED** (CI gate).

The `e2e_no_biometric_data_in_logs` test runs a full verify cycle with
capture frames and asserts no embedding, template, pixel, or `vec=` token
appears in log output. The fault matrix records this as `existing`.

## G5.5 — TPM templates unreadable off-device

**Target:** a sealed key blob fails to unseal on a different TPM.
**Status: PARTIAL.**

MockTpm contract tests pass in CI (`docs/evidence/g5-tpm.md`). Two different
seeds (simulating two machines) produce mutually-unsealable blobs
(`mock_reject_moved_blob`,
`sealed_key_rejects_moved_blob_across_machines`). Corruption and lockout
return distinct errors. The real-TPM single-device test
(`real_tpm_seal_unseal_round_trip`) is `#[ignore]` and passes when run by a
`tss`-group user.

Cross-hardware is WITHHELD. Only one TPM-equipped machine is available.
The G5.5 cross-hardware claim is not made from one machine, per the task
brief: evidence is limited to MockTpm plus same-device unseal.

## Desktop acceptance

GNOME profile management is implemented
(`docs/evidence/gnome-qa/report.md`). The GTK4 / libadwaita enrollment
wizard and settings app show enrolled users and handle the missing-store
case.

KDE profile management is implemented
(`docs/evidence/kde-qa/report.md`). The KCM is wired to the same backend.

Both UIs talk to the daemon through the D-Bus API
(`docs/evidence/dbus-contract.xml`). No UI opens the template store
directly.

End-to-end profile create, review, merge, and delete without direct store
access is implemented but not HIL-verified. The S3 20x HIL gate is WITHHELD
per `docs/evidence/status-audit.md` R10.

## Model and package hashes

Model hashes (YuNet 2023mar, SFace 2021dec, MiniFASNet PAD) and package
hashes are recorded at run time in `EvalConfig.model_hashes` and the install
manifest. They will be bound to the RC results at tag time, not before. No
hash is claimed in this document.

## Summary

Every G2-G5 gate is in one of three states:

- **PASSED** (G5.1, G5.2, G5.3, G5.4): structural CI gates, asserted by
  tests on `master`.
- **WITHHELD** (G2, G3, G4): need HIL measurement, a lawful cohort dataset,
  or RC packages on reference hardware. The harness and protocol are ready;
  the inputs are owner-gated.
- **PARTIAL** (G5.5): MockTpm and same-device real-TPM pass; cross-hardware
  is WITHHELD on one TPM machine.

No gate is failed. No claim is published without evidence. Desktop
acceptance is implemented and unit-tested but not HIL-verified. The
authoritative per-gate ledger remains `docs/evidence/status-audit.md`; this
document is the RC snapshot.
