# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""What a trial actually cost, computed the same way for every contestant.

**Why not just use what the agents report.** Each agent prices its own run from
its own table, and the tables disagree. Measured on one identical task: Claude
Code reported ``$0.3326`` and Stella ``$0.0641``. Stella's catalog knows the
model, so its figure was a fair estimate; Claude Code's does not, so it priced
GLM as the Anthropic model whose alias it had been pointed at. Read side by
side that says "5x cheaper", which is not a fact about either agent — it is two
pricing tables being subtracted from each other.

So ArenaBench does not subtract self-reports. It takes the *token counts*,
which both agents report accurately, and applies **one** table to both. That is
the only way a cost column answers the question an operator is actually asking:
same work, same prices, who spent less.

**A subscription does not change the arithmetic.** On a coding plan the
marginal spend is zero and no per-token comparison exists; what this computes
is list-price equivalent — what the same tokens would have cost on metered
billing. That is a real and useful number ("this run would bill $X"), and it is
labelled as such rather than presented as an invoice.

**An unpriced model gets no number, not a guessed one.** That is the whole
lesson of the paragraph above: a plausible wrong figure populates a column,
sorts against the other arm, and crowns a winner on a dimension nobody
measured. Missing is strictly better than wrong here.

**A recorded model id cannot say which route served it.** The same model is
launched under different spellings depending on how the seat is routed
(``agents.launch_model`` hands a directly-routed seat the bare id and every
other seat the qualified one), so route attribution comes only from the launch
record the runner writes beside each job — :func:`price_for_route` — and every
id read back from a trajectory or event stream collapses to one bare id and
one gateway rate.
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any

__all__ = [
    "PRICES",
    "Price",
    "load_overrides",
    "price_for",
    "price_for_route",
    "price_on_route",
    "trial_cost",
]


@dataclass(frozen=True)
class Price:
    """USD per **million** tokens, as providers publish them."""

    input: float
    output: float
    #: Reading a cache hit. ``None`` means the provider does not bill it
    #: separately, and cached reads are charged at the input rate.
    cache_read: float | None = None
    #: Writing a cache entry. ``None`` means not separately billed.
    cache_write: float | None = None

    def to_json(self) -> dict[str, Any]:
        return {
            "input": self.input,
            "output": self.output,
            "cache_read": self.cache_read,
            "cache_write": self.cache_write,
        }


#: Published list prices, USD per million tokens, keyed two ways.
#:
#: **Bare ids carry the gateway rate.** ``openrouter/z-ai/glm-5.2``,
#: ``z-ai/glm-5.2`` and ``glm-5.2`` all resolve to the one ``glm-5.2`` entry,
#: priced as OpenRouter publishes it (verified against its ``/models`` on
#: 2026-08-04; that endpoint is the source of truth for the bare tier and
#: re-checking it is the way to refresh it). The collapse is deliberate: a
#: model id read back from a trial's artifacts cannot say which route served
#: it, because a routed seat is launched with the bare model id and an
#: unrouted one keeps its ``provider/`` prefix — inferring the route from the
#: spelling would cost Claude Code at z.ai's rate and Stella at OpenRouter's
#: in a match where both call the identical endpoint. Comparability is the
#: whole product here, so an id of unknown route means one rate, and it is the
#: higher published rate rather than a number that flatters whichever seat
#: happened to be routed.
#:
#: **Route-qualified ids carry a provider's own first-party rate** where it
#: differs from the gateway's — GLM-5.2 is 0.60 in / 2.20 out / 0.11 cached
#: direct from z.ai against 0.76 / 2.42 / 0.14 through OpenRouter. These rows
#: are reachable only through :func:`price_for_route`, with the route the
#: *seat recorded at launch* (the runner's ``<job>.seat.json``) — never by
#: parsing a recorded model string, for the reason above. A seat that recorded
#: no route prices at the bare tier, which errs against the seat rather than
#: for it (#1498).
#:
#: Deliberately small. This is not a price oracle — it covers the models people
#: actually seat here, and anything else reports no cost rather than a guess.
PRICES: dict[str, Price] = {
    "glm-5.2": Price(input=0.76, output=2.42, cache_read=0.14),
    # z.ai's own metered rate for the same model, as its pricing page and the
    # Stella seed catalog (`crates/stella-model/src/catalog.rs`, the `zai`
    # row) both carry it. The only family member with a distinct first-party
    # rate: glm-5.1 and glm-4.5-air publish the same numbers on both routes.
    "zai/glm-5.2": Price(input=0.60, output=2.20, cache_read=0.11),
    "glm-5.1": Price(input=0.966, output=3.036, cache_read=0.1794),
    "glm-5": Price(input=0.95, output=2.55, cache_read=0.20),
    "glm-5-turbo": Price(input=1.20, output=4.00, cache_read=0.24),
    # The committed GLM match's triage role. Seated in every pipeline run of
    # that match, and a trial's cost is the sum of its role subtotals (#1566),
    # so leaving this row out would blank the whole trial's cost rather than
    # merely one role's share. Same numbers on both routes, per the catalog.
    "glm-4.5-air": Price(input=0.13, output=0.85, cache_read=0.025),
    "claude-fable-5": Price(
        input=10.0, output=50.0, cache_read=1.0, cache_write=12.50
    ),
    "claude-opus-4.5": Price(
        input=5.0, output=25.0, cache_read=0.5, cache_write=6.25
    ),
    "claude-sonnet-4.5": Price(
        input=3.0, output=15.0, cache_read=0.3, cache_write=3.75
    ),
}

