# SPDX-License-Identifier: GPL-3.0-only
import json

import pytest

from namtor_eval.ablation import ablation_table, render_ablation


def _write_run(path, genuine_scores, impostor_scores, total_ms):
    lat = {
        "capture_ms": 66.0, "detect_ms": 5.0, "embed_ms": 20.0,
        "pad_ms": 3.0, "total_ms": total_ms,
    }
    rows = []
    for s in genuine_scores:
        rows.append({"probe_id": "p", "gallery_id": "g", "genuine": True,
                     "score": s, "pad_score": 0.9, "attack_type": None,
                     "latency": lat})
    for s in impostor_scores:
        rows.append({"probe_id": "p", "gallery_id": "h", "genuine": False,
                     "score": s, "pad_score": 0.9, "attack_type": None,
                     "latency": lat})
    path.write_text("\n".join(json.dumps(r) for r in rows) + "\n")


def test_table_has_one_row_per_run_in_insertion_order(tmp_path):
    a = tmp_path / "a.jsonl"
    b = tmp_path / "b.jsonl"
    _write_run(a, [0.6] * 50, [0.1] * 50, 100.0)
    _write_run(b, [0.9] * 50, [0.1] * 50, 130.0)

    rows = ablation_table({"baseline": a, "+ flip TTA": b}, target_far=0.0)

    assert [r.name for r in rows] == ["baseline", "+ flip TTA"]


def test_row_carries_sample_sizes_and_latency(tmp_path):
    a = tmp_path / "a.jsonl"
    _write_run(a, [0.8] * 40, [0.1] * 60, 111.0)

    rows = ablation_table({"baseline": a}, target_far=0.0)

    assert rows[0].n_genuine == 40
    assert rows[0].n_impostor == 60
    assert rows[0].median_ms == pytest.approx(111.0)


def test_render_includes_sample_sizes(tmp_path):
    a = tmp_path / "a.jsonl"
    _write_run(a, [0.8] * 40, [0.1] * 60, 111.0)
    md = render_ablation(ablation_table({"baseline": a}, target_far=0.0))
    assert "n = 40" in md
    assert "n = 60" in md


def test_unreachable_target_far_names_the_offending_run(tmp_path):
    a = tmp_path / "a.jsonl"
    # Fully overlapping distributions: no threshold achieves FAR 0 with any
    # genuine accepted.
    _write_run(a, [0.5] * 10, [0.5] * 10, 100.0)

    with pytest.raises(ValueError, match="baseline"):
        ablation_table({"baseline": a}, target_far=0.0)
