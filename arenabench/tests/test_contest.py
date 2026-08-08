# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The parameterised contest: seats, refusals, and the comparability note."""

from __future__ import annotations

import pytest

from arenabench.presets import (
    PANEL_TASKS,
    SUPPORTED_WORKERS,
    ContestError,
    contest_comparability_note,
    contest_spec,
    normalize_worker,
)


@pytest.mark.parametrize(
    "spelling", ["anthropic/claude-sonnet-5", "claude-sonnet-5", "  claude-sonnet-5  "]
)
def test_either_spelling_of_a_worker_is_accepted(spelling: str) -> None:
    assert normalize_worker(spelling) == "claude-sonnet-5"


def test_an_unknown_worker_is_refused_and_names_the_supported_ones() -> None:
    """A default verifier would be a match that runs and measures the wrong
    thing — the one outcome worse than an error."""
    with pytest.raises(ContestError) as excinfo:
        normalize_worker("openai/gpt-5.5")
    message = str(excinfo.value)
    for worker in SUPPORTED_WORKERS:
        assert worker in message


def test_the_claude_code_seat_is_first_party_and_stellas_is_openrouter() -> None:
    """The regression this entry point exists to make impossible.

    A Claude Code seat on ``api: openrouter`` holds a valid key, passes the
    credential check, and then fails EVERY trial at turn 1 with
    ``apiKeySource: none`` — scoring 0.0 indistinguishably from a real loss.
    A parameterised contest that hand-rolled its seats would re-derive it.
    """
    spec = contest_spec("anthropic/claude-sonnet-5")
    seats = {seat.id: seat for seat in spec.contestants}

    assert seats["claude-code"].engine.api == "anthropic"
    assert seats["claude-code"].engine.model == "claude-sonnet-5"
    assert not getattr(seats["claude-code"].engine, "base_url", None)

    assert seats["stella"].engine.api == "openrouter"
    assert seats["stella"].engine.model == "anthropic/claude-sonnet-5"


def test_both_arms_run_the_same_worker_at_the_same_effort() -> None:
    """The variable under test is the agent architecture, never the model."""
    spec = contest_spec("claude-opus-5")
    seats = {seat.id: seat for seat in spec.contestants}
    assert seats["stella"].engine.effort == seats["claude-code"].engine.effort
    assert seats["stella"].engine.model.endswith(seats["claude-code"].engine.model)


def test_the_verifier_is_never_the_worker() -> None:
    """A verifier that is its own worker cannot judge it independently."""
    for worker in SUPPORTED_WORKERS:
        spec = contest_spec(worker)
        stella = next(s for s in spec.contestants if s.id == "stella")
        verifier = stella.engine.roles["verifier"].model
        assert verifier != stella.engine.model, worker


def test_num_tasks_slices_the_panel_from_the_front() -> None:
    spec = contest_spec("claude-sonnet-5", num_tasks=3)
    assert tuple(spec.tasks) == PANEL_TASKS[:3]


def test_the_full_panel_is_the_default_and_carries_no_warning() -> None:
    spec = contest_spec("claude-sonnet-5")
    assert tuple(spec.tasks) == PANEL_TASKS
    # A warning that fires every time is a warning nobody reads.
    assert contest_comparability_note(len(PANEL_TASKS)) is None


def test_a_sliced_panel_says_it_is_not_comparable() -> None:
    note = contest_comparability_note(3)
    assert note is not None
    assert "NOT comparable" in note


@pytest.mark.parametrize(
    "kwargs",
    [
        {"num_tasks": 0},
        {"num_tasks": -1},
        {"num_tasks": len(PANEL_TASKS) + 1},
        {"concurrency": 0},
        {"attempts": 0},
    ],
)
def test_incoherent_parameters_are_refused(kwargs: dict[str, int]) -> None:
    with pytest.raises(ContestError):
        contest_spec("claude-sonnet-5", **kwargs)


def test_concurrency_and_attempts_reach_the_spec() -> None:
    spec = contest_spec("claude-sonnet-5", num_tasks=4, concurrency=2, attempts=3)
    assert spec.concurrency == 2
    assert spec.attempts == 3
