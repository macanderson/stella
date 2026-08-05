"""The property that matters: identical token counts cost identically."""

from __future__ import annotations

from datetime import date
from itertools import pairwise

import pytest

from normalized_cost import (
    PRICE_TABLE,
    ROUTING_PREFIXES,
    AmbiguousRunDate,
    DatedPrice,
    TokenPrice,
    _slug,
    always,
    normalize_trial,
    normalized_cost_usd,
    price_for,
)

INTRO = date(2026, 8, 15)
STANDARD = date(2026, 9, 15)


def test_the_same_tokens_cost_the_same_whatever_the_arm_called_the_model():
    """The defect, in one assertion.

    Stella names the model with its router prefix and the comparator does not.
    Pricing the same tokens differently because of a prefix is the whole bug.
    """
    counts = dict(
        n_input_tokens=1_000_000, n_cache_tokens=900_000, n_output_tokens=50_000
    )
    stella_side = normalized_cost_usd("openrouter/z-ai/glm-5.2", **counts)
    comparator_side = normalized_cost_usd("z-ai/glm-5.2", **counts)
    assert stella_side == comparator_side
    assert stella_side is not None


def test_the_anthropic_prefix_prices_identically_to_the_bare_slug():
    """The head-to-head this module exists for.

    Stella reports `anthropic/claude-sonnet-5`; Claude Code reports
    `claude-sonnet-5`. Before the fix one arm priced and the other returned
    None, so the comparable-cost column was empty for every Anthropic-native
    run and the only published number was the self-reported one — which
    inverts the ranking.
    """
    counts = dict(
        n_input_tokens=18_176_778,
        n_cache_tokens=16_422_787,
        n_output_tokens=428_185,
    )
    for run_day in (INTRO, STANDARD):
        stella_side = normalized_cost_usd(
            "anthropic/claude-sonnet-5", on=run_day, **counts
        )
        comparator_side = normalized_cost_usd("claude-sonnet-5", on=run_day, **counts)
        assert stella_side == comparator_side, run_day
        assert stella_side is not None, run_day


def test_every_routing_prefix_collapses_to_the_same_slug():
    """`_slug` must cover every prefix the codebase can actually emit, not the
    one that happened to show up first."""
    for prefix in ROUTING_PREFIXES:
        assert _slug(f"{prefix}/claude-sonnet-5") == "claude-sonnet-5", prefix


def test_a_vendor_namespace_is_not_a_routing_prefix():
    """`z-ai` is OpenRouter's vendor namespace, not a Stella provider id.

    Stripping it would destroy the table key rather than normalise it, so the
    prefix list is the provider-id set and nothing wider.
    """
    assert "z-ai" not in ROUTING_PREFIXES
    assert _slug("z-ai/glm-5.2") == "z-ai/glm-5.2"


def test_only_one_prefix_is_stripped():
    """The prefix names which adapter Stella dials; what follows is that
    provider's own slug, and it may legitimately contain a slash."""
    assert _slug("openrouter/openrouter/auto") == "openrouter/auto"
    # A gateway-routed Sonnet does NOT cost what a first-party one costs, so it
    # must not land on the Anthropic-native row.
    assert _slug("openrouter/anthropic/claude-sonnet-5") == "anthropic/claude-sonnet-5"
    assert (
        normalized_cost_usd(
            "openrouter/anthropic/claude-sonnet-5",
            n_input_tokens=1_000_000,
            n_cache_tokens=0,
            n_output_tokens=0,
            on=INTRO,
        )
        is None
    )


def test_the_introductory_rate_applies_before_it_lapses_and_not_after():
    """Two entries, not one edited entry — so a run's own date picks the rate
    instead of whichever was listed first."""
    counts = dict(n_input_tokens=1_000_000, n_cache_tokens=0, n_output_tokens=0)
    assert normalized_cost_usd(
        "claude-sonnet-5", on=date(2026, 8, 31), **counts
    ) == pytest.approx(2.00)
    assert normalized_cost_usd(
        "claude-sonnet-5", on=date(2026, 9, 1), **counts
    ) == pytest.approx(3.00)

    out = dict(n_input_tokens=0, n_cache_tokens=0, n_output_tokens=1_000_000)
    assert normalized_cost_usd(
        "claude-sonnet-5", on=date(2026, 8, 31), **out
    ) == pytest.approx(10.00)
    assert normalized_cost_usd(
        "claude-sonnet-5", on=date(2026, 9, 1), **out
    ) == pytest.approx(15.00)


def test_cache_reads_bill_at_a_tenth_of_input_on_both_sonnet_rates():
    cached = dict(n_input_tokens=1_000_000, n_cache_tokens=1_000_000, n_output_tokens=0)
    assert normalized_cost_usd("claude-sonnet-5", on=INTRO, **cached) == pytest.approx(
        0.20
    )
    assert normalized_cost_usd(
        "claude-sonnet-5", on=STANDARD, **cached
    ) == pytest.approx(0.30)


def test_a_changed_rate_without_a_date_raises_rather_than_guessing():
    """Silently returning whichever entry was listed first is the same class of
    error as pricing off the wrong table — so it is refused, loudly."""
    with pytest.raises(AmbiguousRunDate):
        normalized_cost_usd(
            "claude-sonnet-5",
            n_input_tokens=1_000_000,
            n_cache_tokens=0,
            n_output_tokens=0,
        )


