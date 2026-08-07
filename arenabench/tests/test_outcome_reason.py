# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The outcome taxonomy (#2076): one closed label per judged trial.

The label annotates Harbor's verdict, never overrides it — ``resolved`` is
read, not recomputed — and the Group A/void half must equal exactly the
trials the ``infrastructure`` machinery already voids: this taxonomy adds
names, never reclassifications.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from arenabench.telemetry import (
    INFRASTRUCTURE_FAILURES,
    VOID_CANCELLED,
    VOID_CREDENTIALS,
    VOID_SETUP,
    VOID_VERIFIER,
    TrialMetrics,
)

_MATCHES = Path.home() / ".arenabench" / "matches"


def _trial(
    *,
    status: str = "done",
    resolved: bool | None,
    failure: str = "",
    infrastructure: bool = False,
) -> TrialMetrics:
    m = TrialMetrics(task="t", trial="t__x")
    m.status = status
    m.resolved = resolved
    m.failure = failure
    m.infrastructure = infrastructure
    return m


class TestVocabulary:
    def test_void_buckets_partition_the_infrastructure_set_exactly(self) -> None:
        """A name added to INFRASTRUCTURE_FAILURES must be bucketed in the
        same change — and no bucket may carry a name the void machinery
        does not actually void."""
        buckets = (VOID_SETUP, VOID_CREDENTIALS, VOID_CANCELLED, VOID_VERIFIER)
        union: set[str] = set()
        for bucket in buckets:
            assert not (union & bucket), "a failure name sits in two buckets"
            union |= bucket
        assert union == INFRASTRUCTURE_FAILURES

    def test_group_b_labels_annotate_the_verdict(self) -> None:
        cases = {
            "solved_clean": _trial(resolved=True),
            "solved_then_timeout": _trial(resolved=True, failure="AgentTimeoutError"),
            "solved_then_error": _trial(resolved=True, failure="ValueError"),
            "failed_wrong_answer": _trial(resolved=False),
            "timeout_before_solve": _trial(resolved=False, failure="AgentTimeoutError"),
            "agent_error": _trial(resolved=False, failure="NonZeroAgentExitCodeError"),
        }
        for expected, metrics in cases.items():
            assert metrics.outcome_reason() == expected

    def test_group_a_voids_carry_the_prefix_and_the_cause(self) -> None:
        assert (
            _trial(resolved=None, failure="CancelledError", infrastructure=True)
            .outcome_reason()
            == "void_cancelled"
        )
        assert (
            _trial(resolved=None, failure="AuthenticationError", infrastructure=True)
            .outcome_reason()
            == "void_credentials"
        )
        assert (
            _trial(resolved=None, failure="HealthcheckError", infrastructure=True)
            .outcome_reason()
            == "void_setup"
        )
        assert (
            _trial(resolved=None, failure="VerifierTimeoutError", infrastructure=True)
            .outcome_reason()
            == "void_verifier"
        )
        # The zero-spend guard (#1480): an agent-named exception voided
        # because not one token was observed.
        assert (
            _trial(
                resolved=None,
                failure="NonZeroAgentExitCodeError",
                infrastructure=True,
            ).outcome_reason()
            == "void_no_fair_attempt"
        )

    def test_unjudged_trials_have_no_reason_yet(self) -> None:
        assert _trial(status="running", resolved=None).outcome_reason() is None
        assert _trial(status="pending", resolved=None).outcome_reason() is None

    def test_the_reason_rides_to_json(self) -> None:
        payload = _trial(resolved=True, failure="AgentTimeoutError").to_json()
        assert payload["outcome_reason"] == "solved_then_timeout"
        # The two-facts rule (#2066): the verdict and the incident both
        # survive beside the label.
        assert payload["resolved"] is True
        assert payload["failure"] == "AgentTimeoutError"


@pytest.mark.skipif(
    not _MATCHES.is_dir(), reason="no local arenabench corpus on this machine"
)
class TestAgainstTheLocalCorpus:
    """The issue's done-when, run wherever the rig's matches exist."""

    def test_every_judged_trial_maps_and_voids_match_the_flag(self) -> None:
        from arenabench.server import ArenaServer

        arena = ArenaServer(_MATCHES.parent)
        judged = 0
        for match in arena.runner.matches.values():
            for contestant in match.spec.contestants:
                for m in match.metrics_for(contestant.id).values():
                    if m.status != "done":
                        assert m.outcome_reason() is None
                        continue
                    judged += 1
                    reason = m.outcome_reason()
                    assert reason, f"{match.spec.id}/{m.trial}: judged, no reason"
                    # Group A == the trials the void machinery already
                    # excludes (resolved is None on a judged trial); the
                    # taxonomy must add names, never reclassify.
                    assert reason.startswith("void_") == (m.resolved is None), (
                        f"{match.spec.id}/{m.trial}: {reason} vs "
                        f"resolved={m.resolved}"
                    )
        assert judged, "corpus present but nothing judged — the walk is inert"

    def test_the_live_fixture_reads_solved_then_timeout(self) -> None:
        from arenabench.server import ArenaServer

        match = ArenaServer(_MATCHES.parent).runner.matches.get("3568657c2db9")
        if match is None:
            pytest.skip("fixture match 3568657c2db9 not on this machine")
        for contestant in match.spec.contestants:
            metrics = match.metrics_for(contestant.id)
            m = metrics.get("crack-7z-hash")
            if m is not None and m.status == "done":
                assert m.outcome_reason() == "solved_then_timeout"
                return
        pytest.skip("crack-7z-hash trial not judged in the fixture match")
