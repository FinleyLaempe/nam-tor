# SPDX-License-Identifier: GPL-3.0-only
"""Presentation attack detection rates, per ISO/IEC 30107-3.

APCER is reported **per attack type and never averaged**. A single averaged
figure conceals exactly the attack that works, which is the one that matters.
"""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass

from .metrics import wilson_interval
from .records import ScoreRecord


@dataclass(frozen=True)
class PadReport:
    threshold: float
    bpcer: float
    n_bonafide: int
    apcer: dict[str, float]
    n_attack: dict[str, int]
    apcer_ci: dict[str, tuple[float, float]]

    def worst_apcer(self) -> tuple[str, float] | None:
        """The attack type with the highest acceptance rate, or ``None``."""
        if not self.apcer:
            return None
        return max(self.apcer.items(), key=lambda kv: kv[1])


def pad_rates(records: list[ScoreRecord], threshold: float) -> PadReport:
    """Computes BPCER and per-type APCER at a PAD threshold.

    A presentation is classified bona fide when ``pad_score >= threshold``.
    """
    bonafide = [r for r in records if not r.is_attack]
    n_bonafide = len(bonafide)
    false_rejects = sum(1 for r in bonafide if r.pad_score < threshold)

    accepted: dict[str, int] = defaultdict(int)
    totals: dict[str, int] = defaultdict(int)
    for r in records:
        if not r.is_attack:
            continue
        assert r.attack_type is not None
        totals[r.attack_type] += 1
        if r.pad_score >= threshold:
            accepted[r.attack_type] += 1

    apcer = {k: accepted[k] / totals[k] for k in totals}
    apcer_ci = {k: wilson_interval(accepted[k], totals[k]) for k in totals}

    return PadReport(
        threshold=threshold,
        bpcer=false_rejects / n_bonafide if n_bonafide else 0.0,
        n_bonafide=n_bonafide,
        apcer=apcer,
        n_attack=dict(totals),
        apcer_ci=apcer_ci,
    )
