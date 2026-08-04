# SPDX-License-Identifier: GPL-3.0-only
import re

from namtor_eval.latency import LatencyStats
from namtor_eval.metrics import OperatingPoint
from namtor_eval.pad import PadReport
from namtor_eval.report import render_report, write_csv

POINT = OperatingPoint(
    threshold=0.62,
    far=8e-6,
    frr=0.021,
    n_genuine=4_000,
    n_impostor=250_000,
    far_ci=(2e-6, 2.4e-5),
    frr_ci=(0.017, 0.026),
)

PAD = PadReport(
    threshold=0.5,
    bpcer=0.03,
    n_bonafide=4_000,
    apcer={"print_colour_laser": 0.01, "screen_replay_phone": 0.0},
    n_attack={"print_colour_laser": 500, "screen_replay_phone": 500},
    apcer_ci={"print_colour_laser": (0.004, 0.024), "screen_replay_phone": (0.0, 0.008)},
)

LAT = LatencyStats(
    n=4_000, median_ms=112.0, p95_ms=430.0, p99_ms=610.0,
    median_capture_ms=66.0, median_compute_ms=46.0,
)


def _report() -> str:
    return render_report(
        title="Nam-tor eval — balanced",
        operating_point=POINT,
        pad_report=PAD,
        latency=LAT,
        config_note="AdaFace R50 INT8, 5-pair burst, per-user z-norm",
    )


def test_report_contains_the_required_sections():
    md = _report()
    for section in ("## Accuracy", "## Presentation attack detection", "## Latency"):
        assert section in md, f"missing section: {section}"


def test_every_reported_rate_carries_its_sample_size():
    # Spec §10.6: "A number without its sample size is not published."
    md = _report()
    rate_lines = [
        line for line in md.splitlines()
        if re.search(r"\b(FAR|FRR|APCER|BPCER)\b", line) and "|" in line
    ]
    assert rate_lines, "no rate rows found in report"
    for line in rate_lines:
        assert re.search(r"n\s*=\s*[\d,]+", line), f"rate without sample size: {line}"


def test_apcer_rows_are_per_attack_type():
    md = _report()
    assert "print_colour_laser" in md
    assert "screen_replay_phone" in md


def test_gate_verdicts_are_stated_explicitly():
    md = _report()
    assert "G2" in md and "G3" in md and "G4" in md


def test_confidence_intervals_are_shown():
    md = _report()
    assert "95% CI" in md


def test_operating_points_an_order_of_magnitude_apart_render_differently():
    # Fixed 3-decimal percent renders 1e-5 and 1e-6 identically as "0.001%",
    # which would make Balanced and Paranoid indistinguishable in the report
    # whose purpose is to compare them.
    from namtor_eval.report import _pct

    assert _pct(1e-5) != _pct(1e-6)
    assert _pct(0.0) == "0.000%"
    assert _pct(0.021) == "2.100%"


def test_csv_export_has_a_header_and_one_row_per_point(tmp_path):
    p = tmp_path / "det.csv"
    write_csv(p, [POINT, POINT])
    lines = p.read_text().strip().splitlines()
    assert lines[0].startswith("threshold,far,frr")
    assert len(lines) == 3
