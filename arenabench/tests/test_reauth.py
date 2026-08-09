# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Credential-rotation recovery for a subscription-authenticated seat."""

from __future__ import annotations

import pytest

from arenabench.reauth import (
    SeatState,
    Snapshot,
    Supervisor,
    TrialView,
    credential_digest,
    decide,
    is_credential_void,
)
from arenabench.telemetry import VOID_CREDENTIALS

TASKS = ("alpha", "beta", "gamma")


def snap(**over):
    base = dict(
        tasks=TASKS,
        trials=(),
        dispatched_digest=credential_digest("dead-token"),
        available_digest=None,
        rounds_used=0,
        max_rounds=8,
        seat_running=True,
    )
    base.update(over)
    return Snapshot(**base)


def done(task: str, failure: str = "") -> TrialView:
    return TrialView(task=task, status="done", failure=failure)


# -- classification ---------------------------------------------------------


def test_every_credential_error_class_is_recognised():
    """The classifier tracks the reviewed taxonomy, not a private copy."""
    assert VOID_CREDENTIALS  # the set is non-empty, so the loop below proves something
    for name in VOID_CREDENTIALS:
        assert is_credential_void(name), name


@pytest.mark.parametrize(
    "failure",
    ["AgentTimeoutError", "NonZeroAgentExitCodeError", "VerifierTimeoutError", ""],
)
def test_non_credential_failures_are_not_credential_voids(failure):
    assert not is_credential_void(failure)


# -- the credential never leaks --------------------------------------------


def test_digest_is_not_the_token_and_is_stable_within_a_process():
    token = "sk-ant-oat01-secret-value"
    digest = credential_digest(token)
    assert digest is not None
    assert token not in digest
    assert "secret" not in digest
    assert credential_digest(token) == digest
    assert credential_digest("a-different-token") != digest


def test_absent_and_blank_tokens_are_indistinguishable_from_no_token():
    """A blank keychain answer must not read as 'we have a credential'."""
    assert credential_digest(None) is None
    assert credential_digest("") is None
    assert credential_digest("   ") is None


# -- the core state machine -------------------------------------------------


def test_a_single_non_credential_failure_does_not_pause_the_run():
    """'Allow one task to fail, check the type' — a real loss is just a result.

    The seat keeps running and nothing is queued for re-dispatch: a timeout is
    something the agent did, and re-running it to get a nicer number is the
    dishonesty this module must not enable.
    """
    step = decide(snap(trials=(done("alpha", "AgentTimeoutError"),)))
    assert step.state is SeatState.RUNNING
    assert step.dispatch == ()
    assert not step.stop_seat
    assert not step.needs_operator


def test_a_lost_task_is_never_re_dispatched_even_alongside_a_credential_void():
    """The honesty property: only the voided and the never-run come back."""
    step = decide(
        snap(
            trials=(
                done("alpha", "AgentTimeoutError"),  # a real loss — keep it
                done("beta", "AuthenticationError"),  # voided — re-run it
            ),
            available_digest=credential_digest("fresh-token"),
            seat_running=False,
        )
    )
    assert step.dispatch == ("beta", "gamma")
    assert "alpha" not in step.dispatch


def test_credential_void_with_no_fresh_token_pauses_for_the_operator():
    step = decide(snap(trials=(done("alpha", "AuthenticationError"),)))
    assert step.state is SeatState.PAUSED
    assert step.needs_operator
    assert step.stop_seat
    assert step.dispatch == ()


def test_a_keychain_that_still_holds_the_dead_token_pauses_rather_than_retrying():
    """Rotation has not happened yet — re-dispatching would burn a round."""
    dead = credential_digest("dead-token")
    step = decide(snap(trials=(done("alpha", "AuthenticationError"),), available_digest=dead))
    assert step.state is SeatState.PAUSED
    assert step.dispatch == ()


def test_a_fresh_token_stops_the_seat_before_dispatching_anything():
    """Never kill and launch in one step: two jobs on one directory is corruption."""
    step = decide(
        snap(
            trials=(done("alpha", "AuthenticationError"),),
            available_digest=credential_digest("fresh-token"),
            seat_running=True,
        )
    )
    assert step.state is SeatState.HEALING
    assert step.stop_seat
    assert step.dispatch == ()


def test_once_the_seat_is_stopped_the_unmeasured_tasks_are_dispatched():
    """Voided *and* never-run: a token death strands both, in dispatch order."""
    step = decide(
        snap(
            trials=(done("alpha", "AuthenticationError"),),
            available_digest=credential_digest("fresh-token"),
            seat_running=False,
        )
    )
    assert step.state is SeatState.HEALING
    assert step.dispatch == ("alpha", "beta", "gamma")
    assert not step.stop_seat


def test_a_completed_sweep_reports_complete_even_with_voids_on_the_record():
    step = decide(
        snap(
            trials=(done("alpha"), done("beta", "AuthenticationError"), done("beta"), done("gamma")),
            available_digest=credential_digest("fresh-token"),
            seat_running=False,
        )
    )
    assert step.state is SeatState.COMPLETE


def test_completion_wins_over_an_exhausted_budget():
    """Finishing on the last permitted round is COMPLETE, not EXHAUSTED."""
    step = decide(
        snap(
            trials=tuple(done(t) for t in TASKS),
            rounds_used=8,
            max_rounds=8,
            seat_running=False,
        )
    )
    assert step.state is SeatState.COMPLETE


