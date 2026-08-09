# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Keep a subscription-authenticated seat alive across a long match.

A Claude Code seat runs on an OAuth access token, and Claude Code *rotates*
that credential whenever any local session refreshes — rotation **revokes** the
previous token. :mod:`.claude_oauth` reads the live one at match-creation time,
which is the correct fix for a stale launcher but says nothing about the eight
hours that follow: the token is baked into the seat's ``harbor run`` subprocess
environment at ``Popen`` time and cannot be changed in flight. So a rotation at
hour two kills every remaining trial of an 89-task sweep, and each one lands as
``AuthenticationError``.

Those trials are already *voided* rather than scored (:data:`.telemetry.VOID_CREDENTIALS`),
so the arm's solve rate is not corrupted — but a sweep that measured 20 of 89
tasks has not measured the arm, and the missing 69 are missing in the direction
that flatters whoever was still running. **A voided trial is an honest refusal
to score, not a result.** This module turns that refusal back into a
measurement by re-running the trial under a live credential.

The shape is deliberately not "retry the request". Nothing here can reach into
a running subprocess:

1. **Detect** a credential void the moment Harbor writes it, not eight hours
   later at the end of the job.
2. **Stop** the seat. Every subsequent task would fail identically, and the
   wall clock spent proving that is wall clock the re-run needs.
3. **Heal** without a human where possible — re-read the local login, because
   rotation means the *new* token is usually already sitting in the keychain.
4. **Pause** and ask only when the local login cannot supply a token that
   differs from the dead one. Waiting is correct here: guessing is what
   produced the dead arm.
5. **Re-dispatch** the voided tasks *and* the ones that never ran, then carry on.

Two invariants hold throughout, and both are load-bearing:

- **The token never touches disk and never enters a log, an argv, or an
  exception message.** Change detection runs on a salted digest with a
  per-process random salt, so even the digest is meaningless outside the run
  that made it. This extends :mod:`.claude_oauth`'s rule rather than
  reinterpreting it.
- **Only the seat that lost its credential is paused.** The opposing arm is a
  separate ``harbor run`` with its own credential and keeps running untouched,
  because pausing both to fix one is itself a confound — the arms would no
  longer have faced the same machine load.

