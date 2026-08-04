# SPDX-License-Identifier: GPL-3.0-only
"""End-to-end latency statistics and gate G2 evaluation."""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

from .records import ScoreRecord

#: Spec §12 G2.1
GATE_G2_MEDIAN_MS = 500.0
#: Spec §12 G2.2
GATE_G2_P95_MS = 1000.0


@dataclass(frozen=True)
class LatencyStats:
    n: int
    median_ms: float
    p95_ms: float
    p99_ms: float
    median_capture_ms: float
    median_compute_ms: float


def latency_stats(records: list[ScoreRecord]) -> LatencyStats:
    """Computes the latency distribution, split into capture and compute."""
    if not records:
        raise ValueError("no records: refusing to report latency for an empty set")

    total = np.array([r.latency.total_ms for r in records], dtype=float)
    capture = np.array([r.latency.capture_ms for r in records], dtype=float)
    compute = np.array([r.latency.compute_ms for r in records], dtype=float)

    return LatencyStats(
        n=len(records),
        median_ms=float(np.median(total)),
        p95_ms=float(np.percentile(total, 95)),
        p99_ms=float(np.percentile(total, 99)),
        median_capture_ms=float(np.median(capture)),
        median_compute_ms=float(np.median(compute)),
    )


def meets_gate_g2(stats: LatencyStats) -> bool:
    """True when both G2.1 (median) and G2.2 (p95) thresholds are met."""
    return stats.median_ms <= GATE_G2_MEDIAN_MS and stats.p95_ms <= GATE_G2_P95_MS
