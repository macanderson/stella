# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Checks a match must survive before anything is dispatched.

Both live here rather than at their call sites for the same reason: they are
the same *kind* of question — "will this run be able to do what it claims?" —
asked by two paths (`MatchRunner.create` locally, `arenabench cloud run` in
the cloud), and answered identically by both. Neither may drift.

Each check refuses a failure whose whole cost is paid *after* submission,
when it is far too late to be cheap:

- a task the dataset does not contain becomes a container that exits 1 and
  scores as an operational abort (#3255);
- a balance that cannot fund the run becomes a 402 partway through, killing
  the remaining trials the same way.

Both fail **open** when they cannot tell. An unmaterialised corpus and an
unreachable gateway are absences of evidence, not evidence of a problem, and
a preflight that blocks on either would stop runs that would have succeeded.
"""

from __future__ import annotations

from . import balance
from .model import MatchSpec
from .registry import Dataset, Registry, known_task_names

__all__ = ["balance_verdict", "require_known_tasks", "resolve_dataset"]


def resolve_dataset(spec: MatchSpec, registry: Registry) -> Dataset:
    """The dataset this match will run, or a refusal naming what is wrong.

    The two ways a corpus can fail a match, answered together because a
    caller needs both answered before it builds anything: the dataset itself
    may be unregistered, and a registered dataset may not contain a task the
    match names.
    """
    dataset = registry.get(spec.dataset)
    if dataset is None:
        raise ValueError(f"unknown dataset: {spec.dataset}")
    require_known_tasks(spec, registry)
    return dataset


def require_known_tasks(spec: MatchSpec, registry: Registry) -> None:
    """Raise if the match names a task ``spec.dataset`` does not contain.

    Empty corpus means "cannot enumerate", never "no tasks exist", so a host
    without the dataset materialised is unaffected rather than blocked.
    """
    phantoms = spec.unknown_tasks(known_task_names(registry, spec.dataset))
    if phantoms:
        raise ValueError(
            f"{spec.dataset} has no such task(s): {', '.join(phantoms)} — "
            "refusing the match rather than dispatching a trial that can "
            "only abort (#3255)"
        )


def balance_verdict(
    trials: int,
    cost_per_trial_usd: float | None,
    seat_credential: str | None = None,
) -> balance.Verdict:
    """Price ``trials`` against whatever balance this host can read.

    The key is read from the submitting host's own environment only: a cloud
    seat reads its credentials from SSM and this host may hold none of them,
    and reaching into SSM for a secret the process does not otherwise need,
    purely to price a run, is a worse trade than an unchecked preflight.

    That trade is settled, and ``seat_credential`` is what keeps it honest.
    A verdict priced against one wallet and spent from another must say so,
    or a false all-clear reads exactly like a real one (#3308) — so the
    caller passes the credential name the seats will actually spend when it
    knows one, and the verdict names both.
    """
    found = balance.openrouter_key()
    if found is None:
        return balance.verdict(None, trials, cost_per_trial_usd)
    env_name, key = found
    reading = balance.fetch_openrouter_balance(key)
    return balance.verdict(
        reading.remaining_usd if reading else None,
        trials,
        cost_per_trial_usd,
        balance.Wallet(env_name=env_name, seat_credential=seat_credential),
    )


def non_stock_timeout_notice(multiplier: float) -> str | None:
    """A one-line banner for a non-stock ``agent_timeout_multiplier`` (#3256).

    tbench.ai's Terminal-Bench 2.1 leaderboard rule — "submissions may not
    modify timeouts or resources" — makes every number a run at this setting
    produces non-comparable to a published leaderboard row. Every number
    recorded before 2026-08-14 used ``agent_timeout_multiplier = 2.0`` and
    carried no such qualifier (`docs/timeout-policy.md`); this is the launch
    site saying so while the operator is still looking, rather than leaving
    it to be rediscovered once a number is already quoted.

    ``None`` at the stock ``1.0`` — the overwhelming common case, and the one
    that must produce no note at all.
    """
    if multiplier == 1.0:
        return None
    return (
        f"agent_timeout_multiplier={multiplier} (!= 1.0): this run is "
        "non-submittable to the tbench.ai leaderboard and must not be "
        "compared to a published row without that qualifier — see "
        "docs/timeout-policy.md"
    )
