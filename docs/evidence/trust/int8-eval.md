# FP32 vs INT8 production inference — evidence

Spec ref: §5.3 (CPU INT8/AVX-VNNI auth path), Task 30.
Implementation: `crates/namtor-worker/src/infer_onnx.rs` (OnnxCpu backend),
`crates/namtor-worker/src/dispatch.rs` (CPU feature detection + quant dispatch),
`crates/namtor-worker/src/int8_eval.rs` (Rust micro-benchmark),
`ml/recipes/eval_int8_vs_fp32.py` (Python eval script).
Commit: `eval(worker): FP32 vs INT8 production inference`.

## Design

The auth path is CPU-only by design (spec §5.3, threat model). INT8 quantized
inference benefits from AVX-VNNI instructions on Intel i9-13900H. ONNX Runtime's
CPU Execution Provider auto-detects AVX-VNNI at runtime — there is no `ort` API
to force-enable it. INT8 quantized models (QDQ format, QInt8 weights, QUInt8
activations) pick up VNNI instructions automatically when the CPU EP handles
`QLinear*` / `DequantizeLinear` ops.

The GPU execution provider is NEVER selected — the `BackendKind` enum has no
GPU variant (defence-in-depth, spec §5.3).

### Dispatch rule (`dispatch.rs`)

| CPU features | Selected quant |
|---|---|
| AVX-VNNI or AVX-512 VNNI | `Quant::Int8` |
| otherwise (including aarch64 NEON) | `Quant::Fp32` |

### INT8 model loading (`infer_onnx.rs`)

`OnnxCpu::quant_file(Quant::Int8)` → `"int8.onnx"`. The daemon's
`ModelFetcher::ensure_int8` generates the INT8 variant from the FP32 source
via `ml/recipes/quantize_int8.py` at first-run (static QDQ quantization,
MinMax calibration).

## Hardware

| Feature | Present |
|---|---|
| Architecture | `x86_64` |
| AVX2 | yes |
| AVX-VNNI | yes |
| AVX-512F | no |
| AVX-512 VNNI | no |

Reference CPU: Intel i9-13900H (Alder Lake-P, 14 cores, AVX2 + AVX-VNNI).

## Evaluation: SFace 2021dec (production face embedding model)

**Model:** SFace 2021dec, 128-d L2-normalised embedding, input [1, 3, 112, 112].
**Quantization:** Static QDQ, MinMax calibration, QUInt8 activations, QInt8 weights.
**Inputs:** 200 synthetic random float32 [1, 3, 112, 112] (seed=42), 20 warmup runs.
**Threads:** 1 (intra-op).
**EP:** CPUExecutionProvider only.

### Model size

| Variant | Size (bytes) | Size (KB) |
|---|---|---|
| FP32 | 38,696,353 | 37,791.4 |
| INT8 | 9,903,159 | 9,671.1 |

**Size ratio (INT8/FP32):** 0.256 — INT8 is ~3.9× smaller.

### Latency

| Statistic | FP32 (ms) | INT8 (ms) | Delta (ms) |
|---|---|---|---|
| Median | 40.66 | 28.42 | −12.24 |
| p95 | 52.51 | 30.95 | −21.56 |
| Mean | 42.62 | 28.91 | −13.71 |

**Latency speedup (FP32 median / INT8 median):** 1.43× — INT8 is 30% faster.

Gate G2 (median ≤ 500 ms, p95 ≤ 1000 ms): **FP32 PASS**, **INT8 PASS**.

### Accuracy (INT8 vs FP32 output delta, synthetic inputs)

| Metric | Value |
|---|---|
| Cosine similarity (mean) | 0.903824 |
| Cosine similarity (min) | 0.876172 |
| Max absolute error (mean) | 0.270701 |
| Max absolute error (max) | 0.341985 |
| Samples | 50 |

**WITHHELD — real accuracy (FAR/FRR).** The cosine similarity on synthetic
random inputs is not a meaningful accuracy metric for face recognition. Real
accuracy measurement requires the R5 cohort dataset (owner-gated, not
available in this run). The `dispatch::build_report` logic retains FP32 when
INT8 FAR/FRR degradation exceeds thresholds (`DegradationThresholds::default()`:
max_far_delta = 5e-6, max_frr_delta = 5e-3). Until the R5 cohort measurement
is made, the INT8 optimization is WITHHELD per `QuantizationReport.withheld`.

## Evaluation: FC test fixture (synthetic, CI-verifiable)

**Model:** Synthetic FC (MatMul [8,4] + Add + ReLU), input [1, 8], output [1, 4].
**Fixtures:** `fixtures/onnx/fc_fp32.onnx` (321 bytes), `fixtures/onnx/fc_int8.onnx` (1611 bytes).
**Purpose:** Exercises the real Rust `OnnxCpu` INT8 code path in CI without
pulling Python or real model weights.

