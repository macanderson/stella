# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Gateway balance preflight: refuse a run the wallet cannot pay for.

Credit exhaustion is the most expensive defect this harness has recorded.
Three runs were killed or maimed by it — one partial, one total loss, one
losing 35 of 89 trials — and in every case the money ran out *after*
submission, which is the worst possible moment: the containers are already
dispatched, the trials that die score as operational aborts, and the run's
solve rate is quietly computed over a denominator that includes them.

None of that is detectable from inside a trial. It is trivially detectable
before one: ask the gateway what is left, multiply the trial count by what a
trial costs, and refuse when the second number is larger.

The split here is the usual one. :func:`verdict` is pure arithmetic over
three floats and is where every decision is made and tested;
:func:`fetch_openrouter_balance` is the one function that touches the
network, returns ``None`` on every failure, and decides nothing. A preflight
that cannot reach the gateway must not block a run — it reports that it
could not tell, exactly as the phantom-task guard does with an unreadable
corpus (#3255).
"""

from __future__ import annotations

import json
import os
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Literal

__all__ = [
    "Balance",
    "Verdict",
    "Wallet",
    "fetch_openrouter_balance",
    "openrouter_key",
    "verdict",
]

#: Environment variables an OpenRouter key may arrive in, in preference order.
KEY_ENV_NAMES = ("OPENROUTER_API_KEY", "OPENROUTER_KEY")

#: How much headroom over the projected spend counts as comfortable. A run
#: priced at exactly its balance is a run that ends in a 402 partway through,
#: because the estimate is an average and tasks are not: the expensive tail
#: of a 89-task panel spends multiples of the mean. Below this multiple the
#: preflight warns; below 1.0 it refuses.
COMFORTABLE_HEADROOM = 2.0


@dataclass(frozen=True)
class Balance:
    """What a gateway says is left to spend, and where the figure came from."""

    remaining_usd: float
    #: Human-readable provenance, printed with the verdict so an operator can
    #: see which endpoint answered rather than trusting a bare number.
    source: str


@dataclass(frozen=True)
class Wallet:
    """Which wallet a reading priced — by name, never by value.

    A cloud run prices the submitting host's own key and then spends a key
    the seats read from SSM. Those are two accounts that merely tend to be
    the same one, and when they are not, a clean verdict over a funded local
    wallet is byte-indistinguishable from a clean verdict over the wallet
    that will actually pay — which is the exact reading that returned three
    runs to the empty-balance loss this preflight exists to stop (#3308).
    So a priced verdict says whose money it counted.
    """

    #: The environment variable the priced key was read from on this host.
    env_name: str
    #: The credential name the trial seats will spend instead, when the run
    #: knows one. ``None`` when nothing is declared — the local key is then
    #: all there is to name, and naming it is still the whole point.
    seat_credential: str | None = None

    def provenance(self) -> str:
        """The parenthetical an operator reads to place the number."""
        priced = f"priced against {self.env_name} on this host"
        if self.seat_credential is None:
            return priced
        return f"{priced}; seats spend the SSM-provided {self.seat_credential}"


@dataclass(frozen=True)
class Verdict:
    """The preflight's answer: whether to submit, and what to tell the human."""

    level: Literal["ok", "warn", "refuse", "unknown"]
    message: str

    @property
    def blocks(self) -> bool:
        """Whether this verdict must stop the submission."""
        return self.level == "refuse"


def verdict(
    remaining_usd: float | None,
    trials: int,
    cost_per_trial_usd: float | None,
    wallet: Wallet | None = None,
) -> Verdict:
    """Whether a wallet holding ``remaining_usd`` should fund this run.

    ``cost_per_trial_usd`` is the operator's own estimate — there is no
    honest default, because a trial's cost is a fact about the panel, the
    model and the arm, and a number invented here would be quoted later as
    if it had been measured. Without it the preflight still refuses an
    empty wallet, which is the case that needs no estimate at all.

    ``remaining_usd`` of ``None`` means the balance could not be read, which
    is reported and never blocks: an unreachable gateway must not be able to
    stop a run that would have succeeded.

    ``wallet`` names whose money was counted and is appended to every arm
    that priced one. The unknown arm deliberately carries none: no wallet
    was read, so there is nothing to attribute.
    """
    if remaining_usd is None:
        return Verdict("unknown", "balance not checked — the gateway did not answer")
    if remaining_usd <= 0:
        return _priced(
            "refuse",
            wallet,
            f"the gateway balance is ${remaining_usd:.2f} — every trial would 402 "
            "on its first model call and score as an operational abort",
        )
    if cost_per_trial_usd is None or cost_per_trial_usd <= 0:
        return _priced(
            "ok",
            wallet,
            f"gateway balance ${remaining_usd:.2f}; projected spend unknown "
            "(pass --est-cost-per-trial to have it checked)",
        )
    projected = trials * cost_per_trial_usd
    if remaining_usd < projected:
        return _priced(
            "refuse",
            wallet,
            f"the gateway balance is ${remaining_usd:.2f} but this run projects "
            f"${projected:.2f} ({trials} trials x ${cost_per_trial_usd:.2f}) — the "
            "money would run out partway through and the trials it kills would "
            "score as operational aborts",
        )
    if remaining_usd < projected * COMFORTABLE_HEADROOM:
        return _priced(
            "warn",
            wallet,
            f"gateway balance ${remaining_usd:.2f} against a projected "
            f"${projected:.2f} — under {COMFORTABLE_HEADROOM:g}x headroom, and a "
            "panel's expensive tail spends multiples of the mean",
        )
    return _priced(
        "ok",
        wallet,
        f"gateway balance ${remaining_usd:.2f} against a projected ${projected:.2f}",
    )


def _priced(
    level: Literal["ok", "warn", "refuse"],
    wallet: Wallet | None,
    message: str,
) -> Verdict:
    """A verdict over a wallet that was actually read, attributed if it can be."""
    if wallet is None:
        return Verdict(level, message)
    return Verdict(level, f"{message} ({wallet.provenance()})")


def openrouter_key() -> tuple[str, str] | None:
    """The submitting host's own OpenRouter key and the name it came from.

    The name travels with the key because the verdict has to print it: a
    reading that cannot say which variable it read cannot be told from a
    reading of the wrong account (#3308). Names are printable; the key is
    not, and nothing but :func:`fetch_openrouter_balance` may see it.

    Deliberately local-only. A cloud run's seats read their credentials from
    SSM and the submitting host may legitimately hold none of them — in which
    case the balance simply goes unchecked. Reaching into SSM to read a
    secret this process does not otherwise need, purely to price a run, is a
    worse trade than an unchecked preflight.
    """
    for name in KEY_ENV_NAMES:
        value = os.environ.get(name)
        if value:
            return name, value
    return None


def fetch_openrouter_balance(api_key: str, timeout: float = 10.0) -> Balance | None:
    """Ask OpenRouter what is left on this key, or ``None`` if it will not say.

    ``None`` on every failure — unreachable, unauthorised, unparseable, an
    unexpected body shape. This function decides nothing; :func:`verdict`
    turns "cannot tell" into a report rather than a refusal.
    """
    request = urllib.request.Request(  # noqa: S310 - fixed https URL, not user input
        "https://openrouter.ai/api/v1/credits",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:  # noqa: S310
            payload = json.loads(response.read().decode("utf-8"))
    except (urllib.error.URLError, TimeoutError, ValueError, OSError):
        return None
    data = payload.get("data") if isinstance(payload, dict) else None
    if not isinstance(data, dict):
        return None
    try:
        granted = float(data["total_credits"])
        used = float(data["total_usage"])
    except (KeyError, TypeError, ValueError):
        return None
    return Balance(
        remaining_usd=granted - used,
        source="openrouter /api/v1/credits",
    )
