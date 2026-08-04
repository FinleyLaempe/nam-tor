# SPDX-License-Identifier: GPL-3.0-only
"""Score record types mirroring `crates/namtor-eval/src/score.rs`."""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Latency:
    capture_ms: float
    detect_ms: float
    embed_ms: float
    pad_ms: float
    total_ms: float

    @property
    def compute_ms(self) -> float:
        """Everything that is not waiting on the camera."""
        return self.total_ms - self.capture_ms


@dataclass(frozen=True)
class ScoreRecord:
    probe_id: str
    gallery_id: str
    genuine: bool
    score: float
    pad_score: float
    attack_type: str | None
    latency: Latency

    @property
    def is_attack(self) -> bool:
        """True for presentation attacks only.

        A zero-effort impostor has ``attack_type is None`` and is *not* an
        attack presentation. Conflating the two corrupts FAR and APCER alike.
        """
        return self.attack_type is not None


def load_jsonl(path: str | Path) -> list[ScoreRecord]:
    """Loads a JSON Lines score file emitted by the Rust pipeline."""
    records: list[ScoreRecord] = []
    with open(path, encoding="utf-8") as fh:
        for lineno, line in enumerate(fh):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError as exc:
                raise ValueError(f"malformed record at line {lineno}: {exc}") from exc
            records.append(
                ScoreRecord(
                    probe_id=row["probe_id"],
                    gallery_id=row["gallery_id"],
                    genuine=row["genuine"],
                    score=row["score"],
                    pad_score=row["pad_score"],
                    attack_type=row.get("attack_type"),
                    latency=Latency(**row["latency"]),
                )
            )
    return records
