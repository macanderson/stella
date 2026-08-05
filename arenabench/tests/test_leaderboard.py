# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Which number wins a dimension, and who is allowed to be holding it.

The leaderboard is where a wrong-but-plausible total becomes a published claim,
so what is pinned here is the two ways a crown is handed out by accident: a
direction that is not a property of the number alone, and a contestant that has
not measured anything yet.
"""

from __future__ import annotations

from arenabench.model import DIMENSIONS, DIMENSIONS_BY_KEY
from arenabench.telemetry import leaders

# --------------------------------------------------------------------------
# dimensions and leaders
# --------------------------------------------------------------------------


class TestDimensions:
    def test_lower_is_better_for_cost_and_clock(self):
        assert DIMENSIONS_BY_KEY["priced_cost"].better(1.0, 2.0)
        assert DIMENSIONS_BY_KEY["clock_time"].better(10, 20)

    def test_the_cost_crown_is_the_shared_table_not_the_self_reports(self):
        """The dimension that crowns must be the one `pricing.py` computes.

        Both arms report a cost and neither is checked, so the difference
        between two self-reports is a difference between two pricing tables:
        measured here, one arm's own figure made a 2.5x token gap read as a
        7.9x cost gap. `priced_cost` is those same token counts through one
        table, which is the only cost number two seats can be ranked on.
        """
        assert DIMENSIONS_BY_KEY["priced_cost"].direction == "lower"
        assert DIMENSIONS_BY_KEY["total_cost"].direction == "neutral"
        assert DIMENSIONS_BY_KEY["total_cost"].label == "Self-Reported"

    def test_cache_read_is_higher_is_better(self):
        """Cache reads are prompt tokens you did not pay full price for."""
        assert DIMENSIONS_BY_KEY["cache_read"].better(9000, 10)

    def test_cache_write_crowns_nobody(self):
        """Writes are a real cost paid to enable future reads, so neither
        direction is self-evidently better and the scoreboard must not claim
        one is."""
        dim = DIMENSIONS_BY_KEY["cache_write"]
        assert dim.direction == "neutral"
        assert not dim.better(1, 2) and not dim.better(2, 1)


class TestLeaders:
    def test_a_contestant_with_no_judged_trial_cannot_lead(self):
        """The bug this pins: a seat that has not spent anything has cost 0,
        clock 0 and tokens 0 — so a naive "lowest wins" hands it four crowns
        for the first minutes of every match."""
        totals = {
            "spent": {
                "judged": 4, "passed": 3, "solve_rate": 75.0, "total_cost": 2.0,
                "clock_time": 300, "tokens_in": 10, "tokens_out": 10, "cache_read": 5,
            },
            "idle": {
                "judged": 0, "passed": 0, "solve_rate": 0.0, "total_cost": 0.0,
                "clock_time": 0, "tokens_in": 0, "tokens_out": 0, "cache_read": 0,
            },
        }
        won = leaders(totals, DIMENSIONS)
        assert all(winners == ["spent"] for winners in won.values()), won

    def test_ties_return_every_tied_contestant(self):
        totals = {
            "a": {
                "judged": 1, "solve_rate": 50.0, "total_cost": 1.0, "clock_time": 5,
                "tokens_in": 1, "tokens_out": 1, "cache_read": 1,
            },
            "b": {
                "judged": 1, "solve_rate": 50.0, "total_cost": 1.0, "clock_time": 5,
                "tokens_in": 1, "tokens_out": 1, "cache_read": 1,
            },
        }
        assert sorted(leaders(totals, DIMENSIONS)["solve_rate"]) == ["a", "b"]

    def test_no_judged_trials_anywhere_crowns_nobody(self):
        assert leaders({"a": {"judged": 0}}, DIMENSIONS) == {}

    def test_the_cost_crown_ignores_what_the_agents_reported(self):
        """The two columns disagree on the winner, and only one of them is a
        measurement. `cheap` says it spent a tenth of what `pricey` did; run
        both seats' tokens through one table and `pricey` is the cheaper run."""
        totals = {
            "cheap": {"judged": 2, "total_cost": 0.10, "priced_cost": 9.00},
            "pricey": {"judged": 2, "total_cost": 1.00, "priced_cost": 2.00},
        }
        won = leaders(totals, DIMENSIONS)
        assert won["priced_cost"] == ["pricey"]
        assert "total_cost" not in won, "a self-report must crown nobody"

    def test_an_unpriced_seat_does_not_win_cost_by_being_unknown(self):
        """`priced_cost` is None for a model the table does not cover. Read as
        zero it is the cheapest possible run, so the seat nobody can cost would
        take the crown — and it would look exactly like a real win."""
        totals = {
            "priced": {"judged": 2, "priced_cost": 4.00},
            "unpriced": {"judged": 2, "priced_cost": None},
        }
        assert leaders(totals, DIMENSIONS)["priced_cost"] == ["priced"]

    def test_a_dimension_nobody_has_a_number_for_crowns_nobody(self):
        totals = {
            "a": {"judged": 1, "priced_cost": None},
            "b": {"judged": 1, "priced_cost": None},
        }
        assert "priced_cost" not in leaders(totals, DIMENSIONS)

