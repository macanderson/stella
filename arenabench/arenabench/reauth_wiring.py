# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Binding :mod:`.reauth` to a live match: processes, containers, job dirs.

:mod:`.reauth` decides *whether* a seat that lost its OAuth credential should
be stopped, healed, paused or abandoned; it is a pure state machine over an
immutable snapshot plus a thin shell of injected callables, and it deliberately
knows nothing about Harbor, Docker, or the arena's object graph. This module is
the other side of those callables — the part that actually signals a process
group, reaps the containers the dead round was holding, and reads the match's
own bookkeeping.

It is a sibling rather than a section of :mod:`.runner` for the reason
:mod:`.reap` is: credential-rotation recovery is one nameable concern with its
own failure modes, and it is not :class:`~.runner.MatchRunner`'s core loop. The
runner keeps exactly three call sites — arm the supervision after launching,
tick it inside the poll loop, and hand an operator's token to it — and every
mechanism behind them lives here.

The dependency runs one way: :mod:`.runner` imports this, and this never
imports :mod:`.runner` at runtime. What it needs of a match it takes as a
parameter and reads by attribute, which is also what keeps every function here
drivable from a test with no Harbor, no Docker and no containers.
"""

from __future__ import annotations

import contextlib
import logging
import os
import signal
import subprocess
import time
from collections.abc import Callable, Sequence
from typing import TYPE_CHECKING

from . import reap, reauth
from .claude_oauth import CLAUDE_OAUTH_ENV, local_claude_token

if TYPE_CHECKING:  # pragma: no cover - types only, and importing them is a cycle
    from .model import Contestant
    from .runner import Match

__all__ = [
    "KILL_GRACE_SECONDS",
    "STOP_GRACE_SECONDS",
    "kill_process_group",
    "stop_match",
    "stop_process_group",
    "stop_seat",
    "supervise_match",
    "terminate_process_group",
    "tick",
]

log = logging.getLogger("arenabench.reauth")

#: How long a stopped seat is given to die before the supervisor moves on.
#:
#: Only long enough to reap the child and record its exit code — nothing waits
#: on it, because :func:`.reauth.decide` never dispatches while the seat's
#: process is still alive, so a Harbor that ignores the signal is simply
#: signalled again on the next tick.
STOP_GRACE_SECONDS = 5.0

#: How long a ``SIGKILL``ed group is given before the caller gives up on it.
#:
#: Short, because ``SIGKILL`` is not deliverable-and-ignorable the way
#: ``SIGTERM`` is: a group that has not gone in this long is blocked in an
#: uninterruptible syscall, and waiting longer will not change that. The point
#: of waiting at all is that the caller's *next* action — reaping containers —
#: is only correct once nothing is still creating them.
KILL_GRACE_SECONDS = 3.0


def _signal_group(process: subprocess.Popen, signum: int) -> None:
    """Send ``signum`` to the process's whole group, or to it alone."""
    try:
        os.killpg(os.getpgid(process.pid), signum)
    except (OSError, ProcessLookupError):
        with contextlib.suppress(OSError):
            if signum == signal.SIGKILL:
                process.kill()
            else:
                process.terminate()


def terminate_process_group(process: subprocess.Popen) -> None:
    """``SIGTERM`` a seat's whole process group, falling back to the parent.

    The group, not the process: Harbor spawns Docker and helper children, and
    terminating only the parent leaves containers running and the job directory
    growing afterwards.

    Asking politely and walking away, which is right for the supervisor —
    :func:`.reauth.decide` re-signals on the next tick, so a Harbor that
    ignores this is simply asked again. It is *not* right for an operator's
    stop, where nothing comes back for a second try; that is
    :func:`stop_process_group`.
    """
    _signal_group(process, signal.SIGTERM)


def kill_process_group(process: subprocess.Popen) -> None:
    """``SIGKILL`` a seat's whole process group, falling back to the parent."""
    _signal_group(process, signal.SIGKILL)


