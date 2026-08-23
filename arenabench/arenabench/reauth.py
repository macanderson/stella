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

Two invariants hold throughout, and both are required:

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

:class:`MatchReauth` is the other end: one supervisor per subscription seat,
advanced from the loop :class:`~.runner.MatchRunner` already runs over the
seats' processes. It exists because *when* a supervisor ticks relative to that
loop is the whole wiring — a paused seat has no live process and is not
finished, so :meth:`MatchReauth.tick` returns whether the match still owes work
and the poll loop's exit condition reads it.
"""

from __future__ import annotations

import hashlib
import os
import time
from collections.abc import Callable, Iterable, Sequence
from dataclasses import dataclass, field, replace
from enum import StrEnum
from pathlib import Path
from typing import Any, Protocol

from .telemetry import RESULT_NAME, VOID_CREDENTIALS, MetricsReader

__all__ = [
    "PAUSE_GRACE_SECONDS",
    "CredentialDigest",
    "MatchReauth",
    "SeatState",
    "Snapshot",
    "Step",
    "SupervisedSeat",
    "Supervisor",
    "TrialView",
    "credential_digest",
    "decide",
    "is_credential_void",
    "own_login_source",
    "read_trials",
    "supervise",
    "supervision_blocked",
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
    """One trial, reduced to the facts a re-dispatch decision needs."""

    task: str
    #: ``pending`` | ``running`` | ``done`` — Harbor's own vocabulary, carried
    #: through :class:`.telemetry.TrialMetrics` unchanged.
    status: str
    #: The Harbor error class name, or ``""`` when the trial did not fail.
    failure: str = ""
    #: Harbor's trial directory name, ``<task>__<attempt>``.
    trial: str = ""
    #: When Harbor concluded this trial (``result.json``'s mtime), or ``None``
    #: while it is still in flight. Read as a *time* rather than an identity
    #: on purpose — see :attr:`superseded`.
    concluded_at: float | None = None
    #: Whether this trial predates the credential currently dispatched.
    #:
    #: A void never leaves the disk, so without this the *first* tick after a
    #: successful re-dispatch reads the old void, finds the keychain offering
    #: nothing newer than the token it just launched with, and pauses — killing
    #: the retry seconds after starting it. The trial is still counted as
    #: evidence of what has been *measured*; it is no longer evidence about
    #: whether the live credential works.
    #:
    #: Decided by :attr:`concluded_at`, never by :attr:`trial`. A re-dispatch
    #: writes into the *same job directory* (#2545), so a name-keyed generation
    #: would be resting on Harbor's attempt suffixes never colliding — an
    #: external tool's invariant, unverifiable from this repository, whose
    #: breach fails in the silent direction: a genuinely dead new credential
    #: would be filtered out as old and the seat would spend its whole retry
    #: budget re-dispatching against a token that can never work, every round
    #: reading as progress. A conclusion time is this repository's own
    #: observation, and ties resolve toward *not* superseded, so ambiguity
    #: pauses loudly instead of continuing quietly.
    superseded: bool = False

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


class SeatState(StrEnum):
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
        """Tasks whose trial died on a credential fault *under this credential*.

        Superseded trials are excluded, and the asymmetry with
        :meth:`unmeasured` is the point: a measurement is permanent, while "the
        credential is dead" is a statement about the one in flight. Counting an
        already-answered void here makes a healthy re-dispatch look like a
        fresh failure.
        """
        dead = {t.task for t in self.trials if t.credential_void and not t.superseded}
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
    #
    # The seat is deliberately **not** stopped here, unlike every other
    # terminal state. This module's authority is a credential fault; deciding
    # that a Harbor job is finished is Harbor's, and the two disagree in ways
    # that cost measurements: with ``--n-attempts > 1`` every task is measured
    # long before the sweep is over, and a job launched without an explicit
    # task list has a task set this snapshot cannot see at all. Killing a
    # healthy seat on either reading destroys exactly the trials the module
    # exists to save — and there is nothing to gain, because a job whose work
    # is done exits on its own.
    if not unmeasured:
        return Step(
            state=SeatState.COMPLETE,
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
        # that issues the launch and the kill in the same step has no point
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
            TrialView(
                task=task,
                status=metrics.status,
                failure=metrics.failure,
                trial=entry.name,
                concluded_at=_concluded_at(entry),
            )
        )
    return views


def _concluded_at(trial_dir: Path) -> float | None:
    """When Harbor wrote this trial's verdict, or ``None`` if it has not.

    Deliberately the *verdict* file rather than the directory: a directory's
    mtime moves every time anything inside it is written, so a trial still
    streaming would keep looking freshly concluded.
    """
    try:
        return (trial_dir / RESULT_NAME).stat().st_mtime
    except OSError:
        return None


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
            # Consecutive duplicates are dropped, not deduplicated globally: a
            # supervisor ticks every couple of seconds for the length of a
            # match, so an unfiltered log is tens of thousands of copies of
            # "89 task(s) outstanding" — in memory, and in every snapshot that
            # carries the record. A state that recurs *after* something else
            # happened is a real transition and is recorded again.
            line = f"[{step.state.value}] {step.reason}"
            if not self.log or self.log[-1] != line:
                self.log.append(line)
        if step.stop_seat:
            self.stop()
        if step.dispatch:
            # `token` is not None here: a dispatch step is only ever reached
            # through the `have_fresh` branch, which required a digest, and a
            # digest exists only for a non-empty token.
            assert token is not None  # invariant of `decide`
            self.launch(step.dispatch, token)
            self.note_dispatch(token)
            self._offered = None
            self.rounds_used += 1
        return step


# --------------------------------------------------------------------------
# the match-level coordinator
# --------------------------------------------------------------------------

#: How long a seat may sit :attr:`SeatState.PAUSED` before the match gives up
#: on it and reports the shortfall.
#:
#: A pause consumes no retry round, so unbounded waiting is a hang rather than
#: a measurement: the match never finishes, the *opposing* arm's numbers are
#: never published either, and an unattended sweep holds its containers until
#: someone notices. Half an hour is long enough for an operator who saw the
#: warning to run ``claude login`` — the local login is re-read on every tick,
#: so that alone resumes the sweep with no further involvement — and short
#: enough that a run nobody is watching ends with a named shortfall.
PAUSE_GRACE_SECONDS = 1800.0


def own_login_source(launched_with: str, read_local: TokenSource) -> TokenSource:
    """An auto-heal source for a seat whose credential *is* this machine's login.

    Healing means re-reading the local Claude Code login, which is only sound
    for a seat that launched on that login in the first place. A seat carrying a
    token from anywhere else — another machine, another account, a CI secret —
    would otherwise be re-dispatched onto the operator's own subscription the
    moment its own token died, and the second half of that arm would have run on
    a different plan than the first. That is not a recovery; it is a silently
    swapped variable in a controlled comparison, which is the failure this whole
    module is a defence against.

    Ownership is settled once, here, against the credential the seat launched
    with: at launch the two agree, and it is precisely their *later* divergence
    that :func:`decide` reads as a rotation. A seat that does not own the local
    login gets a source that answers ``None``, so it pauses for an operator
    rather than healing onto a stranger's plan.
    """
    if credential_digest(launched_with) != credential_digest(read_local()):
        return lambda: None
    return read_local


@dataclass
class SupervisedSeat:
    """One seat's :class:`Supervisor`, bound to the job directory it reads."""

    #: Contestant id — the key every other part of the match record uses.
    contestant: str
    #: Display name, for the one line an operator reads.
    name: str
    #: The seat's Harbor job directory. A re-dispatch writes into this *same*
    #: directory, so a retry's trial lands beside the void it replaces and
    #: :meth:`~.runner.Match.trial_dirs`'s measured-over-void rank surfaces it
    #: with no union across directories (#2545).
    job_dir: Path
    supervisor: Supervisor
    #: Monotonic stamp of when this seat entered :attr:`SeatState.PAUSED`.
    paused_since: float | None = None
    #: Wall clock at the moment this seat's current credential was dispatched,
    #: or ``None`` for a seat still on its original launch (nothing can predate
    #: that). Compared against :attr:`TrialView.concluded_at`, which is why it
    #: has to be wall clock: it is measured against a filesystem mtime.
    dispatched_at: float | None = None
    #: Why this seat is no longer supervised, or ``""`` while it still is.
    released: str = ""
    #: Whether that release left tasks unmeasured. A completed sweep and an
    #: abandoned one both stop being supervised, and a record that cannot tell
    #: them apart is the "silently 20-of-89" this module exists to prevent.
    shortfall: bool = False

    @property
    def state(self) -> SeatState:
        return self.supervisor.state

    def release(self, reason: str, *, shortfall: bool) -> None:
        """Stop supervising this seat, recording why."""
        self.released = reason
        self.shortfall = shortfall
        self.paused_since = None

    def to_json(self) -> dict[str, Any]:
        """This seat's rotation record, for the match snapshot.

        Credential-free by construction: a state name, two counters, and the
        reason strings :func:`decide` produced — which a test pins as free of
        any token.
        """
        return {
            "state": self.state.value,
            "rounds_used": self.supervisor.rounds_used,
            "max_rounds": self.supervisor.max_rounds,
            "waiting": bool(not self.released and self.state is SeatState.PAUSED),
            "released": self.released,
            "shortfall": self.shortfall,
            "log": list(self.supervisor.log),
        }


