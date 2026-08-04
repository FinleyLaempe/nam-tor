# SPDX-License-Identifier: GPL-3.0-only
"""Markdown and CSV rendering of evaluation results.

Enforces the spec §10.6 rule that no rate is published without its sample
size. The corresponding test fails CI if that rule is ever weakened.
"""

from __future__ import annotations

import csv
from pathlib import Path

from .latency import (
    GATE_G2_MEDIAN_MS,
    GATE_G2_P95_MS,
    LatencyStats,
    meets_gate_g2,
)
from .metrics import OperatingPoint
from .pad import PadReport

#: Spec §12 G3.1
GATE_G3_FAR = 1e-5
GATE_G3_FRR = 0.03
#: Spec §12 G4.1
GATE_G4_APCER = 0.05
GATE_G4_BPCER = 0.05


def _pct(x: float) -> str:
    """Formats a rate as a percentage with enough precision to be useful.

    Fixed 3-decimal percent renders both 1e-5 and 1e-6 as "0.001%", which
    would make the Balanced and Paranoid operating points indistinguishable
    in the report that exists to compare them. Small rates therefore get
    additional digits.
    """
    if x <= 0.0:
        return "0.000%"
    if x < 1e-3:
        return f"{x * 100:.6f}%"
    return f"{x * 100:.3f}%"


def _verdict(passed: bool) -> str:
    return "**PASS**" if passed else "**FAIL**"


def render_report(
    *,
    title: str,
    operating_point: OperatingPoint,
    pad_report: PadReport,
    latency: LatencyStats,
    config_note: str,
) -> str:
    """Renders a full evaluation report as Markdown."""
    p = operating_point

    g3_pass = p.far <= GATE_G3_FAR and p.frr <= GATE_G3_FRR
    g2_pass = meets_gate_g2(latency)
    worst = pad_report.worst_apcer()
    g4_pass = (
        pad_report.bpcer <= GATE_G4_BPCER
        and (worst is None or worst[1] <= GATE_G4_APCER)
    )

    lines: list[str] = [
        f"# {title}",
        "",
        f"Configuration: {config_note}",
        "",
        "## Accuracy",
        "",
        f"Threshold: `{p.threshold:.4f}`",
        "",
        "| Metric | Value | 95% CI | Sample |",
        "|---|---|---|---|",
        f"| FAR | {_pct(p.far)} | {_pct(p.far_ci[0])} – {_pct(p.far_ci[1])} "
        f"| n = {p.n_impostor:,} |",
        f"| FRR | {_pct(p.frr)} | {_pct(p.frr_ci[0])} – {_pct(p.frr_ci[1])} "
        f"| n = {p.n_genuine:,} |",
        "",
        f"Gate G3 (FAR ≤ {_pct(GATE_G3_FAR)} at FRR ≤ {_pct(GATE_G3_FRR)}): "
        f"{_verdict(g3_pass)}",
        "",
        "## Presentation attack detection",
        "",
        f"PAD threshold: `{pad_report.threshold:.4f}`",
        "",
        "| Presentation | Rate | 95% CI | Sample |",
        "|---|---|---|---|",
        f"| BPCER | {_pct(pad_report.bpcer)} | — | n = {pad_report.n_bonafide:,} |",
    ]

    for attack in sorted(pad_report.apcer):
        ci = pad_report.apcer_ci[attack]
        lines.append(
            f"| APCER `{attack}` | {_pct(pad_report.apcer[attack])} "
            f"| {_pct(ci[0])} – {_pct(ci[1])} | n = {pad_report.n_attack[attack]:,} |"
        )

    lines += [
        "",
        "APCER is reported per attack type and never averaged (spec §10.6).",
        "",
        f"Gate G4 (APCER ≤ {_pct(GATE_G4_APCER)} per type at "
        f"BPCER ≤ {_pct(GATE_G4_BPCER)}): {_verdict(g4_pass)}",
        "",
        "## Latency",
        "",
        "| Statistic | Value |",
        "|---|---|",
        f"| Median | {latency.median_ms:.1f} ms |",
        f"| p95 | {latency.p95_ms:.1f} ms |",
        f"| p99 | {latency.p99_ms:.1f} ms |",
        f"| Median capture | {latency.median_capture_ms:.1f} ms |",
        f"| Median compute | {latency.median_compute_ms:.1f} ms |",
        f"| Attempts | n = {latency.n:,} |",
        "",
        f"Gate G2 (median ≤ {GATE_G2_MEDIAN_MS:.0f} ms, "
        f"p95 ≤ {GATE_G2_P95_MS:.0f} ms): {_verdict(g2_pass)}",
        "",
    ]
    return "\n".join(lines)


def write_csv(path: str | Path, points: list[OperatingPoint]) -> None:
    """Writes the DET sweep as CSV for plotting."""
    with open(path, "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(
            ["threshold", "far", "frr", "n_genuine", "n_impostor",
             "far_ci_lo", "far_ci_hi", "frr_ci_lo", "frr_ci_hi"]
        )
        for p in points:
            w.writerow(
                [p.threshold, p.far, p.frr, p.n_genuine, p.n_impostor,
                 p.far_ci[0], p.far_ci[1], p.frr_ci[0], p.frr_ci[1]]
            )