def stop_process_group(
    process: subprocess.Popen,
    *,
    grace: float = STOP_GRACE_SECONDS,
    kill_grace: float = KILL_GRACE_SECONDS,
) -> bool:
    """Stop a process group for good: ``SIGTERM``, wait, ``SIGKILL``, wait.

    Returns whether the process is actually gone.

    The escalation is the point. ``SIGTERM`` is a request, and Harbor is a
    Python process that installs handlers and can be mid-``docker compose up``
    when one arrives — so an operator's stop that sends ``SIGTERM`` and
    immediately moves on to reaping containers is racing the very process that
    creates them: the reap lists the daemon, removes what it finds, and the
    surviving harness brings the *next* trial's project up a second later, into
    a match nobody is watching any more. That container then outlives the
    match, holds memory and CPU, and costs the next match trials that are
    scored as agent losses — the #2329 shape, reached by a different road.

    Waiting is therefore not politeness, it is ordering: the caller may only
    reap once nothing is left that could still create a container.
    """
    if process.poll() is not None:
        return True
    terminate_process_group(process)
    with contextlib.suppress(subprocess.TimeoutExpired):
        process.wait(timeout=grace)
    if process.poll() is not None:
        return True
    log.warning(
        "pid %s ignored SIGTERM after %.0fs — escalating to SIGKILL", process.pid, grace
    )
    kill_process_group(process)
    with contextlib.suppress(subprocess.TimeoutExpired):
        process.wait(timeout=kill_grace)
    if process.poll() is None:
        log.error(
            "pid %s survived SIGKILL — it is blocked in the kernel, and any "
            "container it is still holding cannot be reaped reliably",
            process.pid,
        )
        return False
    return True


def stop_seat(match: Match, contestant_id: str) -> None:
    """Stop one seat's Harbor process and reap the containers it owned.

    The supervisor's ``stop``, idempotent by contract. It is
    :meth:`~.runner.MatchRunner.cancel` for one seat rather than the match, and
    it reaps for the same reason cancel does: the task containers are the Docker
    daemon's children, not the process group's, so a re-dispatch would otherwise
    inherit a host the dead round is still holding — the #2329 shape, where
    trials that cannot start are scored as agent losses.

    Scoped to this seat's own job directory: the opposing arm is still running,
    and its containers are not this seat's to remove.
    """
    run = match.runs.get(contestant_id)
    if run is None or run.process is None:
        return
    process = run.process
    if process.poll() is None:
        terminate_process_group(process)
    with contextlib.suppress(subprocess.TimeoutExpired):
        process.wait(timeout=STOP_GRACE_SECONDS)
    if process.poll() is not None and run.exit_code is None:
        run.exit_code = process.returncode
        run.finished_at = time.time()
    with contextlib.suppress(Exception):  # reaping is best-effort by contract
        reap.remove_containers(reap.trial_containers(run.job_dir))


def supervise_match(
    match: Match,
    redispatch: Callable[[Contestant, Sequence[str], str], None],
) -> reauth.MatchReauth | None:
    """A rotation supervisor for every seat running on a subscription token.

    The wiring half of #2545: :mod:`.reauth` states why a Claude Code seat dies
    mid-sweep and what to do about it; this binds one supervisor per such seat
    to the processes and directories it acts on. Only seats actually carrying
    the credential get one, and only the seat that lost its own is ever stopped
    — the opposing arm is a separate Harbor process with its own credential, and
    pausing both to fix one is itself a confound.

    ``redispatch`` is the runner's launch, already bound to the match and its
    Harbor resolution: the retry must go through the same code path as the first
    round or its provenance, seat manifest and SUT pin describe an apparatus
    that never ran.
    """
    candidates = [
        run for run in match.runs.values() if run.contestant.env.get(CLAUDE_OAUTH_ENV)
    ]
    if not candidates:
        return None
    blocked = reauth.supervision_blocked(match.spec.tasks, match.spec.attempts)
    if blocked:
        for run in candidates:
            run.warnings.append(blocked)
        return None

    seats = [
        reauth.supervise(
            contestant=run.contestant.id,
            name=run.contestant.name,
            job_dir=run.job_dir,
            tasks=match.spec.tasks,
            token=run.contestant.env[CLAUDE_OAUTH_ENV],
            read_local=local_claude_token,
            stop=lambda cid=run.contestant.id: stop_seat(match, cid),
            launch=(
                lambda tasks, fresh, c=run.contestant: redispatch(c, tasks, fresh)
            ),
        )
        # A seat that never launched has no process to stop and no credential
        # in flight to rotate.
        for run in candidates
        if not run.error and run.process is not None
    ]
    return reauth.MatchReauth(tuple(seats)) if seats else None


