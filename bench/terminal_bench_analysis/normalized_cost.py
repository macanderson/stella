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
"""

from __future__ import annotations

from dataclasses import dataclass
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


# Provenance: OpenRouter's published rates for the model slugs actually run,
# recorded here rather than fetched so an analysis re-run months later
# reproduces the same number instead of silently repricing history. Update by
# adding a NEW entry with the date in its key when a rate changes; never edit
# a rate a published figure was computed from.
PRICE_TABLE: dict[str, TokenPrice] = {
    "z-ai/glm-5.2": TokenPrice(
        input_per_m=0.60,
        cached_input_per_m=0.11,
        output_per_m=2.20,
    ),
}


def _slug(model: str) -> str:
    """Strip a routing prefix: ``openrouter/z-ai/glm-5.2`` -> ``z-ai/glm-5.2``.

    The two arms name the same model differently — Stella carries the provider
    prefix its router needs, the comparator does not — and pricing the same
    tokens differently because of a prefix is exactly the class of error this
    module exists to remove.
    """
    cleaned = model.strip().lower()
    if cleaned.startswith("openrouter/"):
        cleaned = cleaned[len("openrouter/") :]
    return cleaned


def normalized_cost_usd(
    model: str,
    *,
    n_input_tokens: int,
    n_cache_tokens: int,
    n_output_tokens: int,
    table: dict[str, TokenPrice] | None = None,
) -> float | None:
    """Recompute one trial's cost from its token counts.

    ``n_input_tokens`` is treated as INCLUSIVE of ``n_cache_tokens`` — the
    shape Harbor records — so the fresh-input term is the difference. Clamped
    at zero: a cache count exceeding the input count is a broken record, and
    the answer to that is to charge nothing for a negative quantity rather
    than to subtract from the output term.

    Returns ``None`` when the model has no entry, which callers must render as
    "not priced" rather than as zero.
    """
    price = (table or PRICE_TABLE).get(_slug(model))
    if price is None:
        return None
    cached = max(0, int(n_cache_tokens))
    fresh = max(0, int(n_input_tokens) - cached)
    out = max(0, int(n_output_tokens))
    return (
        fresh * price.input_per_m
        + cached * price.cached_input_per_m
        + out * price.output_per_m
    ) / 1_000_000


def normalize_trial(result: dict[str, Any], model: str) -> dict[str, Any]:
    """Add a comparable cost beside the self-reported one, never replacing it.

    Both numbers are kept on purpose. The self-report is what the agent
    believed it spent and is the only figure that can be reconciled against a
    provider invoice for that arm; the normalized figure is the only one that
    can be compared ACROSS arms. Publishing one without the other invites the
    reader to assume they agree.
    """
    agent = result.get("agent_result") or {}
    normalized = normalized_cost_usd(
        model,
        n_input_tokens=agent.get("n_input_tokens") or 0,
        n_cache_tokens=agent.get("n_cache_tokens") or 0,
        n_output_tokens=agent.get("n_output_tokens") or 0,
    )
    return {
        "self_reported_cost_usd": agent.get("cost_usd"),
        "normalized_cost_usd": normalized,
        "normalized_cost_model": _slug(model),
        "normalized_cost_priced": normalized is not None,
    }