The decision half (:func:`decide`) is a pure function over an immutable
snapshot, so every transition below is unit-testable without a keychain, a
subprocess, or a clock.
"""

from __future__ import annotations

import hashlib
import os
from collections.abc import Iterable, Sequence
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Callable, Protocol

from .telemetry import VOID_CREDENTIALS, MetricsReader

__all__ = [
    "CredentialDigest",
    "SeatState",
    "Snapshot",
    "Step",
    "Supervisor",
    "TrialView",
    "credential_digest",
    "decide",
    "is_credential_void",
    "read_trials",
]


# --------------------------------------------------------------------------
# credential identity, without the credential
# --------------------------------------------------------------------------

#: Per-process salt. A digest is only ever compared to another digest from the
#: same process, so a fresh random salt costs nothing and removes the last way
#: a persisted or logged digest could be tested against a guessed token.
_SALT = os.urandom(16)

#: Opaque, comparable stand-in for a token. Never reversible to the token.
CredentialDigest = str


def credential_digest(token: str | None) -> CredentialDigest | None:
    """A comparable identity for ``token``, or ``None`` when there is none.

    Equality of two digests means "the same token"; that is the only question
    this module ever asks of a credential. An empty or whitespace-only token
    is ``None`` rather than a digest of the empty string, because "the
    keychain answered with nothing" and "the keychain answered" are different
    states and the caller branches on which.
    """
    if token is None:
        return None
    stripped = token.strip()
    if not stripped:
        return None
    return hashlib.blake2b(_SALT + stripped.encode("utf-8"), digest_size=16).hexdigest()


def is_credential_void(failure: str) -> bool:
    """Whether ``failure`` names a credential fault rather than an agent one.

    Delegates to :data:`.telemetry.VOID_CREDENTIALS` rather than restating the
    error names: that set is the reviewed taxonomy and is already pinned by a
    test asserting the void buckets partition
    :data:`.telemetry.INFRASTRUCTURE_FAILURES`. A second copy here would be a
    second thing to forget when a provider invents an error class.
    """
    return failure in VOID_CREDENTIALS


# --------------------------------------------------------------------------
# the snapshot the decision reads
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class TrialView:
    """One trial, reduced to the three facts a re-dispatch decision needs."""

    task: str
    #: ``pending`` | ``running`` | ``done`` — Harbor's own vocabulary, carried
    #: through :class:`.telemetry.TrialMetrics` unchanged.
    status: str
    #: The Harbor error class name, or ``""`` when the trial did not fail.
    failure: str = ""

    @property
    def credential_void(self) -> bool:
        return is_credential_void(self.failure)

    @property
    def measured(self) -> bool:
        """Whether this trial produced a result worth keeping.

        A finished trial that failed on credentials is *not* measured — that
        is the whole premise. A finished trial that failed any other way is:
        a timeout, a crash, and a wrong answer are all real things the agent
        did, and re-running them to get a nicer number is the exact dishonesty
        this module must not enable.
        """
        return self.status == "done" and not self.credential_void


class SeatState(str, Enum):
    """Where a supervised seat is in the rotation-recovery cycle."""

    #: Dispatched and believed healthy.
    RUNNING = "running"
    #: A credential void was seen; the seat is being stopped and a live token
    #: looked for. No human is involved yet.
    HEALING = "healing"
    #: The local login cannot supply a token that differs from the dead one.
    #: Waiting for an operator. Nothing is dispatched in this state.
    PAUSED = "paused"
    #: Every task has a measured result.
    COMPLETE = "complete"
    #: The retry budget is gone. Whatever is still unmeasured stays unmeasured
    #: and is reported as such.
    EXHAUSTED = "exhausted"


@dataclass(frozen=True)
class Snapshot:
    """Everything :func:`decide` is allowed to look at.

    Frozen and self-contained on purpose: the decision must be reproducible
    from a record, so that a supervisor's behaviour during a real run can be
    replayed from the ledger it wrote rather than argued about.
    """

    #: Every task the seat is meant to measure, in dispatch order.
    tasks: tuple[str, ...]
    #: Trials observed so far. Tasks absent from here have not started.
    trials: tuple[TrialView, ...]
    #: Identity of the token the *currently dispatched* seat was launched with.
    dispatched_digest: CredentialDigest | None
    #: Identity of the best token available right now — from the local login,
    #: or one an operator handed over. ``None`` when no token can be had.
    available_digest: CredentialDigest | None
    #: Re-dispatch rounds already spent.
    rounds_used: int = 0
    #: Re-dispatch rounds permitted. A cap, not a target: an unfixable auth
    #: problem must terminate rather than spin, and the honest end state is
    #: :attr:`SeatState.EXHAUSTED` with the shortfall named.
    max_rounds: int = 8
    #: Whether the seat's Harbor process is still alive.
    seat_running: bool = True

    def unmeasured(self) -> tuple[str, ...]:
        """Tasks with no result worth keeping, in dispatch order.

        The re-dispatch set. It is deliberately *not* just the voided tasks:
        when a token dies mid-sweep the tasks that never got a container are
        unmeasured for the same reason, and leaving them out is how a re-run
        converges on a partial sweep that looks complete.
        """
        measured = {t.task for t in self.trials if t.measured}
        return tuple(task for task in self.tasks if task not in measured)

    def voided(self) -> tuple[str, ...]:
        """Tasks whose trial died specifically on a credential fault."""
        dead = {t.task for t in self.trials if t.credential_void}
        return tuple(task for task in self.tasks if task in dead)


@dataclass(frozen=True)
class Step:
    """What the supervisor should do next, and why."""

    state: SeatState
    #: Tasks to dispatch now. Non-empty only when the supervisor should launch.
    dispatch: tuple[str, ...] = ()
    #: Whether the currently running seat process must be stopped first.
    stop_seat: bool = False
    #: One line an operator can act on. Never contains a credential.
    reason: str = ""

    @property
    def needs_operator(self) -> bool:
        return self.state is SeatState.PAUSED


# --------------------------------------------------------------------------
# the decision
# --------------------------------------------------------------------------


def decide(snap: Snapshot) -> Step:
    """The next :class:`Step` for a supervised seat. Pure.

    Ordered by what must be true rather than by what is most common: a
    completed sweep is checked before the retry budget so a run that finishes
    on its last permitted round reports ``COMPLETE`` rather than
    ``EXHAUSTED``, and the budget is checked before healing so an exhausted
    supervisor never asks an operator for a token it has no round left to use.
    """
    unmeasured = snap.unmeasured()

    # Nothing left to measure. True even with voids on the record: a task
    # re-run to a real result is measured, and the void is history.
    if not unmeasured:
        return Step(
            state=SeatState.COMPLETE,
            stop_seat=snap.seat_running,
            reason=f"all {len(snap.tasks)} tasks measured",
        )

    voided = snap.voided()

    # No credential fault: the seat is simply still working. Anything
    # unmeasured is in flight or queued, and neither is this module's business.
    if not voided:
        return Step(
            state=SeatState.RUNNING,
            reason=f"{len(unmeasured)} task(s) outstanding, no credential fault",
        )

    if snap.rounds_used >= snap.max_rounds:
        return Step(
            state=SeatState.EXHAUSTED,
            stop_seat=snap.seat_running,
            reason=(
                f"credential retry budget spent ({snap.rounds_used}/"
                f"{snap.max_rounds}); {len(unmeasured)} task(s) stay unmeasured "
                "and must be reported as a shortfall, not scored"
            ),
        )

    # A live token is one that exists and is not the token that just died.
    # "Exists" alone is not enough and the difference is the whole mechanism:
    # re-reading the keychain before the local CLI has refreshed hands back
    # the revoked value, and re-dispatching on it burns a round to reproduce
    # the same failure.
    have_fresh = (
        snap.available_digest is not None
        and snap.available_digest != snap.dispatched_digest
    )

    if not have_fresh:
        return Step(
            state=SeatState.PAUSED,
            stop_seat=snap.seat_running,
            reason=(
                f"credential revoked mid-match; {len(voided)} trial(s) voided and "
                f"{len(unmeasured)} task(s) unmeasured. The local Claude Code login "
                "offers no different token — log back in, or hand over a fresh one, "
                "and the sweep resumes from here"
            ),
        )

    if snap.seat_running:
        # Stop before dispatching, never both in one step. Two Harbor jobs
        # writing the same job directory is a corrupted arm, and a supervisor
        # that issues the launch in the same breath as the kill has no point
        # at which it can confirm the old process is gone.
        return Step(
            state=SeatState.HEALING,
            stop_seat=True,
            reason=(
                f"credential revoked; stopping the seat before re-dispatching "
                f"{len(unmeasured)} task(s) on a fresh token"
            ),
        )

    return Step(
        state=SeatState.HEALING,
        dispatch=unmeasured,
        reason=(
            f"re-dispatching {len(unmeasured)} unmeasured task(s) on a fresh "
            f"token (round {snap.rounds_used + 1}/{snap.max_rounds})"
        ),
    )


# --------------------------------------------------------------------------
# the I/O shell
# --------------------------------------------------------------------------


def read_trials(job_dir: Path, reader: MetricsReader | None = None) -> list[TrialView]:
    """Every trial under ``job_dir``, as the supervisor sees it.

    Attempts are **not** collapsed. :class:`.monitor.MatchWatcher` collapses
    them because a dashboard shows one row per task, but the re-dispatch
    question is "did *any* attempt measure this task", and folding three
    attempts into whichever sorted first would re-run a task already measured
    by its second attempt — or worse, skip one whose only measured attempt was
    not the one kept.

    Returns an empty list for a job directory that does not exist yet: a seat
    is supervised from the moment it launches, which is before Harbor has
    created anything.
    """
    reader = reader or MetricsReader()
    if not job_dir.is_dir():
        return []
    views: list[TrialView] = []
    for entry in sorted(job_dir.iterdir()):
        if not entry.is_dir():
            continue
        task = entry.name.rsplit("__", 1)[0]
        metrics = reader.read(entry, task)
        views.append(
            TrialView(task=task, status=metrics.status, failure=metrics.failure)
        )
    return views


class TokenSource(Protocol):
    """Where a live credential comes from. Implemented by the keychain reader."""

    def __call__(self) -> str | None: ...


@dataclass
class Supervisor:
    """Drives :func:`decide` against a real seat.

    Deliberately owns no threads and no clock: :meth:`tick` is called by
    whatever loop is already polling the match, which keeps the supervisor
    testable and keeps its ordering relative to the monitor's own scan
    explicit rather than emergent.
    """

    tasks: tuple[str, ...]
    #: Reads the local login. Defaults to the keychain reader, injected here so
    #: a test never needs one.
    token_source: TokenSource
    #: Stops the seat's Harbor process. Must be idempotent.
    stop: Callable[[], None]
    #: Launches a fresh Harbor job for exactly these tasks with this token.
    #: The token is passed in memory and must not be logged by the callee.
    launch: Callable[[Sequence[str], str], None]
    max_rounds: int = 8

    state: SeatState = SeatState.RUNNING
    rounds_used: int = 0
    _dispatched: CredentialDigest | None = None
    _offered: str | None = field(default=None, repr=False)
    #: Every step taken, for the run record. Reasons only — no credentials.
    log: list[str] = field(default_factory=list)

    def note_dispatch(self, token: str) -> None:
        """Record the credential the currently running seat was launched with."""
        self._dispatched = credential_digest(token)

    def provide_token(self, token: str) -> None:
        """Hand the supervisor a credential an operator supplied.

        Held in memory for exactly as long as it takes the next :meth:`tick` to
        launch with it. Preferred over asking the operator to re-login when the
        token came from somewhere other than this machine's Claude Code.
        """
        self._offered = token.strip() or None

    def _available(self) -> tuple[str | None, CredentialDigest | None]:
        """The best token on offer right now, and its identity.

        An operator-supplied token wins over the keychain: they supplied it
        *because* the automatic path had nothing better, and silently
        preferring the keychain would strand a paused run holding the answer.
        """
        token = self._offered or self.token_source()
        return token, credential_digest(token)

    def tick(self, trials: Iterable[TrialView], *, seat_running: bool) -> Step:
        """Advance one step. Returns what was decided, after acting on it."""
        token, available = self._available()
        step = decide(
            Snapshot(
                tasks=self.tasks,
                trials=tuple(trials),
                dispatched_digest=self._dispatched,
                available_digest=available,
                rounds_used=self.rounds_used,
                max_rounds=self.max_rounds,
                seat_running=seat_running,
            )
        )
        self.state = step.state
        if step.reason:
            self.log.append(f"[{step.state.value}] {step.reason}")
        if step.stop_seat:
            self.stop()
        if step.dispatch:
            # `token` is not None here: a dispatch step is only ever reached
            # through the `have_fresh` branch, which required a digest, and a
            # digest exists only for a non-empty token.
            assert token is not None  # noqa: S101 - invariant of `decide`
            self.launch(step.dispatch, token)
            self.note_dispatch(token)
            self._offered = None
            self.rounds_used += 1
        return step
