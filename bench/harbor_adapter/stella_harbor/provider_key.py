# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

"""The dedicated OpenRouter key: its ledger history and its live state.

Two questions the secure launcher asks before it spends anything.
`prior_stage_outcome` reads the public ledger and refuses unless every paid
stage so far bound this one key and reconciled its spend;
`validate_live_provider_key` reads the key's live state and refuses unless it
still matches the intent and can cover the job.

Each refusal is a named predicate with one message, and those messages are the
launcher's contract — `bench/harbor_adapter/tests/test_secure_launcher.py`
matches on them, so change one only alongside the test that asserts it.
"""

from __future__ import annotations

import math
import re
import uuid
from collections.abc import Mapping, Sequence
from datetime import datetime
from typing import Any

from .value_shapes import (
    aware_timestamp,
    finite_nonnegative_number,
    require_exact_object,
    utc_text,
)

__all__ = [
    "DEDICATED_KEY_HARD_LIMIT_USD",
    "DEDICATED_KEY_LABEL",
    "prior_stage_outcome",
    "validate_live_provider_key",
]

DEDICATED_KEY_LABEL = "stella-tb21-dedicated-key-v1"

# The label every paid stage must bind, alongside the hard limit below.
# Binding both is what stops a run charging a key nobody preregistered.
#
# The only bound that can actually stop a claim run spending, now that no
# trial carries one (#2411). It sits at the provider, so exhausting it fails
# the run visibly rather than truncating a trial into a loss — which is the
# whole reason a wallet is guarded here and never inside a trial.
#
# It is now required arithmetic rather than a backstop, and it moved from
# $180 to $600 because of that shift. The confirmatory stage requests 445
# trials: at the frozen $0.17 cap that projected to $75.65, comfortably
# inside $180 *because the cap made it so*, and at the measured forecast
# (`secure_launcher`'s `_CANONICAL_PER_TRIAL_FORECAST_USD`) it projects to
# $534. The old limit did not bound the run's cost — it
# bounded how much of each task the agent was allowed to finish, and the
# projection only fit because trials were being stopped short.
#
# So this is not new spending appetite; it is the same run, priced at what it
# costs. It is also a PROVISIONING REQUIREMENT, not an authorisation:
# on 2026-08-08 the account behind this key had $35.72 left of $1,110, so a
# confirmatory stage cannot run at any per-trial price until it is funded.
#
# Nothing here spends money or grants permission to. The real gate is the
# live-credit check in `_verify_public_paid_intent`, which reads the key's
# actual remaining credit rather than this constant — so an unfunded key fails
# preflight instead of launching and abandoning trials that would then score as
# losses. This number only says what the key must be provisioned to before the
# stage is runnable at all.
DEDICATED_KEY_HARD_LIMIT_USD = 600.0

_ARTIFACT_DIGEST_RE = re.compile(r"[0-9a-f]{64}")

# Which paid stages must already stand, for each stage a launch can be.
_EXPECTED_PAID_CHAIN = {
    "readiness": ["readiness"],
    "calibration": ["readiness", "calibration"],
    "confirmatory": ["readiness", "calibration", "confirmatory"],
}

_PAID_STAGES = frozenset({"readiness", "calibration", "confirmatory"})


def _sole_unspent_current_intent(
    ledger: Mapping[str, Any], current_digest: str
) -> Mapping[str, Any]:
    """The one ledger entry for this intent, refused if it already ran."""
    current_wrappers = [
        wrapper
        for wrapper in ledger["intents"]
        if wrapper["intent_sha256"] == current_digest
    ]
    if len(current_wrappers) != 1:
        raise RuntimeError("public ledger does not uniquely identify current intent")
    if any(
        outcome.get("intent_sha256") == current_digest for outcome in ledger["outcomes"]
    ):
        raise RuntimeError("current paid intent already has a post-launch outcome")
    return current_wrappers[0]


def _paid_stage_chain(
    ledger: Mapping[str, Any], *, stage: str, current_wrapper: Mapping[str, Any]
) -> Sequence[Mapping[str, Any]]:
    """The paid stages so far, refused unless they are this stage's exact prefix."""
    paid_wrappers = [
        wrapper
        for wrapper in ledger["intents"]
        if wrapper["intent"].get("historical") is False
        and wrapper["intent"].get("stage") in _PAID_STAGES
    ]
    expected_stages = _EXPECTED_PAID_CHAIN[stage]
    if [wrapper["intent"].get("stage") for wrapper in paid_wrappers] != expected_stages:
        raise RuntimeError("public ledger paid-stage chain is not the exact prefix")
    if paid_wrappers[-1] != current_wrapper:
        raise RuntimeError("public ledger current intent is not the paid-stage tip")
    return paid_wrappers