def tick(match: Match) -> bool:
    """Advance credential supervision. ``True`` while a seat still owes work.

    The return value is the half of the exit condition a process poll cannot
    see: a paused seat has no live process and is *not* finished, and a healing
    one sits between its stop and its re-dispatch.
    """
    supervisor = match.reauth
    if supervisor is None:
        return False

    def alive(contestant_id: str) -> bool:
        run = match.runs.get(contestant_id)
        return (
            run is not None and run.process is not None and run.process.poll() is None
        )

    pending = supervisor.tick(alive)
    status = supervisor.take_status()
    if status is not None:
        match.note = status
        if status:
            # A pause is the one thing here an operator must act on, and
            # `arenabench run` prints no snapshot field mid-match — the log is
            # the surface it does have.
            logger = log.warning if supervisor.waiting else log.info
            logger("match %s: %s", match.spec.id, status)
    return pending


def stop_match(match: Match) -> tuple[list[str], list[str]]:
    """Stop everything one match owns. Returns ``(reaped, survivors)``.

    The mechanics behind :meth:`~.runner.MatchRunner.cancel`, here for the same
    reason :func:`stop_seat` is: signalling a process group and reaping the
    containers it is not the parent of is one nameable concern, and the runner
    keeps the decision (what the match's status and note become) rather than the
    plumbing.

    **The order is the whole fix.** Signalling a seat and reaping in the same
    breath races the harness that creates the containers — Harbor can be
    mid-``docker compose up`` when the signal lands, so the sweep removes what
    exists and the surviving process brings the next trial's project up behind
    it, into a match nobody is watching any more. So every seat is stopped *and
    waited for* (:func:`stop_process_group`, which escalates to ``SIGKILL``)
    before a single container is removed.

    Then two sweeps, not one. The first covers the ordinary case; the second
    exists for the seat that survived ``SIGKILL`` and for the compose project
    that came up during the first sweep's own ``docker ps``. The second is cheap
    when the first worked — it lists the daemon, finds nothing of ours, and
    removes nothing.

    Nothing here raises. A stop that fails partway must still let the caller
    record the match cancelled, because a match believed to be running is one no
    later sweep will ever touch (see :func:`.reap.reap_finished`).
    """
    survivors: list[str] = []
    for contestant_id, run in match.runs.items():
        process = run.process
        if process is None or process.poll() is not None:
            continue
        if not stop_process_group(process):
            survivors.append(contestant_id)
            continue
        # Record the exit the poll loop is no longer around to see, so a
        # cancelled seat reads as ended rather than as still dispatched.
        if run.exit_code is None:
            run.exit_code = process.returncode
            run.finished_at = time.time()

    # Stop the observers before reaping: both attach to the containers, and a
    # recorder writing frames out of a container being `docker rm -f`ed produces
    # a truncated file with no `moov` — which `video_path` then correctly
    # refuses to serve, so the recording is lost either way and this way costs
    # no error.
    for observer in (match.recorder, match.snapshots):
        if observer is not None:
            with contextlib.suppress(Exception):
                observer.stop()

    # The process group does not contain the task containers: Harbor starts them
    # through the Docker daemon, so killing the group stops the harness and
    # leaves the containers running, owned by nothing. Ten of them accumulated
    # over three stopped matches on one host and cost the *next* match five of
    # six trials to `Docker compose command failed` — scored as agent losses
    # (#2329).
    reaped: list[str] = []
    for _pass in range(2):
        with contextlib.suppress(Exception):  # best-effort by contract
            reaped.extend(n for n in reap.reap_match(match) if n not in reaped)
    return reaped, survivors
