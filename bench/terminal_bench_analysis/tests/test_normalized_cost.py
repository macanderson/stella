"""The property that matters: identical token counts cost identically."""

from __future__ import annotations

import pytest

from normalized_cost import (
    PRICE_TABLE,
    TokenPrice,
    normalize_trial,
    normalized_cost_usd,
)


def test_the_same_tokens_cost_the_same_whatever_the_arm_called_the_model():
    """The defect, in one assertion.

    Stella names the model with its router prefix and the comparator does not.
    Pricing the same tokens differently because of a prefix is the whole bug.
    """
    counts = dict(n_input_tokens=1_000_000, n_cache_tokens=900_000, n_output_tokens=50_000)
    stella_side = normalized_cost_usd("openrouter/z-ai/glm-5.2", **counts)
    comparator_side = normalized_cost_usd("z-ai/glm-5.2", **counts)
    assert stella_side == comparator_side
    assert stella_side is not None


def test_cache_reads_are_priced_as_cache_reads():
    """91% of both arms' input was cache reads, so folding them into the
    fresh-input rate would misprice the largest single term."""
    all_fresh = normalized_cost_usd(
        "z-ai/glm-5.2", n_input_tokens=1_000_000, n_cache_tokens=0, n_output_tokens=0
    )
    all_cached = normalized_cost_usd(
        "z-ai/glm-5.2",
        n_input_tokens=1_000_000,
        n_cache_tokens=1_000_000,
        n_output_tokens=0,
    )
    assert all_fresh == pytest.approx(0.60)
    assert all_cached == pytest.approx(0.11)
    assert all_cached < all_fresh


def test_an_unpriced_model_is_none_not_zero():
    """A fabricated cost is worse than a missing one: the missing one is
    visibly missing, the fabricated one gets published."""
    assert normalized_cost_usd(
        "some/unlisted-model",
        n_input_tokens=10_000,
        n_cache_tokens=0,
        n_output_tokens=10_000,
    ) is None


def test_a_cache_count_exceeding_the_input_count_never_goes_negative():
    """A broken record should charge nothing for a negative quantity rather
    than subtract from the output term."""
    cost = normalized_cost_usd(
        "z-ai/glm-5.2",
        n_input_tokens=100,
        n_cache_tokens=5_000,
        n_output_tokens=1_000_000,
    )
    assert cost == pytest.approx(2.20 + 5_000 * 0.11 / 1_000_000)


def test_the_self_report_is_kept_beside_the_normalized_figure():
    """Publishing one without the other invites the reader to assume they
    agree — and in the measured run they disagreed by 5x."""
    trial = {
        "agent_result": {
            "cost_usd": 20.37,
            "n_input_tokens": 18_176_778,
            "n_cache_tokens": 16_422_787,
            "n_output_tokens": 428_185,
        }
    }
    out = normalize_trial(trial, "z-ai/glm-5.2")
    assert out["self_reported_cost_usd"] == 20.37
    assert out["normalized_cost_priced"] is True
    # The comparator's self-report is several times its normalized cost; the
    # point of the pair is that a reader can see that for themselves.
    assert out["normalized_cost_usd"] < out["self_reported_cost_usd"]


def test_a_custom_table_overrides_without_mutating_the_shipped_one():
    custom = {"z-ai/glm-5.2": TokenPrice(1.0, 1.0, 1.0)}
    assert normalized_cost_usd(
        "z-ai/glm-5.2",
        n_input_tokens=1_000_000,
        n_cache_tokens=0,
        n_output_tokens=0,
        table=custom,
    ) == pytest.approx(1.0)
    assert PRICE_TABLE["z-ai/glm-5.2"].input_per_m == pytest.approx(0.60)