def test_an_exhausted_budget_stops_and_names_the_shortfall():
    """An unfixable auth problem terminates instead of spinning."""
    step = decide(
        snap(
            trials=(done("alpha", "AuthenticationError"),),
            available_digest=credential_digest("fresh-token"),
            rounds_used=8,
            max_rounds=8,
        )
    )
    assert step.state is SeatState.EXHAUSTED
    assert step.dispatch == ()
    assert "unmeasured" in step.reason


def test_an_exhausted_supervisor_never_asks_the_operator_for_a_token():
    """Budget is checked before healing, so a paused prompt is always actionable."""
    step = decide(
        snap(
            trials=(done("alpha", "AuthenticationError"),),
            available_digest=None,
            rounds_used=8,
            max_rounds=8,
        )
    )
    assert step.state is SeatState.EXHAUSTED
    assert not step.needs_operator


def test_no_reason_string_can_carry_a_credential():
    """Every terminal state's operator-facing line is credential-free."""
    secret = "sk-ant-oat01-do-not-leak"
    for available, running, rounds in [
        (None, True, 0),
        (credential_digest(secret), True, 0),
        (credential_digest(secret), False, 0),
        (credential_digest(secret), True, 99),
    ]:
        step = decide(
            snap(
                trials=(done("alpha", "AuthenticationError"),),
                available_digest=available,
                seat_running=running,
                rounds_used=rounds,
            )
        )
        assert secret not in step.reason


# -- the supervisor end to end ---------------------------------------------


class Rig:
    def __init__(self, token=None):
        self.stopped = 0
        self.launches = []
        self.token = token

    def source(self):
        return self.token


def test_supervisor_pauses_then_resumes_on_an_operator_supplied_token():
    """The full flow the operator sees: die, pause, hand over a token, carry on."""
    rig = Rig(token=None)
    sup = Supervisor(
        tasks=TASKS,
        token_source=rig.source,
        stop=lambda: setattr(rig, "stopped", rig.stopped + 1),
        launch=lambda tasks, token: rig.launches.append((tuple(tasks), token)),
    )
    sup.note_dispatch("dead-token")

    trials = [done("alpha"), done("beta", "AuthenticationError")]

    # 1. The token dies and no local login can replace it: pause, seat stopped.
    step = sup.tick(trials, seat_running=True)
    assert step.state is SeatState.PAUSED
    assert rig.stopped == 1
    assert rig.launches == []

    # 2. Still paused while the operator is away. No round is consumed.
    step = sup.tick(trials, seat_running=False)
    assert step.state is SeatState.PAUSED
    assert sup.rounds_used == 0

    # 3. The operator hands over a token: the unmeasured tasks re-dispatch.
    sup.provide_token("fresh-token")
    step = sup.tick(trials, seat_running=False)
    assert step.state is SeatState.HEALING
    assert sup.rounds_used == 1
    assert len(rig.launches) == 1
    dispatched, used = rig.launches[0]
    assert dispatched == ("beta", "gamma")  # alpha was measured; it stays measured
    assert used == "fresh-token"

    # 4. Those tasks come back with real results: the sweep is complete.
    step = sup.tick([done("alpha"), done("beta"), done("gamma")], seat_running=True)
    assert step.state is SeatState.COMPLETE


def test_supervisor_heals_from_the_keychain_without_an_operator():
    """Rotation's common case: the new token is already in the local login."""
    rig = Rig(token="rotated-token")
    sup = Supervisor(
        tasks=TASKS,
        token_source=rig.source,
        stop=lambda: setattr(rig, "stopped", rig.stopped + 1),
        launch=lambda tasks, token: rig.launches.append((tuple(tasks), token)),
    )
    sup.note_dispatch("dead-token")
    trials = [done("alpha", "AuthenticationError")]

    assert sup.tick(trials, seat_running=True).state is SeatState.HEALING
    assert rig.stopped == 1
    assert rig.launches == []

    step = sup.tick(trials, seat_running=False)
    assert step.state is SeatState.HEALING
    assert rig.launches[0][1] == "rotated-token"
    assert sup.rounds_used == 1


def test_an_operator_token_beats_the_keychain():
    """A paused run must not strand itself holding the answer it was given."""
    rig = Rig(token="dead-token")  # keychain has not rotated
    sup = Supervisor(
        tasks=TASKS,
        token_source=rig.source,
        stop=lambda: None,
        launch=lambda tasks, token: rig.launches.append((tuple(tasks), token)),
    )
    sup.note_dispatch("dead-token")
    trials = [done("alpha", "AuthenticationError")]

    assert sup.tick(trials, seat_running=True).state is SeatState.PAUSED
    sup.provide_token("operator-token")
    sup.tick(trials, seat_running=False)
    assert rig.launches[0][1] == "operator-token"


def test_the_supervisor_log_never_records_a_credential():
    secret = "sk-ant-oat01-do-not-leak"
    rig = Rig(token=secret)
    sup = Supervisor(
        tasks=TASKS,
        token_source=rig.source,
        stop=lambda: None,
        launch=lambda tasks, token: None,
    )
    sup.note_dispatch("dead-token")
    trials = [done("alpha", "AuthenticationError")]
    sup.tick(trials, seat_running=True)
    sup.tick(trials, seat_running=False)
    assert sup.log
    assert not any(secret in line for line in sup.log)
    assert secret not in repr(sup)
