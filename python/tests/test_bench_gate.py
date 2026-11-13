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


def test_the_instruction_gate_counts_one_thread_and_fails_past_one_percent():
    from instructions import command, verdict
    from leaderboard import suite

    b = {x["id"]: x for x in suite()}
    cmd, env = command(b["ripgrep/literal"], "/n/scrutate", "/c")
    assert cmd[:3] == ["/n/scrutate", "grep", "-j1"] and env == {}
    cmd, env = command(b["ripgrep/structural"], "/n/scrutate", "/c")
    assert cmd[:2] == ["/n/scrutate", "tsquery"] and env == {"RAYON_NUM_THREADS": "1"}
    cmd, _ = command(b["ripgrep/4 at once (literal)"], "/n/scrutate", "/c")
    assert cmd[:2] == ["/bin/sh", "-c"] and cmd[2].count("/n/scrutate grep -j1") == 4
    assert [verdict(n, 1000, 0.01)[1] for n in (1011, 1010, 1000, 989)] == ["REGRESSION", "same", "same", "faster"]