def _paid_outcomes_for(
    ledger: Mapping[str, Any], paid_wrappers: Sequence[Mapping[str, Any]]
) -> Sequence[Mapping[str, Any]]:
    """The outcomes of the prior paid stages: one each, and none for this one."""
    paid_digests = {wrapper["intent_sha256"] for wrapper in paid_wrappers}
    paid_outcomes = [
        outcome
        for outcome in ledger["outcomes"]
        if outcome.get("intent_sha256") in paid_digests
    ]
    if len(paid_outcomes) != len(paid_wrappers) - 1:
        raise RuntimeError(
            "public ledger paid outcomes do not exactly cover prior stages"
        )
    return paid_outcomes


def _continuous_usage_before(
    intent: Mapping[str, Any],
    *,
    expected_key_identity: Mapping[str, Any],
    previous_usage_after: float | None,
    intent_stage: str,
) -> float:
    """This stage's opening usage, refused unless it binds the key and continues."""
    provider = intent["provider_key"]
    observed_key_identity = {
        "fingerprint_sha256": provider.get("fingerprint_sha256"),
        "label": provider.get("label"),
        "limit_usd": provider.get("limit_usd"),
    }
    if observed_key_identity != dict(expected_key_identity):
        raise RuntimeError("all paid stages must bind one exact dedicated key")
    usage_before = finite_nonnegative_number(
        provider.get("usage_before_usd"),
        label=f"{intent_stage} intent provider usage_before_usd",
    )
    if previous_usage_after is not None and not math.isclose(
        usage_before, previous_usage_after, rel_tol=0, abs_tol=1e-9
    ):
        raise RuntimeError("paid-stage provider usage is not continuous")
    return usage_before


def _sole_prior_outcome(
    paid_outcomes: Sequence[Mapping[str, Any]],
    wrapper: Mapping[str, Any],
    intent_stage: str,
) -> Mapping[str, Any]:
    """The one recorded outcome for a prior stage."""
    matches = [
        outcome
        for outcome in paid_outcomes
        if outcome.get("intent_sha256") == wrapper["intent_sha256"]
    ]
    if len(matches) != 1:
        raise RuntimeError(
            f"public ledger lacks exactly one prior {intent_stage} outcome"
        )
    return matches[0]


def _require_canonical_job_and_artifact(
    outcome: Mapping[str, Any], intent_stage: str
) -> None:
    """A prior stage names one canonical job id and one artifact digest."""
    try:
        parsed_job_id = uuid.UUID(str(outcome.get("job_id")))
    except (ValueError, AttributeError) as exc:
        raise RuntimeError(f"prior {intent_stage} job_id is not one UUID") from exc
    if parsed_job_id.int == 0 or str(parsed_job_id) != outcome.get("job_id"):
        raise RuntimeError(f"prior {intent_stage} job_id is not canonical")
    artifact_digest = outcome.get("artifact_tree_sha256")
    if (
        not isinstance(artifact_digest, str)
        or _ARTIFACT_DIGEST_RE.fullmatch(artifact_digest) is None
    ):
        raise RuntimeError(f"prior {intent_stage} artifact digest is invalid")


def _reconciled_spend(
    outcome: Mapping[str, Any], *, usage_before: float, intent_stage: str
) -> float:
    """A prior stage's closing usage, refused unless its spend reconciled.

    Returns the usage the next stage must open at, which is what makes the
    chain continuous rather than merely each link plausible.
    """
    before = finite_nonnegative_number(
        outcome.get("provider_usage_before_usd"),
        label=f"prior {intent_stage} provider usage before",
    )
    after = finite_nonnegative_number(
        outcome.get("provider_usage_after_usd"),
        label=f"prior {intent_stage} provider usage after",
    )
    delta = finite_nonnegative_number(
        outcome.get("provider_usage_delta_usd"),
        label=f"prior {intent_stage} provider usage delta",
    )
    telemetry = finite_nonnegative_number(
        outcome.get("telemetry_cost_sum_usd"),
        label=f"prior {intent_stage} telemetry sum",
    )
    tolerance = finite_nonnegative_number(
        outcome.get("reconciliation_tolerance_usd"),
        label=f"prior {intent_stage} reconciliation tolerance",
    )
    if (
        not math.isclose(before, usage_before, rel_tol=0, abs_tol=1e-9)
        or after < before
        or not math.isclose(delta, after - before, rel_tol=0, abs_tol=1e-9)
        or tolerance > 0.01
        or abs(delta - telemetry) > tolerance + 1e-12
        or outcome.get("reconciliation_status") != "reconciled"
    ):
        raise RuntimeError(f"prior {intent_stage} spend is not reconciled")
    return after


