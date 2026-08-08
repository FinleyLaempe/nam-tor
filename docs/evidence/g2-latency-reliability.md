# G2 — Latency and Reliability Gate Suite

SPDX-License-Identifier: GPL-3.0-only

Spec reference: §12 (G2 acceptance gates), §5.3 (CPU-only auth path).
Task: 42 — Run G2 production latency and reliability evaluation.
Date: 2026-08-08.

## Gate targets

| Sub-gate | Target | Source |
|---|---|---|
| G2.1 | Median latency <= 500 ms | spec §12 |
| G2.2 | p95 latency <= 1000 ms | spec §12 |
| G2.3 | First boot <= 120% steady-state | spec §12 |

## Status: WITHHELD (production path)

No production-path latency distribution has been published. The eval harness
is ready and the INT8 inference micro-benchmark (Task 30) gives strong
evidence that the inference component alone is well within budget, but the
full auth path (camera open, capture, PAD, recognition, fusion, IPC) has
not been measured end-to-end on the reference hardware.

## What is measured: INT8 inference micro-benchmark

The SFace production embedding model was benchmarked on the reference CPU
(Intel i9-13900H, AVX2 + AVX-VNNI) using 200 synthetic random inputs with
20 warmup runs, 1 intra-op thread, CPUExecutionProvider only. Full details
in `docs/evidence/trust/int8-eval.md`.

| Statistic | FP32 (ms) | INT8 (ms) | Delta (ms) |
|---|---|---|---|
| Median | 40.66 | 28.42 | -12.24 |
| p95 | 52.51 | 30.95 | -21.56 |
| Mean | 42.62 | 28.91 | -13.71 |

INT8 is 1.43x faster than FP32 on the median and well within the G2 budget
for the inference component alone. Model size: FP32 37,791 KB, INT8 9,671 KB
(3.9x smaller). Both FP32 and INT8 pass the G2 latency target at the
inference layer.

**Caveat:** these are synthetic-input micro-benchmarks of the embedding
model only. They do not include camera open (~677ms upper bound per R11),
face detection (YuNet), alignment, PAD, fusion, or IPC overhead. The full
auth path has not been measured.

## What is ready

### Eval harness

`crates/namtor-eval/src/protocol.rs` defines `G2Report` carrying
`median_ms`, `p95_ms`, and a `first_boot` / `steady_state` `LatencyBand`
breakdown, so cold model load cannot hide inside an overall median.
`crates/namtor-eval/src/score.rs` records a `LatencyBreakdown` per attempt;
the G2 report aggregates those records. The harness records
`EvalConfig.hardware_hash` and `EvalConfig.commit_hash` at run time, so a
number published here is bound to a specific build on a specific device.

### Warm/cold lifecycle

`namtor_daemon::lifecycle::Lifecycle` implements the warm/cold state
machine: `Cold` -> `Warming` -> `Verifying` with configurable warm timeout.
`prepare_for_auth` starts the warmup (model load + camera open), `verify_start`
begins inference, `release` returns to `Cold` or `Warming`. First-boot
latency (cold model load) is separated from steady-state by the
`LatencyBand` breakdown in the eval report.

### Camera-open cost (R11)

`measure_open_cost` subprocess upper bound: ~677ms. Re-measured in-process
in M2. This is the dominant cold-start cost. The warm lifecycle amortises
it: `prepare_for_auth` opens the camera during the warming phase, so the
verify call does not pay it again. Whether the warm path stays under 500ms
median has not been measured.

## What blocks the production run

| Blocker | Owner | Status |
|---|---|---|
| RC packages installed on reference hardware | human (root) | WITHHELD: needs root install |
| Python onnxruntime binding (system Python 3.14 incompat) | human | WITHHELD: binding fails to import |
| Model licences accepted (`NAMTOR_ACCEPT_LICENCE=1`) | human | WITHHELD: licences not accepted |
| Production capture path (auto-selection, not hardcoded `/dev/video2`) | code | implemented but not HIL-verified |

## Summary

| Sub-gate | Status | Evidence |
|---|---|---|
| G2.1 Median <= 500ms | WITHHELD | INT8 inference median 28.42ms (component); full path unmeasured |
| G2.2 p95 <= 1000ms | WITHHELD | INT8 inference p95 30.95ms (component); full path unmeasured |
| G2.3 First boot <= 120% steady | WITHHELD | Lifecycle state machine shipped; first-boot latency unmeasured |

The inference component (SFace INT8) is comfortably within budget. The
camera-open cost (~677ms) is the dominant risk for the cold-start path; the
warm lifecycle is designed to amortise it, but the full path has not been
measured end-to-end. G2 stays WITHHELD until the production auth path runs
on the reference hardware with RC packages.
