"""The benchmark gate compares a change with its base inside each block of one run."""
import os
import random
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "bench"))
from leaderboard_regression import compare  # noqa: E402


def _runs(factor, load, rnd):
    return [0.010 * busy * factor * rnd.uniform(0.98, 1.02) for busy in load for _ in range(5)]


def test_a_regression_shows_through_a_machine_whose_load_drifts():
    rnd = random.Random(7)
    load = [1.0, 1.6, 1.0, 2.2, 1.3, 1.0]
    base = {"status": "ok", "blocks": 6, "median": 0.010, "times": _runs(1.0, load, rnd)}
    same = {**base, "times": _runs(1.0, load, rnd)}
    slower = {**base, "times": _runs(1.15, load, rnd)}
    assert compare([("same", base, same)], 0.10, paired=True) == (0, 1)
    assert compare([("slower", base, slower)], 0.10, paired=True) == (1, 1)
    # runs pooled over the blocks cannot tell a 15% regression from that drift
    assert compare([("slower", base, slower)], 0.10, paired=False) == (0, 1)
