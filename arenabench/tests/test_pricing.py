# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The one price table both contestants are costed from.

Every agent prices its own run from its own table and no meter checks it, so a
difference between two self-reports is a fact about the tables. `pricing.py`
answers the question those numbers cannot: the same token counts, the same
rates, both seats.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

import arenabench.pricing as pricing
from arenabench.pricing import price_for, price_for_route, trial_cost


class TestPricing:
    """One table, both seats — and the rates in it are pinned.

    Source of truth for every rate below: OpenRouter's ``/models``, read
    2026-08-04. The same figures are carried, from the same source and date, by
    the Stella catalog these matches run against
    (``crates/stella-model/src/catalog.rs``, the ``openrouter`` rows). A rate
    that moves should move here by an edit that fails this test first, rather
    than by a number quietly drifting away from the catalog it is meant to
    agree with.
    """

    def test_the_gateway_rates_are_the_pinned_ones(self):
        glm = price_for("glm-5.2")
        assert (glm.input, glm.output, glm.cache_read) == (0.76, 2.42, 0.14)
        assert price_for("z-ai/glm-5.1").input == 0.966
        fable = price_for("claude-fable-5")
        assert (fable.input, fable.output) == (10.0, 50.0)

    def test_one_recorded_id_resolves_to_one_rate_however_it_was_routed(self):
        """A routed seat is launched with the bare model id and an unrouted one
        keeps its `provider/` prefix, so pricing a *recorded* string by its
        route would cost the two seats of a single match from two different
        rows — the exact subtracting-two-tables error this module exists to
        prevent. The first-party tier exists (`zai/glm-5.2` is a PRICES key
        now), and a recorded string must still never reach it."""
        assert (
            price_for("glm-5.2")
            == price_for("z-ai/glm-5.2")
            == price_for("openrouter/z-ai/glm-5.2")
            == price_for("zai/glm-5.2")
        )

    def test_an_unpriced_model_reports_nothing_rather_than_zero(self):
        assert price_for("some-model-nobody-seeded") is None
        assert trial_cost(None, tokens_in=1_000_000, tokens_out=1_000) is None

    def test_cached_tokens_are_not_billed_twice(self):
        """`tokens_in` is the total prompt count, cached included. Billing the
        cached portion at the input rate as well would charge the same tokens
        twice, at the dearest rate, against whichever agent caches best."""
        price = price_for("glm-5.2")
        cost = trial_cost(price, tokens_in=1_000_000, tokens_out=0, cache_read=900_000)
        assert cost == pytest.approx(0.1 * 0.76 + 0.9 * 0.14)


class TestRoutePricing:
    """The first-party tier is reachable only through a seat's launch record.

    `price_for` stays route-blind because a recorded model string cannot say
    which route served it; `price_for_route` takes the one string that can —
    the seat's own launch record — and unlocks the direct rate (#1498).
    """

    def test_a_seats_recorded_route_prices_at_the_first_party_rate(self):
        """z.ai's own metered rate, as `catalog.rs`'s `zai` row carries it."""
        direct = price_for_route("zai/glm-5.2")
        assert (direct.input, direct.output, direct.cache_read) == (0.60, 2.20, 0.11)

    def test_a_gateway_route_prices_at_the_gateway_rate(self):
        assert price_for_route("openrouter/z-ai/glm-5.2") == price_for("glm-5.2")

    def test_a_route_with_no_row_of_its_own_prices_at_the_gateway_tier(self):
        """No published first-party rate means the conservative tier, not a
        guess — glm-5.1 lists the same numbers on both routes."""
        assert price_for_route("zai/glm-5.1") == price_for("glm-5.1")

    def test_a_wholly_unknown_route_reports_nothing(self):
        assert price_for_route("acme/mystery-1") is None
        assert price_for_route("") is None

    def test_an_override_keyed_by_route_pins_that_route_alone(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ):
        """Override keys land on the tier they name. Stripping the prefix, as
        lookups do, would silently move a first-party override onto the
        gateway row every recorded string resolves to."""
        monkeypatch.setattr(pricing, "PRICES", dict(pricing.PRICES))
        table = tmp_path / "prices.json"
        table.write_text(
            json.dumps(
                {
                    "zai/glm-5.2": {"input": 0.5, "output": 2.0},
                    "glm-5.2": {"input": 0.8, "output": 2.5},
                }
            ),
            encoding="utf-8",
        )
        assert pricing.load_overrides(table) == 2
        assert pricing.price_for_route("zai/glm-5.2").input == 0.5
        assert pricing.price_for("zai/glm-5.2").input == 0.8, (
            "a recorded string still collapses to the bare tier"
        )


