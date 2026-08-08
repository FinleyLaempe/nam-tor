#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""FP32 vs INT8 production inference evaluation (Task 30, spec §5.3).

Quantizes an FP32 ONNX model to INT8 (static QDQ, MinMax calibration), then
runs both through the ONNX Runtime CPU Execution Provider and measures:

  - **Accuracy**: cosine similarity + max absolute error of the output
    embeddings over N synthetic inputs.
  - **Latency**: median and p95 inference time (ms), after warmup.
  - **Model size**: FP32 vs INT8 file size + compression ratio.

The CPU EP auto-detects AVX-VNNI on the reference Intel i9-13900H; INT8
quantized ops (QLinear*/DequantizeLinear) pick up VNNI instructions
automatically. No GPU execution provider is ever selected (spec §5.3).

Usage:
    python3 eval_int8_vs_fp32.py --fp32 <model_fp32.onnx> \
        --out-dir <dir> [--n-synth 200] [--n-warmup 20] [--threads 1]

Outputs:
    <dir>/int8_eval_report.json   — machine-readable report
    <dir>/int8_eval_report.md     — human-readable markdown table

If --int8 is provided, skips quantization and uses the existing INT8 model.
No model weights or biometric data are committed; only measured metrics.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import statistics
import sys
import tempfile
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort


def _percentile(values: list[float], pct: float) -> float:
    """Linear-interpolation percentile (numpy-compatible)."""
    if not values:
        return 0.0
    s = sorted(values)
    k = (len(s) - 1) * pct / 100.0
    lo = int(k)
    hi = min(lo + 1, len(s) - 1)
    frac = k - lo
    return s[lo] * (1 - frac) + s[hi] * frac


def _cosine_sim(a: np.ndarray, b: np.ndarray) -> float:
    fa = a.flatten().astype(np.float64)
    fb = b.flatten().astype(np.float64)
    na = np.linalg.norm(fa)
    nb = np.linalg.norm(fb)
    if na < 1e-12 or nb < 1e-12:
        return 0.0
    return float(np.dot(fa, fb) / (na * nb))


def _input_shape(session: ort.InferenceSession) -> tuple[str, list[int]]:
    inp = session.get_inputs()[0]
    shape = []
    for d in inp.shape:
        if isinstance(d, int) and d > 0:
            shape.append(d)
        else:
            shape.append(1)  # dynamic dim → 1 for eval
    return inp.name, shape


def _generate_inputs(shape: list[int], n: int) -> list[np.ndarray]:
    """Generate n synthetic float32 inputs of the given shape."""
    np.random.seed(42)
    return [np.random.randn(*shape).astype(np.float32) for _ in range(n)]


def _quantize(fp32_path: Path, int8_path: Path, input_name: str,
              shape: list[int]) -> None:
    """Static INT8 quantization via onnxruntime.quantization."""
    from onnxruntime.quantization import (
        CalibrationDataReader,
        CalibrationMethod,
        QuantFormat,
        QuantType,
    )
    from onnxruntime.quantization.quantize import quantize_static

    class _Reader(CalibrationDataReader):
        def __init__(self, input_name: str, shape: list[int], n: int = 8):
            np.random.seed(99)
            self._data = [
                {input_name: np.random.randn(*shape).astype(np.float32)}
                for _ in range(n)
            ]
            self._i = 0

        def get_next(self):
            if self._i >= len(self._data):
                return None
            d = self._data[self._i]
            self._i += 1
            return d

    quantize_static(
        str(fp32_path),
        str(int8_path),
        calibration_data_reader=_Reader(input_name, shape),
        quant_format=QuantFormat.QDQ,
        activation_type=QuantType.QUInt8,
        weight_type=QuantType.QInt8,
        calibrate_method=CalibrationMethod.MinMax,
    )


def _measure(session: ort.InferenceSession, inputs: list[np.ndarray],
             input_name: str, n_warmup: int) -> list[float]:
    """Run inference n_warmup + len(inputs) times; return latencies in ms."""
    # Warmup
    for inp in inputs[:n_warmup]:
        session.run(None, {input_name: inp})

    latencies: list[float] = []
    for inp in inputs:
        t0 = time.perf_counter()
        session.run(None, {input_name: inp})
        t1 = time.perf_counter()
        elapsed_ms = (t1 - t0) * 1000.0
        latencies.append(elapsed_ms)
    return latencies


