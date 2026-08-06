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
