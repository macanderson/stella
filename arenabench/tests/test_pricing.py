# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The one price table both contestants are costed from.

Every agent prices its own run from its own table and no meter checks it, so a
difference between two self-reports is a fact about the tables. `pricing.py`
answers the question those numbers cannot: the same token counts, the same
rates, both seats.
"""

from __future__ import annotations

import pytest

from arenabench.pricing import price_for, trial_cost


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

    def test_one_id_resolves_to_one_rate_however_it_was_routed(self):
        """A routed seat is launched with the bare model id and an unrouted one
        keeps its `provider/` prefix, so route-keyed prices would cost the two
        seats of a single match from two different rows — the exact
        subtracting-two-tables error this module exists to prevent."""
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