def run_eval(
    fp32_path: Path,
    int8_path: Path | None,
    out_dir: Path,
    n_synth: int = 200,
    n_warmup: int = 20,
    threads: int = 1,
) -> dict:
    """Run the FP32 vs INT8 evaluation and return a report dict."""
    so = ort.SessionOptions()
    so.intra_op_num_threads = threads
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL

    fp32_sess = ort.InferenceSession(
        str(fp32_path), so, providers=["CPUExecutionProvider"]
    )
    input_name, shape = _input_shape(fp32_sess)

    # Quantize if no INT8 path provided.
    if int8_path is None:
        int8_path = out_dir / "int8_quantized.onnx"
        _quantize(fp32_path, int8_path, input_name, shape)

    int8_sess = ort.InferenceSession(
        str(int8_path), so, providers=["CPUExecutionProvider"]
    )

    inputs = _generate_inputs(shape, n_synth)

    # Accuracy: compare outputs
    fp32_outputs = []
    int8_outputs = []
    for inp in inputs[: min(n_synth, 50)]:  # 50 samples for accuracy
        fp32_outputs.append(fp32_sess.run(None, {input_name: inp})[0])
        int8_outputs.append(int8_sess.run(None, {input_name: inp})[0])

    cosines = [_cosine_sim(f, i) for f, i in zip(fp32_outputs, int8_outputs)]
    max_abs_errors = [
        float(np.max(np.abs(f - i)))
        for f, i in zip(fp32_outputs, int8_outputs)
    ]

    # Latency
    fp32_lat = _measure(fp32_sess, inputs, input_name, n_warmup)
    int8_lat = _measure(int8_sess, inputs, input_name, n_warmup)

    # Model sizes
    fp32_size = fp32_path.stat().st_size
    int8_size = int8_path.stat().st_size

    # CPU features
    import platform
    cpu_features = _detect_cpu_features()

    report = {
        "model": fp32_path.name,
        "input_shape": shape,
        "n_synth": n_synth,
        "n_warmup": n_warmup,
        "threads": threads,
        "cpu_features": cpu_features,
        "providers": ort.get_available_providers(),
        "fp32": {
            "size_bytes": fp32_size,
            "latency_median_ms": statistics.median(fp32_lat),
            "latency_p95_ms": _percentile(fp32_lat, 95),
            "latency_mean_ms": statistics.mean(fp32_lat),
        },
        "int8": {
            "size_bytes": int8_size,
            "latency_median_ms": statistics.median(int8_lat),
            "latency_p95_ms": _percentile(int8_lat, 95),
            "latency_mean_ms": statistics.mean(int8_lat),
        },
        "accuracy": {
            "cosine_similarity_mean": statistics.mean(cosines),
            "cosine_similarity_min": min(cosines),
            "max_abs_error_mean": statistics.mean(max_abs_errors),
            "max_abs_error_max": max(max_abs_errors),
            "n_samples": len(cosines),
        },
        "size_ratio": int8_size / fp32_size if fp32_size > 0 else 0.0,
        "latency_speedup": (
            statistics.median(fp32_lat) / statistics.median(int8_lat)
            if statistics.median(int8_lat) > 0
            else 0.0
        ),
    }
    return report


def _detect_cpu_features() -> dict:
    """Detect x86 CPU features relevant to INT8 dispatch."""
    features = {"arch": platform.machine()}
    if platform.machine() != "x86_64":
        return features
    try:
        with open("/proc/cpuinfo") as f:
            flags = ""
            for line in f:
                if line.startswith("flags"):
                    flags = line
                    break
        feature_map = {
            "avx2": "avx2" in flags,
            "avx_vnni": "avx_vnni" in flags,
            "avx512f": "avx512f" in flags,
            "avx512vnni": "avx512vnni" in flags,
        }
        features.update(feature_map)
    except OSError:
        pass
    return features