#: Routing prefixes that are ArenaBench's or Harbor's, never the provider's.
_ROUTE_PREFIXES = ("openrouter/", "anthropic/", "zai/", "z-ai/", "openai/", "google/")


def _normalise(model: str) -> str:
    """Strip routing prefixes down to the id the provider publishes."""
    name = model.strip().lower()
    changed = True
    while changed:
        changed = False
        for prefix in _ROUTE_PREFIXES:
            if name.startswith(prefix):
                name = name[len(prefix) :]
                changed = True
    return name


def load_overrides(path: str | Path | None = None) -> int:
    """Merge an operator's price file into :data:`PRICES`.

    Published prices move, and a benchmark that can only be re-priced by
    editing its source is one that quietly goes stale. The file is a flat
    ``{"model": {"input": 1.0, "output": 2.0, ...}}`` in USD per million
    tokens. Returns how many entries were applied.

    A key lands on the tier it names, case aside: a bare id (``glm-5.2``)
    overrides the gateway rate every recorded string falls to, and a
    route-qualified id (``zai/glm-5.2``) overrides that route alone. No prefix
    is stripped — with two tiers in one table, rewriting the operator's key
    would silently move the override onto the wrong row.
    """
    target = Path(path) if path else Path(
        os.environ.get("ARENABENCH_PRICES", "") or Path.home() / ".arenabench" / "prices.json"
    )
    try:
        raw = json.loads(Path(target).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return 0
    if not isinstance(raw, dict):
        return 0
    applied = 0
    for model, entry in raw.items():
        if not isinstance(entry, dict):
            continue
        try:
            PRICES[str(model).strip().lower()] = Price(
                input=float(entry["input"]),
                output=float(entry["output"]),
                cache_read=(
                    float(entry["cache_read"]) if entry.get("cache_read") is not None else None
                ),
                cache_write=(
                    float(entry["cache_write"])
                    if entry.get("cache_write") is not None
                    else None
                ),
            )
            applied += 1
        except (KeyError, TypeError, ValueError):
            continue
    return applied


def price_for(model: str) -> Price | None:
    """The published price for a recorded model id, or ``None`` if unknown.

    Route-blind on purpose: the id collapses to its bare form and prices at
    the gateway tier, however it was spelled. This is the right lookup for
    every model string read back from a trial's artifacts — see the module
    docstring for why those strings cannot carry route.
    """
    if not model:
        return None
    return PRICES.get(_normalise(model))


def price_for_route(qualified_model: str) -> Price | None:
    """The price for a model on the route a seat *recorded at launch*.

    ``qualified_model`` is the seat's own ``api/model`` string from the launch
    record the runner writes beside each job — the one place the route is a
    fact rather than an inference. An exact row for that route wins; a route
    with no row of its own prices like any recorded id, at the gateway tier.

    Never call this with a model string read back from a trajectory or event
    stream: its spelling depends on how the seat was launched, not on which
    endpoint served it, and pricing on it would cost the two seats of one
    match from two different rows.
    """
    if not qualified_model:
        return None
    exact = PRICES.get(qualified_model.strip().lower())
    return exact if exact is not None else price_for(qualified_model)


def price_on_route(route_api: str, model: str) -> Price | None:
    """The price for any model logged by a seat whose recorded api is known.

    A pipeline seat's stream carries several models — worker, verifier,
    triage — and every one of them rode the seat's provider: a role config
    has no ``api`` or ``base_url`` of its own (`model.RoleConfig`), so the
    seat's launch record speaks for the whole stream. The model id is
    collapsed to its bare form first — its spelling varies with how the seat
    was launched, which is the #1498 trap — and the *recorded* api then
    requalifies it. A route row for that pair wins; otherwise the bare
    gateway tier; otherwise ``None``.

    ``route_api`` must come from the seat's launch record. With no record
    there is no route fact, and the caller should use :func:`price_for`.
    """
    bare = _normalise(model)
    if not bare:
        return None
    exact = PRICES.get(f"{route_api.strip().lower()}/{bare}")
    return exact if exact is not None else PRICES.get(bare)


def trial_cost(
    price: Price | None,
    *,
    tokens_in: int,
    tokens_out: int,
    cache_read: int = 0,
    cache_write: int = 0,
) -> float | None:
    """List-price cost of one trial, or ``None`` when the model is unpriced.

    ``tokens_in`` is treated as the **total** prompt tokens, cached ones
    included — which is what both Harbor's ATIF reduction and Stella's event
    stream report. The cached portions are therefore subtracted out and billed
    at their own rates, or the same tokens would be charged twice, at the most
    expensive rate, against whichever agent caches best. Penalising the better
    cache is precisely backwards.
    """
    if price is None:
        return None
    cached = max(0, int(cache_read)) + max(0, int(cache_write))
    uncached_in = max(0, int(tokens_in) - cached)

    read_rate = price.cache_read if price.cache_read is not None else price.input
    write_rate = price.cache_write if price.cache_write is not None else price.input

    total = (
        uncached_in * price.input
        + max(0, int(cache_read)) * read_rate
        + max(0, int(cache_write)) * write_rate
        + max(0, int(tokens_out)) * price.output
    )
    return total / 1_000_000.0
