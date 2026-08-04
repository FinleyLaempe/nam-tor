# SPDX-License-Identifier: GPL-3.0-only
import json

from namtor_eval.records import ScoreRecord, load_jsonl


def _row(genuine: bool, score: float, attack: str | None = None) -> dict:
    return {
        "probe_id": "p001",
        "gallery_id": "g001",
        "genuine": genuine,
        "score": score,
        "pad_score": 0.9,
        "attack_type": attack,
        "latency": {
            "capture_ms": 66.0,
            "detect_ms": 5.0,
            "embed_ms": 20.0,
            "pad_ms": 3.0,
            "total_ms": 94.0,
        },
    }


def test_loads_records_written_by_the_rust_side(tmp_path):
    p = tmp_path / "scores.jsonl"
    p.write_text(
        json.dumps(_row(True, 0.82)) + "\n"
        + json.dumps(_row(False, 0.11, "screen_replay_phone")) + "\n"
    )

    records = load_jsonl(p)

    assert len(records) == 2
    assert records[0].genuine is True
    assert records[0].latency.total_ms == 94.0
    assert records[1].attack_type == "screen_replay_phone"


def test_is_attack_distinguishes_impostors_from_presentation_attacks(tmp_path):
    p = tmp_path / "scores.jsonl"
    p.write_text(
        json.dumps(_row(False, 0.11)) + "\n"
        + json.dumps(_row(False, 0.44, "print_colour_laser")) + "\n"
    )

    records = load_jsonl(p)

    # A zero-effort impostor is not a presentation attack. Conflating them
    # would corrupt both FAR and APCER.
    assert records[0].is_attack is False
    assert records[1].is_attack is True


def test_blank_lines_are_ignored(tmp_path):
    p = tmp_path / "scores.jsonl"
    p.write_text(json.dumps(_row(True, 0.5)) + "\n\n")
    assert len(load_jsonl(p)) == 1
