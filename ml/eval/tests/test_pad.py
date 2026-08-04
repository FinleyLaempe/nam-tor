# SPDX-License-Identifier: GPL-3.0-only
import pytest

from namtor_eval.pad import pad_rates
from namtor_eval.records import Latency, ScoreRecord

LAT = Latency(66.0, 5.0, 20.0, 3.0, 94.0)


def rec(pad_score: float, attack: str | None) -> ScoreRecord:
    genuine = attack is None
    return ScoreRecord("p", "g", genuine, 0.8, pad_score, attack, LAT)


def test_apcer_is_reported_per_attack_type_not_averaged():
    records = [
        rec(0.9, None), rec(0.9, None),                     # bona fide
        rec(0.1, "print_colour_laser"),                     # correctly rejected
        rec(0.9, "screen_replay_phone"),                    # wrongly accepted
        rec(0.9, "screen_replay_phone"),                    # wrongly accepted
    ]
    report = pad_rates(records, threshold=0.5)

    assert report.apcer["print_colour_laser"] == pytest.approx(0.0)
    assert report.apcer["screen_replay_phone"] == pytest.approx(1.0)
    # An averaged figure would report 0.5 and hide that replay defeats us.
    assert "average" not in report.apcer


def test_bpcer_counts_bona_fide_wrongly_called_attacks():
    records = [
        rec(0.9, None), rec(0.9, None), rec(0.1, None), rec(0.9, None),
        rec(0.1, "print_colour_laser"),
    ]
    report = pad_rates(records, threshold=0.5)
    assert report.bpcer == pytest.approx(0.25)
    assert report.n_bonafide == 4


def test_sample_counts_are_reported_per_attack_type():
    records = [
        rec(0.9, None),
        rec(0.1, "print_ir_optimised"),
        rec(0.2, "print_ir_optimised"),
        rec(0.9, "mask_2d_cutout"),
    ]
    report = pad_rates(records, threshold=0.5)
    assert report.n_attack["print_ir_optimised"] == 2
    assert report.n_attack["mask_2d_cutout"] == 1


def test_worst_apcer_surfaces_the_attack_that_works():
    records = [
        rec(0.9, None),
        rec(0.1, "print_colour_laser"),
        rec(0.9, "screen_replay_monitor"),
    ]
    report = pad_rates(records, threshold=0.5)
    attack, rate = report.worst_apcer()
    assert attack == "screen_replay_monitor"
    assert rate == pytest.approx(1.0)


def test_empty_attack_set_yields_empty_apcer_not_a_crash():
    report = pad_rates([rec(0.9, None)], threshold=0.5)
    assert report.apcer == {}
    assert report.worst_apcer() is None