def render_markdown(report: dict) -> str:
    """Render the report as a markdown table."""
    r = report
    lines = [
        "# INT8 vs FP32 Production Inference Evaluation",
        "",
        f"**Model:** `{r['model']}`",
        f"**Input shape:** {r['input_shape']}",
        f"**Synthetic inputs:** {r['n_synth']}",
        f"**Warmup runs:** {r['n_warmup']}",
        f"**Threads:** {r['threads']}",
        f"**CPU EP providers:** {r['providers']}",
        "",
        "## CPU features",
        "",
        f"| Feature | Present |",
        f"|---|---|",
    ]
    for k, v in r["cpu_features"].items():
        if k == "arch":
            lines.append(f"| arch | `{v}` |")
        else:
            lines.append(f"| {k} | {'yes' if v else 'no'} |")

    lines += [
        "",
        "## Model size",
        "",
        "| Variant | Size (bytes) | Size (KB) |",
        "|---|---|---|",
        f"| FP32 | {r['fp32']['size_bytes']} | {r['fp32']['size_bytes'] / 1024:.1f} |",
        f"| INT8 | {r['int8']['size_bytes']} | {r['int8']['size_bytes'] / 1024:.1f} |",
        "",
        f"**Size ratio (INT8/FP32):** {r['size_ratio']:.3f}",
        "",
        "## Latency",
        "",
        "| Statistic | FP32 (ms) | INT8 (ms) | Delta (ms) |",
        "|---|---|---|---|",
        f"| Median | {r['fp32']['latency_median_ms']:.3f} | {r['int8']['latency_median_ms']:.3f} | {r['int8']['latency_median_ms'] - r['fp32']['latency_median_ms']:+.3f} |",
        f"| p95 | {r['fp32']['latency_p95_ms']:.3f} | {r['int8']['latency_p95_ms']:.3f} | {r['int8']['latency_p95_ms'] - r['fp32']['latency_p95_ms']:+.3f} |",
        f"| Mean | {r['fp32']['latency_mean_ms']:.3f} | {r['int8']['latency_mean_ms']:.3f} | {r['int8']['latency_mean_ms'] - r['fp32']['latency_mean_ms']:+.3f} |",
        "",
        f"**Latency speedup (FP32 median / INT8 median):** {r['latency_speedup']:.2f}×",
        "",
        "## Accuracy (INT8 vs FP32 output delta)",
        "",
        f"**Samples:** {r['accuracy']['n_samples']}",
        "",
        "| Metric | Value |",
        "|---|---|",
        f"| Cosine similarity (mean) | {r['accuracy']['cosine_similarity_mean']:.6f} |",
        f"| Cosine similarity (min) | {r['accuracy']['cosine_similarity_min']:.6f} |",
        f"| Max absolute error (mean) | {r['accuracy']['max_abs_error_mean']:.6f} |",
        f"| Max absolute error (max) | {r['accuracy']['max_abs_error_max']:.6f} |",
        "",
        "## Notes",
        "",
        "- INT8 quantization: static QDQ format, MinMax calibration, QUInt8 activations, QInt8 weights.",
        "- CPU EP auto-detects AVX-VNNI; no GPU execution provider is ever selected (spec §5.3).",
        "- Synthetic random inputs (seed=42); real-model accuracy on face data requires",
        "  the R5 cohort dataset (owner-gated, not available in this run).",
        "",
    ]
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="FP32 vs INT8 production inference evaluation"
    )
    parser.add_argument("--fp32", required=True, type=Path, help="FP32 ONNX model")
    parser.add_argument("--int8", type=Path, help="Existing INT8 model (skip quantization)")
    parser.add_argument("--out-dir", required=True, type=Path, help="Output directory")
    parser.add_argument("--n-synth", type=int, default=200, help="Number of synthetic inputs")
    parser.add_argument("--n-warmup", type=int, default=20, help="Warmup runs")
    parser.add_argument("--threads", type=int, default=1, help="Intra-op threads")
    args = parser.parse_args()

    if not args.fp32.exists():
        print(f"FP32 model not found: {args.fp32}", file=sys.stderr)
        return 1

    args.out_dir.mkdir(parents=True, exist_ok=True)

    report = run_eval(
        args.fp32,
        args.int8,
        args.out_dir,
        n_synth=args.n_synth,
        n_warmup=args.n_warmup,
        threads=args.threads,
    )

    json_path = args.out_dir / "int8_eval_report.json"
    md_path = args.out_dir / "int8_eval_report.md"

    json_path.write_text(json.dumps(report, indent=2), encoding="utf-8")
    md_path.write_text(render_markdown(report), encoding="utf-8")

    print(f"wrote {json_path}")
    print(f"wrote {md_path}")
    print(f"\nSummary:")
    print(f"  FP32 median: {report['fp32']['latency_median_ms']:.3f} ms")
    print(f"  INT8 median: {report['int8']['latency_median_ms']:.3f} ms")
    print(f"  Speedup: {report['latency_speedup']:.2f}×")
    print(f"  Cosine sim: {report['accuracy']['cosine_similarity_mean']:.6f}")
    print(f"  Size ratio: {report['size_ratio']:.3f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