### Model size

| Variant | Size (bytes) |
|---|---|
| FP32 | 321 |
| INT8 | 1611 |

INT8 is LARGER for tiny models — QDQ quantization adds overhead nodes
(QuantizeLinear/DequantizeLinear) that exceed the savings from quantizing 32
float weights. This inverts for production-scale models (SFace: 3.9× smaller).

### Latency

| Statistic | FP32 (ms) | INT8 (ms) |
|---|---|---|
| Median | 0.005 | 0.008 |
| p95 | 0.006 | 0.009 |

INT8 is SLOWER for tiny models — the quantize/dequantize overhead dominates
when the compute is trivial. This inverts for production-scale models (SFace:
1.43× faster).

### Rust micro-benchmark (`int8_eval.rs`)

The `fc_fixture_eval_compares_fp32_and_int8` test loads both FC fixtures via
`OnnxCpu::load_file`, runs 20 inferences on each, and asserts:
- Cosine similarity > 0.8 (same model, quantized).
- Latency measurements are non-empty.
- Size ratio is positive.

This verifies the INT8 loading + inference code path that the auth path uses.

## Quantization recipe

`ml/recipes/quantize_int8.py` — static INT8 quantization using ONNX Runtime:

```python
quantize_static(
    fp32_path, int8_path,
    calibration_data_reader=reader,
    quant_format=QuantFormat.QDQ,
    activation_type=QuantType.QUInt8,
    weight_type=QuantType.QInt8,
    calibrate_method=CalibrationMethod.MinMax,
)
```

`ml/recipes/eval_int8_vs_fp32.py` — the eval script that produced the SFace
numbers above. Reproducible with:

```bash
python3 ml/recipes/eval_int8_vs_fp32.py \
    --fp32 var/lib/namtor/models/sface_2021dec.onnx \
    --out-dir /tmp/int8-eval-sface \
    --n-synth 200 --n-warmup 20 --threads 1
```

## Test coverage

### `int8_eval.rs` (6 tests)

| test | proves |
|---|---|
| `fc_fixture_eval_compares_fp32_and_int8` | OnnxCpu loads + runs both FP32 and INT8 fixtures; cosine sim > 0.8 |
| `cosine_similarity_identical_vectors` | cosine sim of identical vectors = 1.0 |
| `cosine_similarity_orthogonal_vectors` | cosine sim of orthogonal vectors = 0.0 |
| `max_abs_error_basic` | max abs error between known vectors |
| `median_and_p95_of_sorted_samples` | percentile computation correctness |
| `median_of_empty_is_zero` | empty latency list → zero (no panic) |

### `dispatch.rs` (12 tests, pre-existing)

| test | proves |
|---|---|
| `select_quant_picks_int8_when_avx_vnni_present` | AVX-VNNI → Int8 |
| `select_quant_picks_int8_when_avx512_vnni_present` | AVX-512 VNNI → Int8 |
| `select_quant_picks_fp32_when_no_vnni` | no VNNI → Fp32 |
| `select_quant_never_returns_gpu_path` | sweep all features → always Fp32 or Int8 |

### `backend_eval.rs` (12 tests, pre-existing)

The ablation harness gates INT8 promotion on: no GPU, arch support, licence
accepted, sandbox parity, predeclared accuracy (FAR ≤ 1e-5, FRR ≤ 3%),
latency gain, and statistical validity (≥ 1000 comparisons).

## Verification commands

```
cargo test -p namtor-worker --lib          # 66 passed
cargo clippy -p namtor-worker -- -D warnings  # clean
cargo fmt -p namtor-worker --check            # clean
python3 ml/recipes/eval_int8_vs_fp32.py --fp32 <model> --out-dir <dir>
```

## Residual risk / WITHHELD

- **Real FAR/FRR accuracy:** WITHHELD — requires R5 cohort dataset
  (owner-gated). The `dispatch::build_report` logic retains FP32 when
  degradation exceeds thresholds. Until measured, INT8 optimization is
  WITHHELD per `QuantizationReport.withheld = true` (Path B, spec §5.3).
- **Calibration data:** The INT8 quantization used synthetic random
  calibration data. Production calibration should use pre-extracted face
  crops from the R5 cohort (owner-gated dataset decision).
- **SFace model graph:** SFace has initializers in graph inputs (ORT warning),
  which prevents some graph optimizations. This is an upstream model export
  issue, not a Nam-tor issue.
- **Tiny-model inversion:** INT8 is slower and larger for tiny models (FC
  fixture). This is expected and inverts for production-scale models (SFace:
  1.43× faster, 3.9× smaller). The auth path only uses production-scale
  models (SFace 37 MB, YuNet 232 KB).
