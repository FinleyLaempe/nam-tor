# Nam-tor ML Recipes

Scripts that run on the user's machine at first-run or during development.
No model weights or datasets are committed to this repository.

## `quantize_int8.py`

Static INT8 quantization of stock ONNX models using ONNX Runtime. Produces
an INT8-quantized model from an FP32 source for the CPU EP (AVX-VNNI).

```bash
python3 quantize_int8.py --fp32 <model_fp32.onnx> --out <model_int8.onnx> [--calib <dir>]
```

Requires: `onnx`, `onnxruntime`, `numpy` (Python packages).

## `r4_int8_vs_fp32.py`

R4 evaluation: measures INT8 vs FP32 embedding degradation (FAR/FRR at the
operating threshold + per-stage latency) and renders `docs/m3-r4-report.md`.

Consumes two JSONL score files produced by the Rust driver
(`cargo test -p namtor-daemon --test r4 -- --ignored`):

```bash
python3 r4_int8_vs_fp32.py --fp32 r4_fp32.jsonl --int8 r4_int8.jsonl \
    --out docs/m3-r4-report.md --cohort <name>
```

Requires: `namtor-eval` (`pip install -e ml/eval`), `numpy`.

Blocked on: Python `onnxruntime` binding fix + model selection + licence
acceptance (`NAMTOR_ACCEPT_LICENCE=1`) + R5 cohort dataset decision. See
`AGENTS.md` "What M3 needs from the human".
