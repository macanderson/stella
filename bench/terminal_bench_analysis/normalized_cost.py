"""One price table for both arms, because self-reported cost is not comparable.

Every agent reports its own spend. Harbor records whatever the agent says in
``agent_result.cost_usd`` and no meter checks it. That is fine within one arm
and worthless across two, because the two arms do not price tokens the same
way.

The 20-task OpenRouter-vs-OpenRouter run made the gap unmissable. Both arms ran
``z-ai/glm-5.2`` through the same endpoint on the same afternoon:

===============  ==========  ==========  ============  =====================
arm              in tokens   out tokens  self-reported  implied $/1M (total)
===============  ==========  ==========  ============  =====================
Stella           58,809,108     933,380       $12.52                   $0.21
Claude Code      18,176,778     428,185       $20.37                   $1.09
===============  ==========  ==========  ============  =====================

Same model, same provider, same day, a 5x difference in effective rate — and
in the direction that flatters the arm whose harness we wrote. The likeliest
reading is that the comparator prices tokens off its built-in *Claude* table
because it is being proxied and cannot know what the tokens actually cost. It
does not matter which arm is wrong: two different accountings cannot be
subtracted, so a "cost per success" built from them is not a measurement.

Token *counts* were never the problem. They are counts, both arms report them
against the same units, and nothing about proxying changes what a token is. So
cost is recomputed here from counts and one explicit table, applied
identically to both arms.

Deliberately refuses to guess. A model with no entry yields ``None`` rather
than a zero or a nearest-neighbour price, because a fabricated cost is worse
than a missing one: a missing cost is visibly missing, and a fabricated cost
gets published.

It refuses to guess about *dates* too. A rate that changed has two entries with
disjoint windows, and a caller that does not say which day a run happened on
gets an exception rather than whichever entry happened to be listed first —
silently pricing an August run at September's rate is the same class of error
as pricing it off the wrong table.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import date
from typing import Any


@dataclass(frozen=True)
class TokenPrice:
    """USD per one million tokens, by role.

    ``cached_input`` is separate because it dominates agent workloads — 91% of
    both arms' input tokens in the run above were cache reads — so folding it
    into ``input`` would misprice the largest single term.
    """

    input_per_m: float
    cached_input_per_m: float
    output_per_m: float


@dataclass(frozen=True)
class DatedPrice:
    """One rate, and the window over which it was the rate.

    Both bounds are INCLUSIVE, and ``None`` means unbounded on that side: an
    entry with ``effective_from=None`` was in force for every run at or before
    ``effective_to``, and ``effective_to=None`` means it is still in force.
    Unbounded-start is not laziness — it is what lets a dated rate be added to
    a model that already has published figures without moving any of them.

    ``source`` is prose rather than a URL, because a URL that 404s two years
    from now is worse than a sentence a reader can act on.
    """

    price: TokenPrice
    effective_from: date | None
    effective_to: date | None
    source: str

    def covers(self, day: date) -> bool:
        after_start = self.effective_from is None or day >= self.effective_from
        before_end = self.effective_to is None or day <= self.effective_to
        return after_start and before_end


def always(price: TokenPrice, *, source: str) -> tuple[DatedPrice, ...]:
    """A model whose rate has never changed, so no date can disambiguate it.

    Callers building a one-off table for a sensitivity check should not have to
    invent bounds for a rate that has only ever had one value.
    """
    return (
        DatedPrice(price=price, effective_from=None, effective_to=None, source=source),
    )


class AmbiguousRunDate(ValueError):
    """Raised when a model has more than one rate and the caller named no date.

    Not a missing-data condition — those return ``None``. This is a caller
    error, and it is raised rather than resolved because every way of resolving
    it (first entry, newest entry, cheapest entry) silently produces a number
    that looks exactly like a correct one.
    """


class OverlappingPriceWindows(ValueError):
    """Raised when two entries for one model both claim the same day."""


# Provenance: rates for the model slugs actually run, recorded here rather than
# fetched so an analysis re-run months later reproduces the same number instead
# of silently repricing history. Update by adding a NEW entry with the date in
# its window when a rate changes; never edit a rate a published figure was
# computed from.
#
# Windows for one slug must tile without gaps or overlaps, so a dated lookup
# always resolves to exactly one entry. `tests/test_normalized_cost.py` asserts
# that as a property over the whole table rather than per row.
PRICE_TABLE: dict[str, tuple[DatedPrice, ...]] = {
    # Unbounded on both sides: this is the rate every published GLM figure was
    # computed from, and narrowing it to a window would move those numbers.
    "z-ai/glm-5.2": always(
        TokenPrice(
            input_per_m=0.60,
            cached_input_per_m=0.11,
            output_per_m=2.20,
        ),
        source="OpenRouter published rates for z-ai/glm-5.2",
    ),
    # Anthropic-native Sonnet 5. Two entries, not one edited entry: the
    # introductory rate is in force through 2026-08-31 and the standard rate
    # from 2026-09-01, so a run's own date decides which applies. Cache reads
    # bill at 0.1x input, matching `stella-model/src/catalog.rs`, which carries
    # the standard $0.30 for this same model.
    "claude-sonnet-5": (
        DatedPrice(
            price=TokenPrice(
                input_per_m=2.00,
                cached_input_per_m=0.20,
                output_per_m=10.00,
            ),
            effective_from=None,
            effective_to=date(2026, 8, 31),
            source="Anthropic introductory pricing, in force through 2026-08-31",
        ),
        DatedPrice(
            price=TokenPrice(
                input_per_m=3.00,
                cached_input_per_m=0.30,
                output_per_m=15.00,
            ),
            effective_from=date(2026, 9, 1),
            effective_to=None,
            source="Anthropic standard list pricing, from 2026-09-01",
        ),
    ),
}


# Every routing prefix Stella can put in front of a model slug, derived rather
# than guessed. Two sources, unioned:
#
#   - `stella-cli/src/config/providers.rs::PROVIDERS` — the ids a `--model
#     provider/slug` spec resolves against (`config.rs` splits on the first
#     `/`), plus `LOCAL_PROVIDER.id`. That is the full set `Dialect` can be
#     reached through: `Dialect` names the wire shape, and several providers
#     share one, so the routing prefix is always the provider id, never the
#     dialect.
#   - `bench/harbor_adapter/stella_harbor/__init__.py::_PROVIDER_CREDENTIAL_ENV`
#     — what the benchmark harness itself accepts as a first segment when it
#     picks a credential and a base URL. It adds `google` as an alias for
#     `gemini`.
#
# `z-ai` is deliberately ABSENT. Stella's provider id is `zai`; `z-ai` is
# OpenRouter's vendor namespace *inside* a model slug, so stripping it would
# destroy the `z-ai/glm-5.2` key rather than normalise it.
ROUTING_PREFIXES: frozenset[str] = frozenset(
    {
        "anthropic",
        "bedrock",
        "deepseek",
        "gemini",
        "google",
        "local",
        "openai",
        "openrouter",
        "vertex",
        "xai",
        "zai",
    }
)


def _slug(model: str) -> str:
    """Strip ONE routing prefix: ``anthropic/claude-sonnet-5`` -> ``claude-sonnet-5``.

    The two arms name the same model differently — Stella carries the provider
    prefix its router needs, the comparator does not — and pricing the same
    tokens differently because of a prefix is exactly the class of error this
    module exists to remove.

    Exactly one prefix, never repeatedly, because the prefix says which
    *adapter* Stella dials and what follows is that provider's own slug — which
    is the thing that has a price. ``openrouter/openrouter/auto`` is
    OpenRouter's meta-router reached through OpenRouter and must land on
    ``openrouter/auto``; looping would strip it to ``auto`` and lose the model.
    The same rule keeps ``openrouter/anthropic/claude-sonnet-5`` off the
    Anthropic-native row, which is correct rather than a gap: a gateway-routed
    run does not cost what a first-party one costs, and quietly charging it
    first-party rates would be this module's own failure mode.
    """
    cleaned = model.strip().lower()
    head, slash, rest = cleaned.partition("/")
    if slash and head in ROUTING_PREFIXES:
        return rest
    return cleaned


def price_for(
    model: str,
    *,
    on: date | None = None,
    table: dict[str, tuple[DatedPrice, ...]] | None = None,
) -> DatedPrice | None:
    """Resolve the rate that was in force for ``model`` on ``on``.

    Returns ``None`` when the model has no entry, and when it has entries but
    none covering ``on`` — a run predating every rate we recorded is unpriced,
    not free.

    Raises ``AmbiguousRunDate`` when ``on`` is omitted for a model whose rate
    has changed. Omitting it stays legal for a model with a single entry,
    because there is nothing there to pick wrong.
    """
    rates = (table if table is not None else PRICE_TABLE).get(_slug(model))
    if not rates:
        return None
    if on is None:
        if len(rates) > 1:
            windows = ", ".join(
                f"{rate.effective_from or '...'}..{rate.effective_to or '...'}"
                for rate in rates
            )
            raise AmbiguousRunDate(
                f"{_slug(model)!r} has {len(rates)} rates ({windows}); pass the "
                f"run's date as `on=` so the right one is chosen rather than "
                f"whichever was listed first"
            )
        return rates[0]
    covering = [rate for rate in rates if rate.covers(on)]
    if len(covering) > 1:
        raise OverlappingPriceWindows(
            f"{_slug(model)!r} has {len(covering)} rates covering {on}; the "
            f"table is wrong, and picking one would publish an arbitrary price"
        )
    return covering[0] if covering else None


def normalized_cost_usd(
    model: str,
    *,
    n_input_tokens: int,
    n_cache_tokens: int,
    n_output_tokens: int,
    on: date | None = None,
    table: dict[str, tuple[DatedPrice, ...]] | None = None,
) -> float | None:
    """Recompute one trial's cost from its token counts.

    ``n_input_tokens`` is treated as INCLUSIVE of ``n_cache_tokens`` — the
    shape Harbor records — so the fresh-input term is the difference. Clamped
    at zero: a cache count exceeding the input count is a broken record, and
    the answer to that is to charge nothing for a negative quantity rather
    than to subtract from the output term.

    ``on`` is the day the trial ran, and decides which rate applies when a
    model's rate has changed. Returns ``None`` when the model has no entry, or
    no entry covering ``on``, which callers must render as "not priced" rather
    than as zero.
    """
    dated = price_for(model, on=on, table=table)
    if dated is None:
        return None
    price = dated.price
    cached = max(0, int(n_cache_tokens))
    fresh = max(0, int(n_input_tokens) - cached)
    out = max(0, int(n_output_tokens))
    return (
        fresh * price.input_per_m
        + cached * price.cached_input_per_m
        + out * price.output_per_m
    ) / 1_000_000


def normalize_trial(
    result: dict[str, Any], model: str, *, on: date | None = None
) -> dict[str, Any]:
    """Add a comparable cost beside the self-reported one, never replacing it.

    Both numbers are kept on purpose. The self-report is what the agent
    believed it spent and is the only figure that can be reconciled against a
    provider invoice for that arm; the normalized figure is the only one that
    can be compared ACROSS arms. Publishing one without the other invites the
    reader to assume they agree.

    ``normalized_cost_rate_source`` rides along so a published figure carries
    the rate it was computed from, rather than leaving a reader to guess which
    entry a re-run would pick.
    """
    agent = result.get("agent_result") or {}
    dated = price_for(model, on=on)
    normalized = normalized_cost_usd(
        model,
        n_input_tokens=agent.get("n_input_tokens") or 0,
        n_cache_tokens=agent.get("n_cache_tokens") or 0,
        n_output_tokens=agent.get("n_output_tokens") or 0,
        on=on,
    )
    return {
        "self_reported_cost_usd": agent.get("cost_usd"),
        "normalized_cost_usd": normalized,
        "normalized_cost_model": _slug(model),
        "normalized_cost_priced": normalized is not None,
        "normalized_cost_rate_source": dated.source if dated is not None else None,
    }
