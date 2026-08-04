# SPDX-License-Identifier: GPL-3.0-only
"""Proves the whole chain runs with no camera attached.

Generates a synthetic session with the Rust binary, confirms it replays, then
drives the Python harness end to end. This is the guard that keeps the project
CI-testable as it grows.
"""

import json
import pathlib
import shutil
import subprocess

import pytest

from namtor_eval.latency import latency_stats
from namtor_eval.metrics import far_frr_sweep, point_at_far
from namtor_eval.pad import pad_rates
from namtor_eval.records import load_jsonl
from namtor_eval.report import render_report

# The test runs from ml/eval, so a bare "target/debug/..." would resolve to
# ml/eval/target/ and never be found. Walk up to the workspace root instead.
REPO_ROOT = pathlib.Path(__file__).resolve().parents[3]


def _synth_binary() -> str | None:
    on_path = shutil.which("namtor-synth")
    if on_path:
        return on_path
    for profile in ("debug", "release"):
        candidate = REPO_ROOT / "target" / profile / "namtor-synth"
        if candidate.is_file():
            return str(candidate)
    return None


@pytest.mark.skipif(
    _synth_binary() is None,
    reason="namtor-synth not built; run `cargo build --bin namtor-synth`",
)
def test_synth_binary_produces_a_replayable_session(tmp_path):
    out = tmp_path / "session"
    subprocess.run(
        [_synth_binary(), "--out", str(out), "--pairs", "3",
         "--seed", "42", "--geometry", "face"],
        check=True,
    )

    manifest = json.loads((out / "manifest.json").read_text())
    assert manifest["version"] == 1
    assert len(manifest["frames"]) == 6
    assert manifest["frames"][0]["emitter"] == "On"
    assert manifest["frames"][1]["emitter"] == "Off"


def test_harness_produces_a_report_from_score_records(tmp_path):
    lat = {"capture_ms": 66.0, "detect_ms": 5.0, "embed_ms": 20.0,
           "pad_ms": 3.0, "total_ms": 94.0}
    rows = []
    for i in range(100):
        rows.append({"probe_id": f"p{i}", "gallery_id": f"p{i}", "genuine": True,
                     "score": 0.85, "pad_score": 0.95, "attack_type": None,
                     "latency": lat})
    for i in range(100):
        rows.append({"probe_id": f"p{i}", "gallery_id": f"q{i}", "genuine": False,
                     "score": 0.15, "pad_score": 0.95, "attack_type": None,
                     "latency": lat})
    for i in range(20):
        rows.append({"probe_id": f"a{i}", "gallery_id": "p0", "genuine": False,
                     "score": 0.80, "pad_score": 0.10,
                     "attack_type": "screen_replay_phone", "latency": lat})

    path = tmp_path / "scores.jsonl"
    path.write_text("\n".join(json.dumps(r) for r in rows) + "\n")

    records = load_jsonl(path)
    point = point_at_far(far_frr_sweep(records), target_far=0.0)
    md = render_report(
        title="smoke",
        operating_point=point,
        pad_report=pad_rates(records, threshold=0.5),
        latency=latency_stats(records),
        config_note="synthetic smoke test",
    )

    assert "## Accuracy" in md
    assert "screen_replay_phone" in md
    assert "n = 100" in md
    # The replay attack is fully rejected at pad threshold 0.5.
    assert "APCER `screen_replay_phone` | 0.000%" in md