def test_an_unchanged_rate_needs_no_date():
    """A model with a single entry has nothing to pick wrong, so requiring a
    date there would only break existing callers for no safety gain."""
    assert normalized_cost_usd(
        "z-ai/glm-5.2",
        n_input_tokens=1_000_000,
        n_cache_tokens=0,
        n_output_tokens=0,
    ) == pytest.approx(0.76)


def test_the_studys_pinned_worker_is_priced():
    """The confirmatory run is pinned to `openrouter/z-ai/glm-5.1`. A table
    with no row for it leaves every trial of the actual study unpriced, which
    is indistinguishable from the table not being applied at all."""
    assert normalized_cost_usd(
        "openrouter/z-ai/glm-5.1",
        n_input_tokens=1_000_000,
        n_cache_tokens=0,
        n_output_tokens=0,
    ) == pytest.approx(0.966)


def test_the_gateway_route_is_priced_apart_from_the_first_party_one():
    """`z-ai/glm-5.2` is the OpenRouter route and `zai/glm-5.2` is z.ai
    direct. They bill differently, so one row for both would misprice
    whichever arm took the other route."""
    counts = dict(n_input_tokens=1_000_000, n_cache_tokens=0, n_output_tokens=0)
    gateway = normalized_cost_usd("openrouter/z-ai/glm-5.2", **counts)
    first_party = normalized_cost_usd("zai/glm-5.2", **counts)
    assert gateway == pytest.approx(0.76)
    assert first_party == pytest.approx(0.60)


def test_price_windows_tile_without_gaps_or_overlaps():
    """A dated lookup must always resolve to exactly one entry. Asserted as a
    property over the whole table so a future rate change cannot open a hole
    that only shows up as a missing column months later."""
    for slug, rates in PRICE_TABLE.items():
        assert rates, slug
        ordered = sorted(rates, key=lambda r: r.effective_from or date.min)
        assert ordered[0].effective_from is None, (
            f"{slug}: the earliest rate must be open-started, or runs before it "
            f"silently become unpriced"
        )
        assert ordered[-1].effective_to is None, (
            f"{slug}: the latest rate must be open-ended, or today's runs "
            f"silently become unpriced"
        )
        for earlier, later in pairwise(ordered):
            assert earlier.effective_to is not None, slug
            assert later.effective_from is not None, slug
            assert later.effective_from == earlier.effective_to + _ONE_DAY, (
                f"{slug}: {earlier.effective_to} -> {later.effective_from} "
                f"leaves a gap or an overlap"
            )


def test_every_entry_carries_its_provenance():
    """A published figure has to be traceable to the rate it came from."""
    for slug, rates in PRICE_TABLE.items():
        for rate in rates:
            assert rate.source.strip(), slug


def test_an_unpriced_model_is_none_not_zero():
    """A fabricated cost is worse than a missing one: the missing one is
    visibly missing, the fabricated one gets published."""
    assert (
        normalized_cost_usd(
            "some/unlisted-model",
            n_input_tokens=10_000,
            n_cache_tokens=0,
            n_output_tokens=10_000,
        )
        is None
    )


def test_a_cache_count_exceeding_the_input_count_never_goes_negative():
    """A broken record should charge nothing for a negative quantity rather
    than subtract from the output term."""
    cost = normalized_cost_usd(
        "z-ai/glm-5.2",
        n_input_tokens=100,
        n_cache_tokens=5_000,
        n_output_tokens=1_000_000,
    )
    assert cost == pytest.approx(2.42 + 5_000 * 0.14 / 1_000_000)


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
    assert out["normalized_cost_rate_source"]
    # The comparator's self-report is several times its normalized cost; the
    # point of the pair is that a reader can see that for themselves.
    assert out["normalized_cost_usd"] < out["self_reported_cost_usd"]


def test_normalize_trial_agrees_across_arms_on_the_same_day():
    trial = {
        "agent_result": {
            "cost_usd": 2.65,
            "n_input_tokens": 5_000_000,
            "n_cache_tokens": 4_500_000,
            "n_output_tokens": 120_000,
        }
    }
    stella = normalize_trial(trial, "anthropic/claude-sonnet-5", on=INTRO)
    comparator = normalize_trial(trial, "claude-sonnet-5", on=INTRO)
    assert stella["normalized_cost_usd"] == comparator["normalized_cost_usd"]
    assert stella["normalized_cost_model"] == comparator["normalized_cost_model"]
    assert (
        stella["normalized_cost_rate_source"]
        == (comparator["normalized_cost_rate_source"])
    )


def test_a_custom_table_overrides_without_mutating_the_shipped_one():
    custom = {"z-ai/glm-5.2": always(TokenPrice(1.0, 1.0, 1.0), source="test fixture")}
    assert normalized_cost_usd(
        "z-ai/glm-5.2",
        n_input_tokens=1_000_000,
        n_cache_tokens=0,
        n_output_tokens=0,
        table=custom,
    ) == pytest.approx(1.0)
    assert PRICE_TABLE["z-ai/glm-5.2"][0].price.input_per_m == pytest.approx(0.76)


def test_a_run_before_every_recorded_rate_is_unpriced_not_free():
    custom = {
        "made-up": (
            DatedPrice(
                price=TokenPrice(1.0, 1.0, 1.0),
                effective_from=date(2026, 1, 1),
                effective_to=None,
                source="test fixture",
            ),
        )
    }
    assert price_for("made-up", on=date(2025, 12, 31), table=custom) is None
    assert price_for("made-up", on=date(2026, 1, 1), table=custom) is not None


_ONE_DAY = date(2026, 1, 2) - date(2026, 1, 1)
