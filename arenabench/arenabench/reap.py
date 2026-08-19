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
cancelled or failed, *and* none of whose containers is currently running. A
container it does not recognise is left alone: another arena, or an
``arenabench run`` in another process, may own it, and an arena's view cannot
see a match it did not start (#2326). Reaping a live opponent's container would
manufacture exactly the mis-scored loss this module exists to prevent.

**A match owns a compose project, not a container name.** Harbor brings each
trial up as its own project named after the trial, and
:func:`~.snapshot.container_for_trial` can only name that project's ``main``
service. A task with a sidecar owns ``<trial>-db-1`` as well, and a project
that came up in the seconds before a stop owns containers whose trial
directory was never written — neither is reachable by name. So
:func:`match_containers` unions the derivable names with everything the daemon
reports under this match's projects, and :func:`match_is_live` asks the same
question of liveness.
"""

from __future__ import annotations

import logging
import os
import subprocess
from dataclasses import dataclass
from pathlib import Path

from .snapshot import container_for_trial

__all__ = [
    "Container",
    "list_containers",
    "match_containers",
    "match_is_live",
    "match_projects",
    "reap_finished",
    "reap_match",
    "remove_containers",
    "running_containers",
    "trial_containers",
    "trial_projects",
]

log = logging.getLogger("arenabench.reap")

#: Match statuses whose containers nothing is coming back for.
OVER: frozenset[str] = frozenset({"finished", "cancelled", "failed"})

#: The compose label naming the project a container belongs to. Harbor brings
#: each trial up as its own compose project named after the trial, so this is
#: the join key from a trial directory to *every* container that trial owns —
#: not just the one service :func:`~.snapshot.container_for_trial` can name.
COMPOSE_PROJECT_LABEL = "com.docker.compose.project"

#: Separates the fields of one ``docker ps`` row. A tab, because a container
#: name may not contain one and a compose project name may not either, while
#: both may contain the characters a comma or a space would split on.
_FIELD_SEP = "\t"


@dataclass(frozen=True)
class Container:
    """One container the daemon knows about, as a reap decision needs it."""

    name: str
    #: The ``com.docker.compose.project`` label, or ``""`` when unlabelled.
    project: str
    #: Whether it is running *now*, as opposed to merely existing. The
    #: distinction is the whole safety interlock in :func:`match_is_live`:
    #: an exited container is residue, a running one is somebody's trial.
    running: bool


def _docker(*args: str, timeout: float = 60.0) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ)
    # The same trap the recorder and the snapshotter hit: a host-wide amd64 pin
    # makes docker look for an amd64 variant of a local arm64 image.
    env.pop("DOCKER_DEFAULT_PLATFORM", None)
    return subprocess.run(
        ["docker", *args], capture_output=True, text=True, timeout=timeout, env=env
    )


def list_containers() -> list[Container]:
    """Every container the daemon knows about, with its project and liveness.

    ``[]`` covers every cannot-ask case identically — no daemon, no ``docker``
    on PATH, a wedged socket — because reaping is best-effort by contract: a
    host that cannot be asked is a host where nothing can be removed, and
    failing the match over it would trade a leak for an outage.

    One ``docker ps`` for all three fields rather than a call per question:
    the caller that needs liveness (:func:`match_is_live`) runs on the disk
    sweep behind every listing request, and asking the daemon once per
    container there would put a subprocess storm behind a page refresh.

    A row this cannot parse is skipped rather than guessed at. The format
    string is ours, so a short row means a daemon speaking a shape we do not
    know, and inventing a project label for it is how a live opponent's
    container gets reaped.
    """
    fields = _FIELD_SEP.join(
        ("{{.Names}}", f"{{{{.Label \"{COMPOSE_PROJECT_LABEL}\"}}}}", "{{.State}}")
    )
    try:
        done = _docker("ps", "-a", "--format", fields)
    except (OSError, subprocess.SubprocessError) as exc:
        log.debug("could not list containers: %s", exc)
        return []
    if done.returncode != 0:
        log.debug("could not list containers: %s", (done.stderr or "").strip())
        return []
    found: list[Container] = []
    for line in done.stdout.splitlines():
        parts = line.split(_FIELD_SEP)
        if len(parts) != 3 or not parts[0].strip():
            continue
        name, project, state = (part.strip() for part in parts)
        found.append(Container(name=name, project=project, running=state == "running"))
    return found


def running_containers() -> list[str]:
    """Every container name the daemon currently knows about, or ``[]``.

    Named for the ``docker ps`` it wraps rather than for what it returns:
    the listing is ``-a``, so an *exited* container is in it. That is the
    behaviour :func:`remove_containers` needs — ``docker rm -f`` targets a
    container that exists, running or not — and changing it would silently
    stop reaping the residue this module exists to remove. When the question
    is whether a container is alive, ask :func:`list_containers`.
    """
    return [container.name for container in list_containers()]


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


def trial_projects(job_dir: Path) -> list[str]:
    """Compose project names for every trial directory under one seat's job dir.

    Harbor derives the project from the trial id, lowercased, exactly as
    :func:`~.snapshot.container_for_trial` derives the ``-main-1`` name from
    it — that name *is* ``<project>-<service>-<index>``. Taking the project
    rather than the composed name is what makes a sweep cover the services
    beside ``main``: a task with a database sidecar brings up
    ``<trial>-db-1`` too, and a reap that only ever named ``main`` left it
    resident (the #2329 shape, one service over).
    """
    try:
        entries = sorted(p for p in job_dir.iterdir() if p.is_dir())
    except OSError:
        return []
    return [entry.name.lower() for entry in entries]


def match_projects(match: object) -> list[str]:
    """Every compose project this match's trials could own, deduplicated."""
    found: list[str] = []
    for run in getattr(match, "runs", {}).values():
        for project in trial_projects(run.job_dir):
            if project not in found:
                found.append(project)
    return found


def match_containers(match: object) -> list[str]:
    """Every container name this match's trials could own, deduplicated.

    Two sources, unioned, because neither alone is the set:

    * the ``-main-1`` name each trial directory *names*, which is derivable
      with no daemon at all and so still works on a host that cannot be asked;
    * every container the daemon reports under one of this match's compose
      projects, which is the only way to reach a service other than ``main``
      and the only way to reach a container whose trial directory does not
      exist yet — a compose project that came up moments before the stop and
      whose trial dir the harness never got to write.

    Attempts are **not** collapsed the way :meth:`~.runner.Match.trial_dirs`
    collapses them: each attempt is its own container, and reaping one of three
    would leave two behind.
    """
    found: list[str] = []
    for run in getattr(match, "runs", {}).values():
        for name in trial_containers(run.job_dir):
            if name not in found:
                found.append(name)
    projects = set(match_projects(match))
    if projects:
        for container in list_containers():
            if container.project in projects and container.name not in found:
                found.append(container.name)
    return found


def match_is_live(match: object) -> bool:
    """Whether any container of this match's compose projects is running now.

    The interlock between "this match is over" and "these containers may be
    removed". A match this process does not supervise — one ``arenabench run``
    started elsewhere, or one that outlived an arena restart — has no process
    to poll, and every other signal about it is a guess from file timestamps.
    A running container is not a guess: something is executing this match's
    trial right now, and both :func:`reap_finished` and the status a listing
    reports have to respect that or they manufacture the mis-scored loss this
    module exists to prevent (#2326, #2329).

    ``False`` when the daemon cannot be asked, which is the safe direction
    here and only here: it can cost a sweep, never a live trial, because
    :func:`reap_finished` still refuses to touch a match not recorded
    :data:`OVER`.
    """
    projects = set(match_projects(match))
    if not projects:
        return False
    return any(
        container.running and container.project in projects
        for container in list_containers()
    )


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
        # Belt to the status's braces. A status is this process's *belief*
        # about a match, and for a match restored from disk it is derived
        # rather than observed — so a match wrongly believed over would have
        # its live containers swept out from under it here. A running
        # container refuses that, whatever the status says.
        if match_is_live(match):
            log.info(
                "match %s is recorded over but still owns a running container — "
                "leaving it alone",
                getattr(getattr(match, "spec", None), "id", "?"),
            )
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
