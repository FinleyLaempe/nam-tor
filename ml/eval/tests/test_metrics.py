# SPDX-License-Identifier: GPL-3.0-only
import numpy as np
import pytest

from namtor_eval.metrics import far_frr_sweep, point_at_far, wilson_interval
from namtor_eval.records import Latency, ScoreRecord

LAT = Latency(66.0, 5.0, 20.0, 3.0, 94.0)


def rec(genuine: bool, score: float, attack: str | None = None) -> ScoreRecord:
    return ScoreRecord("p", "g", genuine, score, 0.9, attack, LAT)


def test_perfectly_separated_scores_have_zero_error_between_the_clusters():
    records = [
        rec(True, 0.9), rec(True, 0.8), rec(True, 0.7),
        rec(False, 0.3), rec(False, 0.2), rec(False, 0.1),
    ]
    points = far_frr_sweep(records, thresholds=[0.5])
    assert len(points) == 1
    assert points[0].far == pytest.approx(0.0)
    assert points[0].frr == pytest.approx(0.0)
    assert points[0].n_genuine == 3
    assert points[0].n_impostor == 3


def test_threshold_above_a_genuine_score_rejects_it():
    records = [
        rec(True, 0.9), rec(True, 0.8), rec(True, 0.7),
        rec(False, 0.3), rec(False, 0.2), rec(False, 0.1),
    ]
    points = far_frr_sweep(records, thresholds=[0.75])
    # 0.7 falls below threshold: one of three genuine rejected.
    assert points[0].frr == pytest.approx(1 / 3)
    assert points[0].far == pytest.approx(0.0)


def test_threshold_below_an_impostor_score_accepts_it():
    records = [rec(True, 0.9), rec(False, 0.3), rec(False, 0.1)]
    points = far_frr_sweep(records, thresholds=[0.2])
    assert points[0].far == pytest.approx(0.5)


def test_presentation_attacks_are_excluded_from_far():
    # Attacks belong to APCER, never to FAR. Including them would understate
    # or overstate FAR depending on attack difficulty.
    records = [
        rec(True, 0.9),
        rec(False, 0.1),
        rec(False, 0.95, "print_colour_laser"),
    ]
    points = far_frr_sweep(records, thresholds=[0.5])
    assert points[0].n_impostor == 1
    assert points[0].far == pytest.approx(0.0)


def test_point_at_far_returns_the_strictest_point_meeting_the_target():
    records = [rec(True, s / 100) for s in range(50, 100)] + [
        rec(False, s / 100) for s in range(0, 50)
    ]
    points = far_frr_sweep(records)
    p = point_at_far(points, 1e-5)
    assert p.far <= 1e-5


def test_point_at_far_raises_when_target_is_unreachable():
    records = [rec(True, 0.5), rec(False, 0.5)]
    points = far_frr_sweep(records)
    with pytest.raises(ValueError, match="no operating point"):
        point_at_far(points, 0.0)


def test_wilson_interval_widens_as_sample_shrinks():
    lo_big, hi_big = wilson_interval(1, 100_000)
    lo_small, hi_small = wilson_interval(1, 10)
    assert (hi_big - lo_big) < (hi_small - lo_small)


def test_wilson_interval_of_zero_events_is_bounded_below_by_zero():
    lo, hi = wilson_interval(0, 1000)
    assert lo == pytest.approx(0.0)
    assert 0.0 < hi < 0.01


def test_sweep_is_fast_enough_for_the_sample_size_gate_g3_requires():
    # Gate G3.2 requires >=100k impostor comparisons. A per-threshold boolean
    # mask would be O(T*N) and take hours here; the sorted binary search is
    # O((N+T) log N). This test fails loudly if the implementation regresses.
    import time

    rng = np.random.default_rng(0)
    records = [rec(True, float(s)) for s in rng.normal(0.8, 0.05, 5_000)] + [
        rec(False, float(s)) for s in rng.normal(0.2, 0.05, 100_000)
    ]

    start = time.perf_counter()
    points = far_frr_sweep(records)
    elapsed = time.perf_counter() - start

    assert len(points) > 1000
    assert elapsed < 5.0, f"sweep took {elapsed:.1f}s; implementation is O(T*N)"


def test_sweep_counts_match_a_naive_reference_implementation():
    # Guards the binary-search boundary conditions, which are easy to get
    # off by one against the >= / < semantics.
    rng = np.random.default_rng(1)
    scores = np.round(rng.uniform(0, 1, 200), 3)
    records = [rec(i % 2 == 0, float(s)) for i, s in enumerate(scores)]

    genuine = [r.score for r in records if r.genuine]
    impostor = [r.score for r in records if not r.genuine]

    for p in far_frr_sweep(records):
        naive_far = sum(1 for s in impostor if s >= p.threshold) / len(impostor)
        naive_frr = sum(1 for s in genuine if s < p.threshold) / len(genuine)
        assert p.far == pytest.approx(naive_far)
        assert p.frr == pytest.approx(naive_frr)