def _require_completed_and_recorded(
    outcome: Mapping[str, Any],
    *,
    intent_stage: str,
    wrapper: Mapping[str, Any],
    publications_by_subject: Mapping[tuple[str, str], Mapping[str, Any]],
    next_sequence: int,
    current_declared: datetime,
    current_sequence: int,
) -> None:
    """A prior stage was published, then run, then recorded — in that order."""
    completed_at = aware_timestamp(
        outcome.get("completed_at"), label=f"prior {intent_stage} completed_at"
    )
    started_at = aware_timestamp(
        outcome.get("started_at"), label=f"prior {intent_stage} started_at"
    )
    recorded_at = aware_timestamp(
        outcome.get("recorded_at"), label=f"prior {intent_stage} recorded_at"
    )
    intent_publication = publications_by_subject.get(
        ("intent", wrapper["intent_sha256"])
    )
    publication_sequence = (
        intent_publication.get("sequence")
        if isinstance(intent_publication, Mapping)
        else None
    )
    expected_status = "excluded" if intent_stage == "readiness" else "complete"
    if (
        outcome.get("status") != expected_status
        or not isinstance(publication_sequence, int)
        or publication_sequence >= outcome["sequence"]
        or outcome["sequence"] >= next_sequence
        or started_at > completed_at
        or completed_at > recorded_at
        or recorded_at > current_declared
        or outcome["sequence"] >= current_sequence
    ):
        raise RuntimeError(
            f"prior {intent_stage} outcome was not completed and recorded"
        )


def prior_stage_outcome(
    ledger: dict[str, Any],
    *,
    stage: str,
    current_digest: str,
    current_intent: dict[str, Any],
    publications_by_subject: Mapping[tuple[str, str], Mapping[str, Any]],
) -> dict[str, Any] | None:
    """The prior paid stage's outcome, or `None` when this is the first one."""
    current_wrapper = _sole_unspent_current_intent(ledger, current_digest)
    paid_wrappers = _paid_stage_chain(
        ledger, stage=stage, current_wrapper=current_wrapper
    )
    paid_outcomes = _paid_outcomes_for(ledger, paid_wrappers)

    current_sequence = current_wrapper["sequence"]
    current_declared = aware_timestamp(
        current_intent.get("declared_at"), label="current intent declared_at"
    )
    expected_key_identity = {
        "fingerprint_sha256": current_intent["provider_key"]["fingerprint_sha256"],
        "label": DEDICATED_KEY_LABEL,
        "limit_usd": DEDICATED_KEY_HARD_LIMIT_USD,
    }

    prior_summary: dict[str, Any] | None = None
    previous_usage_after: float | None = None
    for index, wrapper in enumerate(paid_wrappers):
        intent = wrapper["intent"]
        intent_stage = intent["stage"]
        usage_before = _continuous_usage_before(
            intent,
            expected_key_identity=expected_key_identity,
            previous_usage_after=previous_usage_after,
            intent_stage=intent_stage,
        )
        # The last wrapper is this launch's own intent: it binds the key and
        # continues the usage chain, and has no outcome yet by construction.
        if index == len(paid_wrappers) - 1:
            break

        outcome = _sole_prior_outcome(paid_outcomes, wrapper, intent_stage)
        _require_canonical_job_and_artifact(outcome, intent_stage)
        previous_usage_after = _reconciled_spend(
            outcome, usage_before=usage_before, intent_stage=intent_stage
        )
        _require_completed_and_recorded(
            outcome,
            intent_stage=intent_stage,
            wrapper=wrapper,
            publications_by_subject=publications_by_subject,
            next_sequence=paid_wrappers[index + 1]["sequence"],
            current_declared=current_declared,
            current_sequence=current_sequence,
        )
        prior_summary = {
            "stage": intent_stage,
            "intent_sha256": wrapper["intent_sha256"],
            "status": outcome["status"],
            "completed_at": outcome["completed_at"],
            "recorded_at": outcome["recorded_at"],
        }

    if stage == "readiness":
        return None
    if prior_summary is None:
        raise RuntimeError("public ledger lacks a reconciled prior-stage outcome")
    return prior_summary


_LIVE_KEY_FIELDS = frozenset(
    {
        "is_management_key",
        "is_provisioning_key",
        "limit",
        "limit_reset",
        "limit_remaining",
        "usage",
    }
)

_KEY_RECORD_FIELDS = frozenset(
    {
        "disabled",
        "hash",
        "include_byok_in_limit",
        "limit",
        "limit_remaining",
        "limit_reset",
        "name",
        "usage",
    }
)


def _live_key_data(response: Mapping[str, Any]) -> Mapping[str, Any]:
    """The key's own view of itself: a normal dedicated hard-limit key."""
    outer = require_exact_object(
        response, frozenset({"data"}), label="OpenRouter key-control response"
    )
    data = outer.get("data")
    if (
        not isinstance(data, dict)
        or not _LIVE_KEY_FIELDS.issubset(data)
        or data.get("is_management_key") is not False
        or data.get("is_provisioning_key") is not False
        or data.get("limit_reset") is not None
    ):
        raise RuntimeError(
            "OpenRouter benchmark credential must be a normal dedicated hard-limit key"
        )
    return data


