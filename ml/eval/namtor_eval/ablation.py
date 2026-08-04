# SPDX-License-Identifier: GPL-3.0-only
"""Ablation table: which accuracy levers earned their cost.

Each run is a score file produced with one additional lever enabled. The table
is the project's primary evidence for design decisions (spec §10.6).
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from .latency import latency_stats
from .metrics import far_frr_sweep, point_at_far
from .records import load_jsonl


@dataclass(frozen=True)
class AblationRow:
    name: str
    far: float
    frr: float
    median_ms: float
    n_genuine: int
    n_impostor: int


def ablation_table(
    runs: dict[str, str | Path], target_far: float
) -> list[AblationRow]:
    """Builds one row per run, each evaluated at the same target FAR."""
    rows: list[AblationRow] = []
    for name, path in runs.items():
        records = load_jsonl(path)
        points = far_frr_sweep(records)
        try:
            point = point_at_far(points, target_far)
        except ValueError as exc:
            raise ValueError(f"run {name!r}: {exc}") from exc

        rows.append(
            AblationRow(
                name=name,
                far=point.far,
                frr=point.frr,
                median_ms=latency_stats(records).median_ms,
                n_genuine=point.n_genuine,
                n_impostor=point.n_impostor,
            )
        )
    return rows


def render_ablation(rows: list[AblationRow]) -> str:
    """Renders the ablation table as Markdown."""
    out = [
        "| Configuration | FAR | FRR | Median latency | Genuine | Impostor |",
        "|---|---|---|---|---|---|",
    ]
    for r in rows:
        out.append(
            f"| {r.name} | {r.far * 100:.4f}% | {r.frr * 100:.3f}% "
            f"| {r.median_ms:.1f} ms | n = {r.n_genuine:,} | n = {r.n_impostor:,} |"
        )
    return "\n".join(out)
