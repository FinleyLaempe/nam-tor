# SPDX-License-Identifier: GPL-3.0-only
"""FAR/FRR computation with confidence intervals.

A comparison is *accepted* when ``score >= threshold``. FAR is the fraction of
zero-effort impostor comparisons accepted; FRR the fraction of genuine
comparisons rejected. Presentation attacks are excluded — they are measured by
APCER in :mod:`namtor_eval.pad`.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

import numpy as np

from .records import ScoreRecord


@dataclass(frozen=True)
class OperatingPoint:
    threshold: float
    far: float
    frr: float
    n_genuine: int
    n_impostor: int
    far_ci: tuple[float, float]
    frr_ci: tuple[float, float]


def wilson_interval(k: int, n: int, z: float = 1.96) -> tuple[float, float]:
    """Wilson score interval for a binomial proportion.

    Used instead of the normal approximation because error rates here are near
    zero with large n, where the normal approximation is badly behaved.
    """
    if n == 0:
        return (0.0, 1.0)
    p = k / n
    denom = 1.0 + z * z / n
    centre = (p + z * z / (2 * n)) / denom
    margin = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / denom
    return (max(0.0, centre - margin), min(1.0, centre + margin))


def far_frr_sweep(
    records: list[ScoreRecord],
    thresholds: list[float] | None = None,
) -> list[OperatingPoint]:
    """Computes FAR and FRR across a threshold sweep.

    When ``thresholds`` is omitted, every observed score is used as a
    candidate threshold, which is sufficient to recover the exact DET curve.
    """
    genuine = np.sort(np.array([r.score for r in records if r.genuine], dtype=float))
    impostor = np.sort(
        np.array(
            [r.score for r in records if not r.genuine and not r.is_attack],
            dtype=float,
        )
    )

    if thresholds is None:
        both = np.concatenate([genuine, impostor]) if len(records) else np.array([])
        candidates = np.unique(both)
    else:
        candidates = np.unique(np.asarray(list(thresholds), dtype=float))

    n_g = len(genuine)
    n_i = len(impostor)

    # Counting per threshold with a boolean mask is O(T*N), which at the
    # >=100k impostor comparisons gate G3.2 requires would be ~1e10 operations
    # and take hours. Both arrays are sorted, so binary search gives the same
    # counts in O((N + T) log N).
    #
    #   false accepts  = impostor >= t  = n_i - searchsorted(impostor, t, 'left')
    #   false rejects  = genuine  < t   =       searchsorted(genuine,  t, 'left')
    false_accepts = n_i - np.searchsorted(impostor, candidates, side="left")
    false_rejects = np.searchsorted(genuine, candidates, side="left")

    points: list[OperatingPoint] = []
    for t, fa, fr in zip(candidates, false_accepts, false_rejects):
        points.append(
            OperatingPoint(
                threshold=float(t),
                far=int(fa) / n_i if n_i else 0.0,
                frr=int(fr) / n_g if n_g else 0.0,
                n_genuine=n_g,
                n_impostor=n_i,
                far_ci=wilson_interval(int(fa), n_i),
                frr_ci=wilson_interval(int(fr), n_g),
            )
        )
    return points


def point_at_far(
    points: list[OperatingPoint], target_far: float
) -> OperatingPoint:
    """Returns the point with lowest FRR among those meeting ``target_far``."""
    eligible = [p for p in points if p.far <= target_far]
    if not eligible:
        raise ValueError(f"no operating point achieves FAR <= {target_far}")
    return min(eligible, key=lambda p: p.frr)
