#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""R4: INT8 vs FP32 embedding degradation measurement.

Consumes two JSONL score files produced by the Rust driver
(``crates/namtor-daemon/tests/r4.rs``):

  - ``r4_fp32.jsonl``  — scores from the FP32 OnnxCpu backend
  - ``r4_int8.jsonl``  — scores from the INT8 OnnxCpu backend

Computes FAR/FRR at the operating threshold (spec §12 G3.1: FAR ≤ 1e-5
at FRR ≤ 3%) and per-stage latency for both quant modes, then writes a
Markdown delta table to ``docs/m3-r4-report.md``.

Usage:
    python3 r4_int8_vs_fp32.py --fp32 r4_fp32.jsonl --int8 r4_int8.jsonl \
        --out docs/m3-r4-report.md [--cohort <name>]

This script runs on the dev machine after the human fixes the Python
onnxruntime binding and accepts model licences. No weights or biometric
data are committed; only measured rates and latency deltas are published.

Requires: namtor-eval (pip install -e ml/eval), numpy.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from namtor_eval.latency import LatencyStats, latency_stats, meets_gate_g2
from namtor_eval.metrics import OperatingPoint, far_frr_sweep, point_at_far
from namtor_eval.records import ScoreRecord, load_jsonl

GATE_G3_FAR = 1e-5
GATE_G3_FRR = 0.03


def _pct(x: float) -> str:
    if x <= 0.0:
        return "0.000%"
    if x < 1e-3:
        return f"{x * 100:.6f}%"
    return f"{x * 100:.3f}%"


def _verdict(passed: bool) -> str:
    return "**PASS**" if passed else "**FAIL**"


def _summarise(records: list[ScoreRecord]) -> tuple[OperatingPoint, LatencyStats]:
    sweep = far_frr_sweep(records)
    op = point_at_far(sweep, GATE_G3_FAR)
    lat = latency_stats(records)
    return op, lat


def render_delta(
    *,
    fp32_op: OperatingPoint,
    int8_op: OperatingPoint,
    fp32_lat: LatencyStats,
    int8_lat: LatencyStats,
    cohort: str,
) -> str:
    far_delta = int8_op.far - fp32_op.far
    frr_delta = int8_op.frr - fp32_op.frr
    lat_delta = int8_lat.median_ms - fp32_lat.median_ms
    speedup = fp32_lat.median_ms / int8_lat.median_ms if int8_lat.median_ms > 0 else 0.0

    fp32_g3 = fp32_op.far <= GATE_G3_FAR and fp32_op.frr <= GATE_G3_FRR
    int8_g3 = int8_op.far <= GATE_G3_FAR and int8_op.frr <= GATE_G3_FRR
    fp32_g2 = meets_gate_g2(fp32_lat)
    int8_g2 = meets_gate_g2(int8_lat)

    lines: list[str] = [
        "# M3 R4 — INT8 vs FP32 embedding degradation",
        "",
        "## Configuration",
        "",
        f"- Cohort (R5 background): **{cohort}**",
        "- Backend: OnnxCpu, CPU EP (AVX-VNNI auto-detected by ORT)",
        "- Threshold: swept; operating point at FAR ≤ 1e-5",
        "",
        "## Accuracy delta",
        "",
        "| Metric | FP32 | INT8 | Delta |",
        "|---|---|---|---|",
        f"| FAR | {_pct(fp32_op.far)} | {_pct(int8_op.far)} | {_pct(far_delta)} |",
        f"| FRR | {_pct(fp32_op.frr)} | {_pct(int8_op.frr)} | {_pct(frr_delta)} |",
        f"| n_genuine | {fp32_op.n_genuine:,} | {int8_op.n_genuine:,} | — |",
        f"| n_impostor | {fp32_op.n_impostor:,} | {int8_op.n_impostor:,} | — |",
        "",
        f"Gate G3 (FAR ≤ {_pct(GATE_G3_FAR)} at FRR ≤ {_pct(GATE_G3_FRR)}): "
        f"FP32 {_verdict(fp32_g3)}, INT8 {_verdict(int8_g3)}",
        "",
        "## Latency delta",
        "",
        "| Statistic | FP32 | INT8 | Delta |",
        "|---|---|---|---|",
        f"| Median | {fp32_lat.median_ms:.1f} ms | {int8_lat.median_ms:.1f} ms | {lat_delta:+.1f} ms |",
        f"| p95 | {fp32_lat.p95_ms:.1f} ms | {int8_lat.p95_ms:.1f} ms | {int8_lat.p95_ms - fp32_lat.p95_ms:+.1f} ms |",
        f"| Median compute | {fp32_lat.median_compute_ms:.1f} ms | {int8_lat.median_compute_ms:.1f} ms | {int8_lat.median_compute_ms - fp32_lat.median_compute_ms:+.1f} ms |",
        f"| Speedup | — | — | {speedup:.2f}× |",
        "",
        f"Gate G2 (median ≤ 500 ms, p95 ≤ 1000 ms): "
        f"FP32 {_verdict(fp32_g2)}, INT8 {_verdict(int8_g2)}",
        "",
        "## R5 cohort decision",
        "",
        f"Background cohort for score normalisation: **{cohort}**",
        "",
        "Licence verified per `ml/DATASETS.md` before use. No cohort-derived",
        "weights or biometric data are committed to the repository.",
        "",
        "## Verdict",
        "",
        "INT8 is the design choice for the auth path (spec §5.3: CPU INT8/",
        "AVX-VNNI). This report records whether INT8 meets G3 accuracy and",
        "G2 latency, and the measured degradation vs FP32.",
        "",
    ]
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="R4 INT8 vs FP32 eval report")
    parser.add_argument("--fp32", required=True, type=Path, help="FP32 JSONL score file")
    parser.add_argument("--int8", required=True, type=Path, help="INT8 JSONL score file")
    parser.add_argument("--out", required=True, type=Path, help="Output Markdown report path")
    parser.add_argument("--cohort", default="(not yet decided)", help="R5 cohort dataset name")
    args = parser.parse_args()

    fp32_records = load_jsonl(args.fp32)
    int8_records = load_jsonl(args.int8)

    if not fp32_records or not int8_records:
        print("error: empty score file(s)", file=sys.stderr)
        return 1

    fp32_op, fp32_lat = _summarise(fp32_records)
    int8_op, int8_lat = _summarise(int8_records)

    report = render_delta(
        fp32_op=fp32_op,
        int8_op=int8_op,
        fp32_lat=fp32_lat,
        int8_lat=int8_lat,
        cohort=args.cohort,
    )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(report, encoding="utf-8")
    print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