def _management_key_record(response: Mapping[str, Any]) -> Mapping[str, Any]:
    """The account's view of the same key, read with the management credential."""
    record_outer = require_exact_object(
        response,
        frozenset({"data"}),
        label="OpenRouter management key-record response",
    )
    record = record_outer.get("data")
    if not isinstance(record, dict) or not _KEY_RECORD_FIELDS.issubset(record):
        raise RuntimeError("OpenRouter management key record lacks required fields")
    return record


def _account_credits(response: Mapping[str, Any]) -> tuple[float, float]:
    """The account's total credits and total usage."""
    credits_outer = require_exact_object(
        response,
        frozenset({"data"}),
        label="OpenRouter credits response",
    )
    credits_data = require_exact_object(
        credits_outer.get("data"),
        frozenset({"total_credits", "total_usage"}),
        label="OpenRouter credits data",
    )
    total_credits = finite_nonnegative_number(
        credits_data.get("total_credits"), label="OpenRouter total credits"
    )
    total_usage = finite_nonnegative_number(
        credits_data.get("total_usage"), label="OpenRouter total usage"
    )
    return total_credits, total_usage


def validate_live_provider_key(
    response: Mapping[str, Any],
    key_record_response: Mapping[str, Any],
    credits_response: Mapping[str, Any],
    *,
    intent: Mapping[str, Any],
    runtime_identity: Mapping[str, Any],
    fetched_at: datetime,
) -> dict[str, Any]:
    """The key's live snapshot, refused unless it matches the intent and can
    cover the job."""
    data = _live_key_data(response)
    record = _management_key_record(key_record_response)
    provider = intent["provider_key"]

    live_limit = finite_nonnegative_number(data.get("limit"), label="live key limit")
    live_usage = finite_nonnegative_number(data.get("usage"), label="live key usage")
    live_remaining = finite_nonnegative_number(
        data.get("limit_remaining"), label="live key limit_remaining"
    )
    intended_limit = finite_nonnegative_number(
        provider.get("limit_usd"), label="intent key limit"
    )
    intended_usage = finite_nonnegative_number(
        provider.get("usage_before_usd"), label="intent key usage"
    )
    record_limit = finite_nonnegative_number(
        record.get("limit"), label="management key record limit"
    )
    record_usage = finite_nonnegative_number(
        record.get("usage"), label="management key record usage"
    )
    record_remaining = finite_nonnegative_number(
        record.get("limit_remaining"),
        label="management key record limit_remaining",
    )
    label = record.get("name")
    if (
        record.get("hash") != runtime_identity["provider_key_fingerprint_sha256"]
        or label != DEDICATED_KEY_LABEL
        or label != provider.get("label")
        or record.get("disabled") is not False
        or record.get("include_byok_in_limit") is not True
        or record.get("limit_reset") is not None
        or live_limit != intended_limit
        or live_limit != DEDICATED_KEY_HARD_LIMIT_USD
        or record_limit != live_limit
        or not math.isclose(live_usage, intended_usage, rel_tol=0, abs_tol=1e-9)
        or not math.isclose(record_usage, live_usage, rel_tol=0, abs_tol=1e-9)
        or not math.isclose(record_remaining, live_remaining, rel_tol=0, abs_tol=1e-6)
        or not math.isclose(
            live_remaining, live_limit - live_usage, rel_tol=0, abs_tol=1e-6
        )
    ):
        raise RuntimeError("live OpenRouter management key record differs from intent")

    projected = float(intent["requested_trials"]) * float(
        intent["per_trial_forecast_usd"]
    )
    projected_remaining = live_remaining - projected
    if projected_remaining < -1e-9:
        raise RuntimeError("live OpenRouter hard limit cannot cover the registered job")

    total_credits, total_usage = _account_credits(credits_response)
    available = total_credits - total_usage
    if available < -1e-6 or available + 1e-9 < projected:
        raise RuntimeError("OpenRouter account credit cannot cover the registered job")

    return {
        "fingerprint_sha256": runtime_identity["provider_key_fingerprint_sha256"],
        "label": label,
        "limit_usd": live_limit,
        "usage_usd": live_usage,
        "limit_remaining_usd": live_remaining,
        "nominal_planned_spend_usd": projected,
        "nominal_remaining_after_usd": projected_remaining,
        "total_credits_usd": total_credits,
        "total_usage_usd": total_usage,
        "available_credits_usd": available,
        "fetched_at_utc": utc_text(fetched_at),
    }
