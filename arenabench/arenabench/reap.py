# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Task containers a stopped match leaves behind, and how they are removed.

Harbor starts each trial's container through the Docker daemon, so the
containers are **not** children of the ``harbor run`` process group. Killing
that group — which is what :meth:`~.runner.MatchRunner.cancel` does, and what a
``kill`` on the runner does — therefore stops the harness and leaves the
containers running, owned by nothing.

They are not merely untidy. Observed 2026-08-08: three matches stopped
mid-flight during setup iteration left ten orphaned containers resident. The
practical ceiling on that host was about two task containers per match, so the
*next* match's Claude Code arm lost five of its six trials to::

    RuntimeError: Docker compose command failed for ...

The arm read 1/6. Five of those trials never started, and their true cause was
a container from a match that had ended half an hour earlier — but every
projection showed an agent losing. Infrastructure must never land in an agent's
denominator (#2329), and the cheapest way to keep it out is for the
infrastructure not to be there. The belt to this module's braces is
:func:`.telemetry.is_compose_failure`, which keeps a trial that dies this way
out of ``solve_rate`` even when a container did survive something.

Two reaps, because neither alone is enough:

* **On stop** — :func:`reap_match` removes a match's own containers when it is
  cancelled or when the server is interrupted. This is the one that keeps a
  host clean in the ordinary case.
* **On start** — :func:`reap_finished` sweeps containers belonging to matches
  this workspace has *already recorded as over* before launching a new one,
  because a ``kill -9`` cannot run an exit hook and nothing else will ever
  come back for them.

**The start sweep is deliberately conservative.** It reaps only containers
whose trial id belongs to a match in this workspace that is recorded finished,
cancelled or failed. A container it does not recognise is left alone: another
arena, or an ``arenabench run`` in another process, may own it, and an arena's
view cannot see a match it did not start (#2326). Reaping a live opponent's
container would manufacture exactly the mis-scored loss this module exists to
prevent.
"""

from __future__ import annotations

import logging
import os
import subprocess
from pathlib import Path

from .snapshot import container_for_trial

__all__ = [
    "match_containers",
    "reap_finished",
    "reap_match",
    "remove_containers",
    "running_containers",
    "trial_containers",
]

log = logging.getLogger("arenabench.reap")

#: Match statuses whose containers nothing is coming back for.
OVER: frozenset[str] = frozenset({"finished", "cancelled", "failed"})

def _docker(*args: str, timeout: float = 60.0) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ)
    # The same trap the recorder and the snapshotter hit: a host-wide amd64 pin
    # makes docker look for an amd64 variant of a local arm64 image.
    env.pop("DOCKER_DEFAULT_PLATFORM", None)
    return subprocess.run(
        ["docker", *args], capture_output=True, text=True, timeout=timeout, env=env
    )


def running_containers() -> list[str]:
    """Every container name the daemon currently knows about, or ``[]``.

    ``[]`` covers every cannot-ask case identically — no daemon, no ``docker``
    on PATH, a wedged socket — because reaping is best-effort by contract: a
    host that cannot be asked is a host where nothing can be removed, and
    failing the match over it would trade a leak for an outage.
    """
    try:
        done = _docker("ps", "-a", "--format", "{{.Names}}")
    except (OSError, subprocess.SubprocessError) as exc:
        log.debug("could not list containers: %s", exc)
        return []
    if done.returncode != 0:
        log.debug("could not list containers: %s", (done.stderr or "").strip())
        return []
    return [line.strip() for line in done.stdout.splitlines() if line.strip()]


def trial_containers(job_dir: Path) -> list[str]:
    """Container names for every trial directory under one seat's job dir.

    Derived from the trial ids on disk rather than from a name pattern, which
    is what makes reaping tractable at all: a match owns exactly the containers
    its own trial ids name, so a sweep can be precise instead of guessing from
    a prefix that another match may share.
    """
    try:
        entries = sorted(p for p in job_dir.iterdir() if p.is_dir())
    except OSError:
        return []
    return [container_for_trial(entry) for entry in entries]


def match_containers(match: object) -> list[str]:
    """Every container name this match's trials could own, deduplicated.

    Attempts are **not** collapsed the way :meth:`~.runner.Match.trial_dirs`
    collapses them: each attempt is its own container, and reaping one of three
    would leave two behind.
    """
    found: list[str] = []
    for run in getattr(match, "runs", {}).values():
        for name in trial_containers(run.job_dir):
            if name not in found:
                found.append(name)
    return found


def remove_containers(names: list[str]) -> list[str]:
    """``docker rm -f`` the named containers that exist; return what went.

    Intersected with the daemon's own listing first, so the returned list is
    what was actually removed rather than what was attempted — a log line
    naming ten containers that were already gone teaches an operator to stop
    reading these lines.
    """
    if not names:
        return []
    present = set(running_containers())
    targets = [name for name in names if name in present]
    if not targets:
        return []
    try:
        done = _docker("rm", "-f", *targets, timeout=180.0)
    except (OSError, subprocess.SubprocessError) as exc:
        log.warning("could not reap %d container(s): %s", len(targets), exc)
        return []
    if done.returncode != 0:
        log.warning("could not reap containers: %s", (done.stderr or "").strip())
        return []
    return targets


def reap_match(match: object) -> list[str]:
    """Remove the containers this match owns. Best-effort; returns what went."""
    removed = remove_containers(match_containers(match))
    if removed:
        log.info(
            "reaped %d orphaned container(s) from match %s: %s",
            len(removed),
            getattr(getattr(match, "spec", None), "id", "?"),
            ", ".join(removed),
        )
    return removed


def reap_finished(matches: list[object]) -> list[str]:
    """Sweep containers belonging to matches already recorded as over.

    The ``kill -9`` safety net: an exit hook cannot run for a process that was
    not asked to stop, so the containers of a match nobody cancelled cleanly
    survive until something comes back for them. This is that something, and it
    runs before a launch because that is the moment the headroom is needed.

    Only matches in :data:`OVER` are swept. A container this workspace cannot
    account for is never touched — see the module docstring.
    """
    doomed: list[str] = []
    for match in matches:
        if getattr(match, "status", "") not in OVER:
            continue
        for name in match_containers(match):
            if name not in doomed:
                doomed.append(name)
    removed = remove_containers(doomed)
    if removed:
        log.warning(
            "reaped %d container(s) left by a match that already ended — they "
            "were holding memory and CPU the next match needs, and a trial "
            "that cannot start is scored against an agent that never ran "
            "(#2329): %s",
            len(removed),
            ", ".join(removed),
        )
    return removed