def supervision_blocked(tasks: Sequence[str], attempts: int) -> str:
    """Why this match cannot be credential-supervised, or ``""`` when it can.

    Both refusals rather than best guesses, because a supervisor whose model of
    the sweep is wrong stops seats that were working:

    * **an explicit task list** — the re-dispatch set is the sweep's tasks minus
      the ones with a measured trial, and a job launched with no
      ``--include-task-name`` filter has a task set nothing here can see;
    * **one attempt per task** — with ``--n-attempts > 1``, "this task has a
      measured trial" stops meaning "this task is finished", so a supervisor
      would both release its watch early and re-dispatch a whole fresh attempt
      set to recover one voided attempt.
    """
    if not tasks:
        return (
            "credential-rotation recovery is off for this match: it runs the "
            "whole dataset, and the supervisor cannot tell which tasks are still "
            "owed without an explicit task list"
        )
    if attempts != 1:
        return (
            f"credential-rotation recovery is off for this match: {attempts} "
            "attempts per task, and the supervisor measures completeness per task"
        )
    return ""


def supervise(
    *,
    contestant: str,
    name: str,
    job_dir: Path,
    tasks: Sequence[str],
    token: str,
    read_local: TokenSource,
    stop: Callable[[], None],
    launch: Callable[[Sequence[str], str], None],
) -> SupervisedSeat:
    """One seat's supervisor, with the credential it launched on recorded.

    ``token`` is the value the seat's Harbor process was started with. It is
    held only as a :func:`credential_digest` — the supervisor's whole question
    about a credential is "is this still the one that died", and a digest
    answers it without the module ever holding the token again.
    """
    supervisor = Supervisor(
        tasks=tuple(tasks),
        token_source=own_login_source(token, read_local),
        stop=stop,
        launch=launch,
    )
    supervisor.note_dispatch(token)
    return SupervisedSeat(
        contestant=contestant, name=name, job_dir=job_dir, supervisor=supervisor
    )


