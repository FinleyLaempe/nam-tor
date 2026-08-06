# M3 R4 — INT8 vs FP32 embedding degradation

**Status: WITHHELD — blocked on owner-gated inputs.**

This report is a placeholder. The measurement is not made until the blocking
inputs are supplied; per the project's evidence rule, the R4 claim is
`WITHHELD` rather than `PENDING` so no reader mistakes the absence of a number
for a pending-but-imminent result. See `docs/evidence/status-audit.md` §3 (R4)
and §4 (owner-gated inputs).

Path B holds: spec §5.3 commits to CPU INT8 (AVX-VNNI) as the auth-path
quantisation on architectural grounds, so the INT8 path is the design default
regardless of the measurement. R4 measures the *degradation* vs FP32 and
validates the G2/G3 gates; it does not decide the architecture.

## Task 19 — CPU dispatch logic (shipped)

`crates/namtor-worker/src/dispatch.rs` ships the runtime CPU feature
detection and INT8/FP32 dispatch the auth path uses. The dispatch is
CPU-only by construction; the GPU execution provider is never probed and
never selected (spec §5.3, threat model).

### Dispatch rule

`select_quant(features: &CpuFeatures) -> Quant`:

| Detected feature                                | Selected | Rationale                                  |
|-------------------------------------------------|----------|--------------------------------------------|
| AVX-VNNI (`avxvnni`)                            | `Int8`   | VNNI accelerates INT8 quantized matmul.    |
| AVX-512 VNNI (`avx512vnni`)                     | `Int8`   | Same VNNI contract on AVX-512.             |
| AVX2 only (no VNNI)                             | `Fp32`   | FP32 is faster without VNNI.               |
| AArch64 NEON                                    | `Fp32`   | NEON does not accelerate INT8 quant.       |
| Unsupported arch (no VNNI, no NEON)             | `Fp32`   | Safe default.                              |
| Any combination including a GPU flag            | n/a      | No GPU flag exists; GPU path is unreachable. |

`CpuFeatures::detect()` runs `is_x86_feature_detected!` on x86_64 and
`is_aarch64_feature_detected!` on aarch64 at runtime; the per-arch probe is
behind a `cfg(target_arch = ...)` gate, so an unsupported instruction path
is impossible at both compile time and runtime.

### WITHHELD semantics

`QuantizationReport` is `withheld = true` in two cases:

1. **No lawful comparison cohort** (R5 WITHHELD — owner-gated dataset
   selection). The architectural default from `select_quant` is preserved
   (spec §5.3 Path B); the report's `selected` field reflects that default.
2. **INT8 accuracy degradation exceeds thresholds.** `DegradationThresholds`
   default to `max_far_delta = 5e-6` and `max_frr_delta = 5e-3` (half the
   spec §5.3 gate headroom). When `|INT8 − FP32|` exceeds either, the
   report's `selected` is forced to `Quant::Fp32` and `withheld_reason`
   records `"INT8 accuracy degradation exceeds thresholds; FP32 retained"`.
   A degraded model is never silently promoted.

The `selected` field is the only output the auth path consumes; the report
itself is evidence, not a runtime input.

### Why R4 is still WITHHELD

The dispatch logic is shipped and tested (12 unit tests in `dispatch.rs`),
but the actual FP32-vs-INT8 measurement still requires:

1. A working Python `onnxruntime` binding (currently broken on Python 3.14;
   see AGENTS.md).
2. Accepted upstream model licences (`NAMTOR_ACCEPT_LICENCE=1`).
3. An FP32 + INT8 ONNX model pair staged under `var/lib/namtor/r4-embed/`.
4. A licence-verified comparison cohort (R5 dataset decision).

All four are owner-gated. Until they are supplied, the `QuantizationReport`
produced at runtime carries `withheld = true` with reason
`"no comparison data available (R5 cohort WITHHELD)"`, and the auth path
runs on the architectural default — `Quant::Int8` on the reference i9-13900H
(which has AVX-VNNI), `Quant::Fp32` on hosts without VNNI.

It is filled by running:

```bash
# 1. Fix Python onnxruntime binding (see AGENTS.md)
# 2. Accept model licences: NAMTOR_ACCEPT_LICENCE=1
# 3. Place FP32 + INT8 ONNX models in var/lib/namtor/r4-embed/
# 4. Place labelled comparison set in fixtures/r4/{probes,gallery}/
# 5. Run the Rust driver (produces r4_fp32.jsonl + r4_int8.jsonl):
cargo test -p namtor-daemon --test r4 -- --ignored
# 6. Render this report from the JSONL:
python3 ml/recipes/r4_int8_vs_fp32.py \
    --fp32 r4_fp32.jsonl --int8 r4_int8.jsonl \
    --out docs/m3-r4-report.md --cohort <name>
```

## What R4 resolves

Spec §5.3 commits to CPU INT8 (AVX-VNNI) as the auth-path quantisation.
R4 measures whether INT8 meets the G3 accuracy gate (FAR ≤ 1/100,000 at
FRR ≤ 3%) and the G2 latency gate (median ≤ 500 ms, p95 ≤ 1000 ms), and
records the measured degradation vs FP32 on the same comparison set.

## What R5 resolves

R5 records the background cohort dataset used for score normalisation.
The cohort must be licence-verified per `ml/DATASETS.md` before use. No
cohort-derived weights or biometric data are committed to the repository.

### R5 cohort decision

**Cohort: WITHHELD — blocked on owner-gated dataset selection**

The decision is recorded here once the human verifies a dataset's
licence and selects it as the normalisation cohort. Candidates from
`ml/DATASETS.md`:

- CASIA NIR-VIS 2.0 (research licence, signed agreement)
- LAMP-HQ (research licence)
- BUAA-VisNir (research licence)
- Oulu-CASIA NIR-VIS (research licence)

## Placeholder delta table

| Metric | FP32 | INT8 | Delta |
|---|---|---|---|
| FAR | — | — | — |
| FRR | — | — | — |
| Median latency | — | — | — |
| p95 latency | — | — | — |

Gate G3 (FAR ≤ 0.001% at FRR ≤ 3.000%): **WITHHELD — no lawful cohort, no ≥100k impostor comparison run**
Gate G2 (median ≤ 500 ms, p95 ≤ 1000 ms): **WITHHELD — no production-path latency distribution published**
