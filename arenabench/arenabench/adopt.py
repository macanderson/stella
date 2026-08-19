# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""What a match found on disk says about itself.

``MatchRunner.restore_from_disk`` adopts every match directory this process did
not launch, and ``ArenaServer._refresh_from_disk`` calls it behind every listing
request. An adopted match has no process to poll and no supervisor watching it,
so its status has to be *derived* from artifacts rather than observed — which is
a nameable concern with its own failure modes, and the reason this is a sibling
of :mod:`.runner` rather than a section of it, exactly as :mod:`.reap` and
:mod:`.reauth_wiring` are.

Adoption used to stamp ``status = "finished"`` unconditionally. The cosmetic
half of that was a green pill over a contest still running. The expensive half
is that :func:`.reap.reap_finished` sweeps the containers of any match recorded
over — so a live match wrongly believed finished has its **running trial
containers removed** by the next launch, which is the mis-scored loss of #2329
and #2326 reached a third way, this time manufactured by the arena's own
bookkeeping.

The dependency runs one way: :mod:`.runner` imports this, and this never imports
:mod:`.runner`. What it needs of a match it takes as a parameter and reads by
attribute, which is also what keeps every function here drivable from a test
with no Harbor, no Docker and no containers.
"""

from __future__ import annotations

import logging
import time
from pathlib import Path
from typing import TYPE_CHECKING

from . import reap

log = logging.getLogger("arenabench.adopt")

if TYPE_CHECKING:  # pragma: no cover - types only, and importing it is a cycle
    from .runner import Match

__all__ = [
    "STALE_AFTER_SECONDS",
    "last_write",
    "refresh",
    "settled_trials",
    "status_for",
]

#: How long an adopted match's artifacts may go unwritten before this process
#: stops believing anything is still driving it.
#:
#: Generous on purpose, because the cost is asymmetric. Too short and a live
#: match between trials — pulling an image, waiting on a model call that writes
#: nothing — is declared dead, and the next launch reaps its containers. Too
#: long and a genuinely abandoned match holds a listing row reading "running"
#: for a while longer, which costs nobody a trial.
STALE_AFTER_SECONDS = 15 * 60.0

def settled_trials(match: Match) -> tuple[int, int]:
    """``(settled, expected)`` trials for a match, read off its job directories.

    *Settled* means a ``result.json`` exists — Harbor writes it once, when the
    trial is over, whatever the outcome — so this counts trials that reached an
    end, never trials that reached a *good* end. *Expected* is one per declared
    task per seat.

    Counted through :meth:`Match.trial_dirs`, so the several attempts of one
    task collapse the same way the scoreboard collapses them: with
    ``--n-attempts > 1`` a task is settled when its representative attempt is,
    and counting raw directories would make ``expected`` unknowable from the
    spec alone.
    """
    settled = 0
    expected = 0
    for contestant in match.spec.contestants:
        trials = match.trial_dirs(contestant.id)
        tasks = set(match.spec.tasks) or set(trials)
        expected += len(tasks)
        settled += sum(
            1
            for task in tasks
            if task in trials and (trials[task] / "result.json").is_file()
        )
    return settled, expected


def last_write(match: Match) -> float:
    """Epoch seconds of the most recent write anywhere in this match, or 0.

    Deliberately not a walk of the whole directory: a finished match holds
    thousands of files and this runs behind every listing request. It stats
    only the files a *live* match is necessarily touching — each seat's
    launcher log, and each trial's event stream and result — which is O(trials)
    and bounded by the task count.
    """
    newest = 0.0
    candidates: list[Path] = list(match.workspace.glob("*.log"))
    for run in match.runs.values():
        if not run.job_dir.is_dir():
            continue
        try:
            trials = [p for p in run.job_dir.iterdir() if p.is_dir()]
        except OSError:
            continue
        for trial in trials:
            candidates.append(trial / "agent" / "stella-events.jsonl")
            candidates.append(trial / "result.json")
    for path in candidates:
        try:
            newest = max(newest, path.stat().st_mtime)
        except OSError:
            continue
    return newest


def status_for(match: Match) -> tuple[str, str]:
    """``(status, note)`` for a match this process found rather than ran.

    Three questions, asked strongest-evidence first:

    1. **Is a container of this match running?** That is an observation,
       not an inference, and it settles the matter — something is
       executing a trial right now.
    2. **Did every declared task produce a result?** If so the contest is
       over and the artifacts are complete.
    3. **Has anything been written recently?** A match with unsettled
       trials is either still working or was abandoned mid-flight, and
       the only thing on disk that separates those is whether the files
       are still moving.

    An abandoned match — unsettled and quiet — is ``finished`` with a note
    saying how many trials never produced a result, **not** ``failed``.
    That is deliberately the codebase's own existing bar rather than a
    stricter one invented here: :meth:`.runner.MatchRunner._settle` stamps ``failed`` only when
    *no* trial ever ran, and calling a match failed because 3 of 88 trials
    were wedged would relabel most of a real archive. A partial match is a
    contest that happened and came up short in places; the note is where
    that is said, and the note is what an operator reads. So the two
    outcomes here are the same two ``_settle`` reaches, decided the same
    way — which is what keeps a match's status meaning one thing whether
    this process ran it or found it.
    """
    if reap.match_is_live(match):
        return "running", (
            "adopted from artifacts on disk; a container of this match is "
            "running, so it is still in flight"
        )
    settled, expected = settled_trials(match)
    if settled >= expected:
        return "finished", "restored from artifacts on disk after a server restart"
    quiet_for = time.time() - last_write(match)
    missing = expected - settled
    if quiet_for < STALE_AFTER_SECONDS:
        return "running", (
            f"adopted from artifacts on disk; {missing} of {expected} trials "
            "have not produced a result yet and the files are still being "
            "written"
        )
    quiet = f"{quiet_for / 60:.0f} minutes"
    if settled == 0:
        # `_settle`'s rule, reached from the other direction: no trial ever
        # produced a result, so no contest happened and "finished" would
        # put a green pill over 0/N.
        return "failed", (
            f"adopted from artifacts on disk; none of its {expected} trials "
            f"produced a result and nothing has been written for {quiet} — "
            "the process that owned this match is gone"
        )
    return "finished", (
        f"adopted from artifacts on disk; {missing} of {expected} trials "
        f"never produced a result and nothing has been written for {quiet} — "
        "the process that owned this match is gone, so it will not finish them"
    )


def refresh(match: Match) -> bool:
    """Re-derive an adopted match's status from what is on disk now.

    Returns whether anything moved. Called on every disk sweep for adopted
    matches only: a match this process launched has a poll loop that owns its
    status, and re-deriving that from file timestamps would fight the
    supervisor — and lose, because a match between trials writes nothing.
    """
    status, note = status_for(match)
    if status == match.status:
        return False
    match.status = status
    match.note = note
    match.finished_at = last_write(match) if status in reap.OVER else None
    log.info("adopted match %s is now %s", match.spec.id, status)
    return True
