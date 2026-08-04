# SPDX-License-Identifier: GPL-3.0-only
import pytest

from namtor_eval.latency import latency_stats, meets_gate_g2
from namtor_eval.records import Latency, ScoreRecord


def rec(total_ms: float, capture_ms: float = 66.0) -> ScoreRecord:
    compute = total_ms - capture_ms
    return ScoreRecord(
        "p", "g", True, 0.8, 0.9, None,
        Latency(capture_ms, compute * 0.2, compute * 0.7, compute * 0.1, total_ms),
    )


def test_median_and_percentiles():
    records = [rec(float(t)) for t in range(100, 200)]
    s = latency_stats(records)
    assert s.n == 100
    assert s.median_ms == pytest.approx(149.5, abs=1.0)
    assert s.p95_ms == pytest.approx(194.0, abs=2.0)


def test_capture_and_compute_are_reported_separately():
    # Spec §12 G2 requires the split, because capture dominates and the
    # distinction determines where optimisation effort belongs.
    records = [rec(100.0, capture_ms=66.0) for _ in range(10)]
    s = latency_stats(records)
    assert s.median_capture_ms == pytest.approx(66.0)
    assert s.median_compute_ms == pytest.approx(34.0)


def test_gate_g2_passes_within_budget():
    records = [rec(110.0) for _ in range(100)]
    assert meets_gate_g2(latency_stats(records)) is True


def test_gate_g2_fails_when_median_exceeds_500ms():
    records = [rec(600.0) for _ in range(100)]
    assert meets_gate_g2(latency_stats(records)) is False


def test_gate_g2_fails_when_p95_exceeds_1000ms():
    # 10 of 100 attempts at 1500 ms: numpy's default linear percentile puts
    # p95 inside the 1500-block (index 94.05 of [100]*90 + [1500]*10), so
    # p95 = 1500 > 1000 and G2.2 fails. A 95/5 split would interpolate to
    # 170 and accidentally pass the gate.
    records = [rec(100.0) for _ in range(90)] + [rec(1500.0) for _ in range(10)]
    stats = latency_stats(records)
    assert stats.median_ms < 500
    assert stats.p95_ms > 1000.0
    assert meets_gate_g2(stats) is False


def test_empty_input_raises_rather_than_reporting_zero():
    # Reporting 0 ms for no data would silently pass the gate.
    with pytest.raises(ValueError, match="no records"):
        latency_stats([])