@dataclass
class MatchReauth:
    """Every supervised seat in one match, advanced by the runner's poll loop.

    Owns no thread and no timer, for the reason :class:`Supervisor` owns
    neither: the ordering between "read the trials" and "is the process still
    alive" has to be explicit, and a second thread would make it emergent.
    """

    seats: tuple[SupervisedSeat, ...]
    #: Shared across ticks so a 90-trial job directory is not re-reduced from
    #: scratch every two seconds. Deliberately **not** the match's own reader:
    #: this one is read from the supervision thread while the match's is read
    #: from every HTTP thread, and one cache mutated by both is a race nobody
    #: would ever reproduce.
    reader: MetricsReader = field(default_factory=MetricsReader)
    pause_grace: float = PAUSE_GRACE_SECONDS
    #: Elapsed time, for the pause grace. Injected so a test can expire a pause
    #: without sleeping through one.
    clock: Callable[[], float] = time.monotonic
    #: Instants comparable with a file's mtime, for
    #: :attr:`SupervisedSeat.dispatched_at`. A second clock because it answers a
    #: different question, and a monotonic reading is meaningless against a
    #: filesystem timestamp.
    wall: Callable[[], float] = time.time
    _noted: str = ""

    def tick(self, seat_running: Callable[[str], bool]) -> bool:
        """Advance every live seat. ``True`` while any still owes the match work.

        That return value is the exit condition the runner's poll loop needs. A
        paused or healing seat has **no live process and is not finished**, so a
        loop that breaks on "nothing is alive" would end the match at the exact
        instant the supervisor stopped the seat it is about to re-dispatch.
        """
        pending = False
        for seat in self.seats:
            if seat.released:
                continue
            if self._advance(seat, seat_running(seat.contestant)):
                pending = True
        return pending

    def _advance(self, seat: SupervisedSeat, running: bool) -> bool:
        since = seat.dispatched_at
        try:
            step = seat.supervisor.tick(
                (
                    replace(
                        view,
                        superseded=(
                            since is not None
                            and view.concluded_at is not None
                            and view.concluded_at < since
                        ),
                    )
                    for view in read_trials(seat.job_dir, self.reader)
                ),
                seat_running=running,
            )
        except Exception as exc:
            # A re-dispatch that cannot launch (no Harbor, a broken SUT pin, a
            # refused adapter) must not wedge the match in a supervision loop
            # that can never make progress, and must not take the *other* arm's
            # finished trials down with it. The seat stops being supervised and
            # says so; the runner has already recorded the failed round.
            seat.release(f"re-dispatch failed: {exc}", shortfall=True)
            return False

        if step.dispatch:
            # Every verdict already on disk belongs to the credential that just
            # died. Stamped after the launch returns, never before it: a
            # dispatch that raised leaves the generation untouched, and a
            # verdict written while the launch was in flight stays current.
            seat.dispatched_at = self.wall()

        if step.state is SeatState.PAUSED:
            if seat.paused_since is None:
                seat.paused_since = self.clock()
            waited = self.clock() - seat.paused_since
            if waited < self.pause_grace:
                return True
            seat.release(
                f"paused {waited / 60:.0f}m with no credential to resume on; its "
                "unmeasured tasks stay unmeasured and are a shortfall, not a result",
                shortfall=True,
            )
            return False

        seat.paused_since = None
        if step.state is SeatState.HEALING:
            return True
        if step.state is SeatState.EXHAUSTED:
            seat.release(step.reason, shortfall=True)
        elif step.state is SeatState.COMPLETE:
            seat.release(step.reason, shortfall=False)
        # RUNNING is the only remaining state, and it is not this module's
        # business: the seat is working, or it died of something a credential
        # cannot fix. Either way the runner's own process poll decides when the
        # match is over, exactly as it did before any of this existed.
        return False

    @property
    def waiting(self) -> tuple[SupervisedSeat, ...]:
        """Seats stopped and asking an operator for a credential."""
        return tuple(
            seat
            for seat in self.seats
            if not seat.released and seat.state is SeatState.PAUSED
        )

    def provide_token(self, token: str, *, contestant: str | None = None) -> list[str]:
        """Hand an operator's credential to the seats waiting for one.

        In memory, for exactly as long as the next :meth:`tick` needs to launch
        with it. :mod:`.claude_oauth`'s contract is that the value never reaches
        disk, and a resume path that wrote it to a file to cross a process
        boundary would break that contract for every seat at once.

        Only a **paused** seat takes one, and the ids that did are returned, so
        a caller reports "no seat is waiting" instead of claiming a resume that
        will not happen. A healthy seat handed a token would carry it into
        whatever it decides later, and a stale operator token is exactly the
        dead credential :func:`decide` refuses to re-dispatch on.
        """
        taken: list[str] = []
        for seat in self.waiting:
            if contestant is not None and seat.contestant != contestant:
                continue
            seat.supervisor.provide_token(token)
            taken.append(seat.contestant)
        return taken

    def to_json(self) -> dict[str, dict[str, Any]]:
        """The rotation record of every supervised seat, keyed by contestant."""
        return {seat.contestant: seat.to_json() for seat in self.seats}

    def take_status(self) -> str | None:
        """The operator-facing line, when it changed since the last call.

        ``None`` means "nothing new to say", which is what a poll loop needs to
        avoid rewriting the same note onto the match every two seconds. An empty
        string is a real answer: the condition cleared.
        """
        line = self._status_line()
        if line == self._noted:
            return None
        self._noted = line
        return line

    def _status_line(self) -> str:
        waiting = self.waiting
        if waiting:
            return (
                "credential revoked mid-match for "
                + ", ".join(seat.name for seat in waiting)
                + " — the seat is stopped and its unmeasured tasks are held. Log "
                "back in to Claude Code (the local login is re-read every few "
                "seconds) or hand over a fresh token, and the sweep resumes"
            )
        short = [seat for seat in self.seats if seat.shortfall]
        if short:
            return "; ".join(f"{seat.name}: {seat.released}" for seat in short)
        healed = [seat for seat in self.seats if seat.supervisor.rounds_used]
        if healed:
            return "; ".join(
                f"{seat.name} resumed on a fresh credential "
                f"({seat.supervisor.rounds_used} re-dispatch round(s))"
                for seat in healed
            )
        return ""
