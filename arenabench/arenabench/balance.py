# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The OpenRouter balance preflight for cloud submissions.

Three cloud runs were lost whole to an empty OpenRouter balance — h2h891
(16 trials), fivetools5 (30 of 30), gate89high1 (35 of 89): the submission
path happily fanned out one Batch job per trial while every model call
inside them was going to answer HTTP 402, and each container burned real
compute to score a ``0.0`` indistinguishable from an agent loss. The same
class of waste the SSM credential refusal already stops at submit time
(#1827), one layer up: the key existed, the account behind it was empty.

So, before anything is uploaded or submitted, ``cloud run`` asks OpenRouter
what the key can still spend (``GET /api/v1/credits``) and compares it with
a projection of the run's cost — trial count times a per-trial estimate the
operator can tune with ``--est-per-trial``. The comparison is deliberately
crude: a projection does not need to be right to catch the failure this
exists for, which is an 89-trial submission against a balance near zero.

**The check refuses only what it positively knows.** An insufficient
*known* balance refuses the submission (``--skip-balance-check`` overrides).
A preflight that cannot learn the balance — no ``OPENROUTER_API_KEY`` in
the operator's environment, a network failure, an unexpected response
shape — proceeds with a printed warning, because a broken preflight must
never block a funded run.

Same split as :mod:`.cloud`: decisions are pure functions over plain data;
the one HTTP call is a stdlib ``urllib`` adapter, injected through
:class:`.cloud.CloudExecutor` the same way ``ls_remote`` is, so the tests
neither read the environment nor reach the network.
"""

from __future__ import annotations

import json
import urllib.request
from collections.abc import Callable, Mapping
from typing import Any

__all__ = [
    "CREDITS_URL",
    "BalanceUnknownError",
    "fetch_openrouter_credits",
    "preflight",
    "projected_spend",
    "remaining_credits",
    "shortfall_message",
]

#: The one endpoint consulted. Returns the key's lifetime purchases and
#: spend; the difference is what is left to fund a run. Response shape
#: verified against the live API on 2026-08-14:
#: ``{"data":{"total_credits":1650,"total_usage":1606.027034237}}``.
CREDITS_URL = "https://openrouter.ai/api/v1/credits"

#: One bounded read: a preflight that can hang is a preflight that blocks
#: the funded runs it exists to protect.
_TIMEOUT = 10.0


class BalanceUnknownError(RuntimeError):
    """The balance could not be learned; the message says why.

    Deliberately its own type rather than :class:`.cloud.CloudError`: every
    raiser here names a fail-open condition — the caller prints the reason
    and proceeds — and sharing the submission path's error type would invite
    a genuine submission failure being waved through with it.
    """


# --------------------------------------------------------------------------
# pure decisions
# --------------------------------------------------------------------------


def remaining_credits(payload: Any) -> float:
    """``total_credits - total_usage`` from the ``/api/v1/credits`` body.

    Any surprise in the shape raises :class:`BalanceUnknownError` — never a
    guessed ``0.0``, which would refuse a funded run on a parsing bug, the
    exact inversion of this module's fail-open contract.
    """
    data = payload.get("data") if isinstance(payload, Mapping) else None
    if not isinstance(data, Mapping):
        raise BalanceUnknownError(
            "credits response carries no data object: " + repr(payload)[:200]
        )
    try:
        total = float(data["total_credits"])
        used = float(data["total_usage"])
    except (KeyError, TypeError, ValueError) as exc:
        raise BalanceUnknownError(
            f"credits response is not the documented shape: {exc!r}"
        ) from exc
    return total - used


def projected_spend(trials: int, est_per_trial: float) -> float:
    """What the run is projected to cost, in the credits' own USD unit."""
    return trials * est_per_trial


def shortfall_message(remaining: float, trials: int, est_per_trial: float) -> str | None:
    """The refusal, or ``None`` when the balance covers the projection.

    One message carrying all three facts an operator needs: the balance
    that was found, the projection it fell short of (with the estimate knob
    that shapes it), and the flag that overrides the refusal.
    """
    projected = projected_spend(trials, est_per_trial)
    if remaining >= projected:
        return None
    return (
        f"OpenRouter balance ${remaining:.2f} cannot cover the projected spend of "
        f"${projected:.2f} ({trials} trial(s) x ${est_per_trial:.2f}/trial — tune with "
        "--est-per-trial). Add credit at https://openrouter.ai/settings/credits, or "
        "pass --skip-balance-check to submit anyway."
    )


# --------------------------------------------------------------------------
# the HTTP adapter
# --------------------------------------------------------------------------


def fetch_openrouter_credits(
    api_key: str, *, timeout: float = _TIMEOUT
) -> Mapping[str, Any]:
    """GET :data:`CREDITS_URL`; any failure raises :class:`BalanceUnknownError`.

    The messages name the endpoint and the failure, never the key. HTTP
    errors (a 401 from a malformed key included) are fail-open like
    transport errors: a preflight that cannot authenticate has learned
    nothing about the balance, and refusing on ignorance is the inversion
    this module forbids.
    """
    request = urllib.request.Request(
        CREDITS_URL, headers={"Authorization": f"Bearer {api_key}"}
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            body = response.read().decode("utf-8")
    except OSError as exc:  # URLError, HTTPError and timeouts all subclass it
        raise BalanceUnknownError(f"GET {CREDITS_URL} failed: {exc}") from exc
    try:
        payload = json.loads(body)
    except ValueError as exc:
        raise BalanceUnknownError(f"{CREDITS_URL} returned non-JSON: {exc}") from exc
    if not isinstance(payload, Mapping):
        raise BalanceUnknownError(f"{CREDITS_URL} returned a non-object body")
    return payload


# --------------------------------------------------------------------------
# the verb-side orchestration
# --------------------------------------------------------------------------


def preflight(
    *,
    trials: int,
    est_per_trial: float,
    api_key: str | None,
    fetch: Callable[[str], Mapping[str, Any]] = fetch_openrouter_credits,
    out: Callable[[str], None] = print,
) -> str | None:
    """Run the balance preflight; the refusal message, or ``None`` to proceed.

    Fails open and says so: a missing key, a fetch failure, or a response
    shape surprise each print one "skipped" line through ``out`` and return
    ``None``. Only a balance this function positively learned can refuse.
    """
    if not api_key:
        out("balance   : check skipped — OPENROUTER_API_KEY is not set in this shell")
        return None
    try:
        remaining = remaining_credits(fetch(api_key))
    except Exception as exc:  # a broken preflight — network, HTTP, shape, or
        # its own bug — must never block a funded run; the printed reason is
        # what keeps the failure visible instead of silent.
        out(
            f"balance   : check skipped — {exc} (proceeding: a broken preflight "
            "must not block a funded run)"
        )
        return None
    refusal = shortfall_message(remaining, trials, est_per_trial)
    if refusal is None:
        out(
            f"balance   : ${remaining:.2f} available covers "
            f"${projected_spend(trials, est_per_trial):.2f} projected "
            f"({trials} trial(s) x ${est_per_trial:.2f})"
        )
    return refusal
