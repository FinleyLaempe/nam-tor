# M3 R4 — INT8 vs FP32 embedding degradation

**Status: PENDING — blocked on human action.**

This report is a placeholder. It is filled by running:

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

**Cohort: (not yet decided — blocked on human)**

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

Gate G3 (FAR ≤ 0.001% at FRR ≤ 3.000%): **PENDING**
Gate G2 (median ≤ 500 ms, p95 ≤ 1000 ms): **PENDING**
