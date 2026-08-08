# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""When did the agent actually solve it — and what did it do afterwards.

A Terminal-Bench trial is graded **once**, after the agent process is dead,
against whatever the workspace looks like at that moment. Nothing observes the
moment the tests started passing. Three things follow, and none of them are
currently measured:

* Every step after that moment is strictly negative expected value. It cannot
  raise a reward that is already 1.0, and it costs tokens and clock.
* An agent can *destroy* its own passing solution and score 0, indistinguishable
  from never having solved the task at all.
* A trial killed by the wall clock can still pass — being interrupted is not
  the same as failing.

This module recovers the missing moment. During a run it captures periodic
workspace snapshots; afterwards :func:`replay_flip` re-runs the task's own
verifier against those snapshots to find the earliest one that passes. The
difference between that snapshot and the end of the run is the part of the
trial that could only lose.

**Why the verifier cannot simply be run during the trial.** A task's verifier
is a directory of tests that has to be copied into the container to execute.
Doing that mid-run would leave the answer key sitting in the agent's own
filesystem, readable. End-only grading is what keeps the oracle hidden, so the
replay happens after the agent is gone and cannot learn from it.

**Why snapshots are taken with a detached git dir.** ``git --git-dir`` pointed
outside the workspace with ``--work-tree`` pointed at it gives a full content
snapshot while writing **nothing** into the workspace — no ``.git``, no index,
no stray ignore file. That matters more than it sounds: ``fix-git`` is a real
task in this dataset whose whole subject *is* the repository state. A
snapshotter that ran ``git init`` in the workspace would silently rewrite the
task it was measuring. This one is invisible to the agent and to the verifier.

Capture is opt-in and degrades to nothing. No Docker, no git in the image, or a
container that vanishes mid-sweep yields a trial with no snapshots and a
warning — never a failed run. Measurement must not be able to cost a benchmark.

**Degrading to nothing is not the same as saying nothing.** That warning was
only ever emitted by the *start* side; a capture that failed after a clean
start was logged at ``debug``, and a capture that returned an empty sha was
logged nowhere at all — so a broken capture and a slow host were the same
observable, an empty manifest. Since replay reads an empty manifest as
``flip_index=None``, and an operator reads *that* as "the agent never solved
it", the silence could record a solved trial as a failure. Every capture
attempt now leaves an outcome in :class:`CaptureStatus`
(``arena/snapshots/capture.json``), a trial that captured nothing says so once
at ``error``, and the record rides into ``flip.json`` so nothing downstream has
to guess (#2196).
"""

from __future__ import annotations

import json
import logging
import os
import shutil
import subprocess
import threading
import time
from collections.abc import Callable, Iterable, Sequence
from dataclasses import dataclass, field, replace
from pathlib import Path

__all__ = [
    "SNAPSHOT_DIRNAME",
    "CaptureStatus",
    "FlipResult",
    "SnapshotEntry",
    "SnapshotSupervisor",
    "bisect_first_pass",
    "load_capture_status",
    "load_manifest",
    "read_task_image",
]

log = logging.getLogger("arenabench.snapshot")

#: Where a trial's snapshots live, beside the video the recorder writes.
SNAPSHOT_DIRNAME = "arena/snapshots"

MANIFEST_NAME = "manifest.jsonl"

#: Beside the manifest: why the manifest looks the way it does. See
#: :class:`CaptureStatus`.
CAPTURE_STATUS_NAME = "capture.json"

#: The snapshot repository lives here, inside the container but outside the
#: workspace, so the workspace is never modified. See the module docstring.
SNAP_GIT_DIR = "/tmp/.arena-snapshots.git"

#: Terminal-Bench images do not agree on where the task lives. Probed in order.
WORKSPACE_CANDIDATES: tuple[str, ...] = ("/app", "/workspace", "/home/user", "/root")

#: Never snapshot the agent's own private state. Stella writes a code-graph
#: SQLite database under `.stella/private/` that churns on essentially every
#: step; including it makes patches enormous and tells us nothing about whether
#: the task is solved. The verifier does not read it either.
SNAPSHOT_EXCLUDES: tuple[str, ...] = (".stella/", "node_modules/", "__pycache__/")

_GIT_IDENT = (
    "-c",
    "user.name=arenabench",
    "-c",
    "user.email=snapshot@arenabench.invalid",
)


# ---------------------------------------------------------------------------
# Manifest
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class SnapshotEntry:
    """One captured workspace state."""

    index: int
    #: Unix time the snapshot was committed, for aligning against the event
    #: stream. Steps are not observable from outside the agent; time is.
    at: float
    #: Commit sha inside the detached snapshot repo (container-local).
    sha: str
    #: Patch filename relative to the snapshot directory, or ``None`` when the
    #: snapshot was identical to the one before it.
    patch: str | None
    #: Seconds since the trial's first snapshot. The number an operator reads.
    elapsed: float
    #: The container directory this snapshot was taken of.
    #:
    #: Recorded rather than re-derived at replay time, because the two can
    #: disagree: capture probes a container the agent has already been living
    #: in, replay probes a fresh one from the pristine image, and a directory
    #: the agent created will exist in the first and not the second. When they
    #: disagreed the patch applied cleanly to the *wrong* directory and every
    #: probe returned "did not pass" — surfacing as `flip_index=None`, which
    #: reads as "the agent never solved it" rather than "this was misapplied".
    workspace: str = ""

    def to_json(self) -> dict[str, object]:
        return {
            "index": self.index,
            "at": self.at,
            "sha": self.sha,
            "patch": self.patch,
            "elapsed": round(self.elapsed, 3),
            "workspace": self.workspace,
        }

    @staticmethod
    def from_json(raw: dict[str, object]) -> SnapshotEntry | None:
        try:
            return SnapshotEntry(
                index=int(raw["index"]),  # type: ignore[arg-type]
                at=float(raw["at"]),  # type: ignore[arg-type]
                sha=str(raw["sha"]),
                patch=str(raw["patch"]) if raw.get("patch") else None,
                elapsed=float(raw.get("elapsed") or 0.0),  # type: ignore[arg-type]
                # Absent in manifests written before the field existed; replay
                # falls back to probing when it is empty.
                workspace=str(raw.get("workspace") or ""),
            )
        except (KeyError, TypeError, ValueError):
            return None


def load_manifest(trial_dir: Path) -> list[SnapshotEntry]:
    """Every snapshot captured for a trial, in capture order.

    A malformed line is skipped rather than fatal — the manifest is appended to
    by a live watcher and the last line can be a torn write.
    """
    path = trial_dir / SNAPSHOT_DIRNAME / MANIFEST_NAME
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return []
    entries: list[SnapshotEntry] = []
    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            raw = json.loads(line)
        except ValueError:
            continue
        entry = SnapshotEntry.from_json(raw) if isinstance(raw, dict) else None
        if entry is not None:
            entries.append(entry)
    return sorted(entries, key=lambda e: e.index)


# ---------------------------------------------------------------------------
# What capture actually did
# ---------------------------------------------------------------------------

#: The supervisor has seen the trial but capture has not begun yet.
CAPTURE_STARTING = "starting"
#: Capture is live and the last attempt landed a snapshot.
CAPTURE_CAPTURING = "capturing"
#: Capture ran until the trial (or the run) ended. The manifest is what it got.
CAPTURE_COMPLETE = "complete"
#: Capture began and then broke; :attr:`CaptureStatus.reason` says how.
CAPTURE_FAILED = "failed"
#: Capture never began — no container, no workspace directory, or no git.
CAPTURE_UNAVAILABLE = "unavailable"

#: States that mean capture is over and will not resume for this trial.
TERMINAL_CAPTURE_STATES = (CAPTURE_COMPLETE, CAPTURE_FAILED, CAPTURE_UNAVAILABLE)


@dataclass(frozen=True)
class CaptureStatus:
    """Why a trial's snapshot manifest looks the way it does.

    The manifest records what capture *achieved*, and nothing recorded what it
    *attempted*. So a short manifest — or an absent one — read identically
    whether the trial was short, the host was slow, or capture broke on its
    first attempt and then retried in silence for the rest of the run. Those
    are opposite conclusions about the same benchmark trial, and the last one
    is the dangerous one: replay reads an empty manifest as ``flip_index=None``
    and an operator reads *that* as "the agent never solved it", which converts
    a solved trial into a failure in the benchmark record (#2196).

    This is the record that separates them, written beside the manifest as
    ``arena/snapshots/capture.json`` so it survives the run and is readable by
    anything that later reads the manifest. It is deliberately a *record*, not
    an alarm: capture degrades to nothing by design (see the module docstring),
    and measurement must not be able to cost a benchmark.
    """

    #: The trial directory's name, so a record read on its own still says who.
    trial: str
    #: One of the ``CAPTURE_*`` constants above.
    state: str
    #: Snapshots that reached the manifest.
    snapshots: int
    #: Capture attempts made, landed or lost. ``attempts - snapshots`` is the
    #: measurement that was tried for and did not survive.
    attempts: int
    #: Consecutive failures at the moment this was written; zero after any
    #: attempt that landed.
    failures: int
    #: The last failure, in full. Never truncated here — the truncated half is
    #: usually the part that matters.
    reason: str
    #: Unix time this record was last written.
    at: float

    @property
    def captured_nothing(self) -> bool:
        """No snapshot ever reached the manifest, so there is nothing to replay.

        The question a downstream consumer has to ask before reading
        ``flip_index=None`` as "never solved": a trial that was never measured
        cannot answer it either way.
        """
        return self.snapshots <= 0

    @property
    def broke(self) -> bool:
        """Capture was failing when this record was last written.

        Weaker than :attr:`captured_nothing` and, for this bug, the more
        common shape: :meth:`SnapshotSupervisor._begin` lands snapshot 0 of
        the *pristine* workspace, capture then breaks, and the trial finishes
        before ``max_capture_attempts`` consecutive failures retire it. Capture
        did run to the end, so the record closes ``complete`` and
        ``captured_nothing`` is false — but the one state in the manifest is
        the unsolved one, so replay still reports ``flip_index=None`` for a
        trial the agent may well have solved. The severe case is one snapshot,
        not zero, and a check keyed on :data:`CAPTURE_FAILED` alone would miss
        it (#2196).

        :attr:`failures` is the evidence rather than :attr:`state`, because it
        resets on any attempt that lands: non-zero here means the last thing
        capture did was fail, whatever state the record was closed in.
        """
        return self.state == CAPTURE_FAILED or self.failures > 0

    def to_json(self) -> dict[str, object]:
        return {
            "trial": self.trial,
            "state": self.state,
            "snapshots": self.snapshots,
            "attempts": self.attempts,
            "failures": self.failures,
            "reason": self.reason,
            "at": self.at,
        }

    @staticmethod
    def from_json(raw: dict[str, object]) -> CaptureStatus | None:
        try:
            return CaptureStatus(
                trial=str(raw["trial"]),
                state=str(raw["state"]),
                snapshots=int(raw["snapshots"]),  # type: ignore[arg-type]
                attempts=int(raw.get("attempts") or 0),  # type: ignore[arg-type]
                failures=int(raw.get("failures") or 0),  # type: ignore[arg-type]
                reason=str(raw.get("reason") or ""),
                at=float(raw.get("at") or 0.0),  # type: ignore[arg-type]
            )
        except (KeyError, TypeError, ValueError):
            return None


def load_capture_status(trial_dir: Path) -> CaptureStatus | None:
    """The capture record for a trial, or ``None`` if there is none.

    ``None`` is not "capture was fine": it is a trial captured before this
    record existed, or one no supervisor ever looked at. Callers that need to
    know whether a flip is trustworthy must treat it as *unknown* rather than
    as consent.
    """
    path = trial_dir / SNAPSHOT_DIRNAME / CAPTURE_STATUS_NAME
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return CaptureStatus.from_json(raw) if isinstance(raw, dict) else None


def write_capture_status(trial_dir: Path, status: CaptureStatus) -> None:
    """Record what capture is doing, best-effort.

    Written whole on every change rather than appended to, because the
    interesting read is always the latest one and a torn tail would cost the
    record its meaning. A failure to write is logged and swallowed: the whole
    point of this record is a run that keeps going.
    """
    path = trial_dir / SNAPSHOT_DIRNAME / CAPTURE_STATUS_NAME
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(status.to_json(), indent=2) + "\n", encoding="utf-8")
    except OSError as exc:  # pragma: no cover - reporting must not fail a run
        log.warning("could not write the capture record for %s: %s", trial_dir.name, exc)


def read_task_image(task_dir: Path) -> str | None:
    """The docker image a task runs in, read from its ``task.toml``.

    Parsed with a deliberately small reader rather than ``tomllib`` so this
    module keeps working on a manifest written by a newer schema than the
    interpreter's parser understands — the field has been stable since 1.0 and
    a schema bump should not disable replay.
    """
    try:
        text = (task_dir / "task.toml").read_text(encoding="utf-8")
    except OSError:
        return None
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped.startswith("docker_image"):
            continue
        _, _, value = stripped.partition("=")
        return value.strip().strip("\"'") or None
    return None


# ---------------------------------------------------------------------------
# The search
# ---------------------------------------------------------------------------


def bisect_first_pass(count: int, probe: Callable[[int], bool | None]) -> int | None:
    """Index of the earliest snapshot that passes, or ``None`` if none does.

    ``probe`` returns ``True``/``False``, or ``None`` when the verifier could
    not be run at all for that snapshot — a probe that errored is treated as
    *unknown* and the search steps past it rather than reading it as a failure,
    because a mis-scored probe here silently moves the answer.

    **On monotonicity.** Binary search assumes that once the tests pass they
    keep passing, which is exactly what this function exists to question — an
    agent may break its own solution. So this is deliberately only the *cheap
    first pass*: it costs ``log2(n)`` verifier runs where a scan costs ``n``,
    and each run can take minutes. :func:`confirm_first_pass` re-probes the
    neighbourhood of the answer to catch the non-monotonic case. Callers that
    need certainty over cost should scan instead.
    """
    if count <= 0:
        return None
    lo, hi = 0, count - 1
    answer: int | None = None
    while lo <= hi:
        mid = (lo + hi) // 2
        verdict = probe(mid)
        if verdict is None:
            # Unknown: nudge forward so a broken probe cannot pin the search.
            lo = mid + 1
            continue
        if verdict:
            answer = mid
            hi = mid - 1
        else:
            lo = mid + 1
    return answer


def confirm_first_pass(
    answer: int, probe: Callable[[int], bool | None], *, window: int = 3
) -> int:
    """Walk backwards from a bisected answer while earlier snapshots also pass.

    Catches the case bisection cannot see: the agent solved the task, broke it,
    and fixed it again. Bounded by ``window`` because each step is a full
    verifier run.
    """
    earliest = answer
    for offset in range(1, window + 1):
        candidate = answer - offset
        if candidate < 0:
            break
        if probe(candidate) is not True:
            break
        earliest = candidate
    return earliest


@dataclass(frozen=True)
class FlipResult:
    """Where in a trial the reward first appeared."""

    #: Snapshot index that first passed, or ``None`` if none did.
    flip_index: int | None
    #: Seconds from the first snapshot to the flip.
    flip_elapsed: float | None
    #: Seconds the trial kept running after it was already passing.
    wasted_elapsed: float | None
    #: Total snapshots considered, and how many verifier runs it took.
    snapshots: int
    probes: int
    #: Snapshots that could not be probed at all.
    unknown: int
    #: What capture did for this trial, when a record exists (#2196).
    #:
    #: Rides along in ``flip.json`` because ``flip_index=None`` is not one
    #: finding but two: "the agent never solved it" and "capture never worked,
    #: so this trial was never measured". Reading the second as the first
    #: records a solved trial as a failure. ``None`` means the trial predates
    #: the record — unknown, never a clean bill of health.
    capture: CaptureStatus | None = None

    def to_json(self) -> dict[str, object]:
        return {
            "flip_index": self.flip_index,
            "flip_elapsed": self.flip_elapsed,
            "wasted_elapsed": self.wasted_elapsed,
            "snapshots": self.snapshots,
            "probes": self.probes,
            "unknown": self.unknown,
            "capture": self.capture.to_json() if self.capture is not None else None,
        }


def summarise_flip(
    entries: Sequence[SnapshotEntry],
    flip_index: int | None,
    *,
    probes: int,
    unknown: int,
    capture: CaptureStatus | None = None,
) -> FlipResult:
    """Turn a flip index into the durations an operator actually reads."""
    flip_elapsed: float | None = None
    wasted: float | None = None
    if flip_index is not None and 0 <= flip_index < len(entries):
        flip_elapsed = entries[flip_index].elapsed
        wasted = max(0.0, entries[-1].elapsed - flip_elapsed)
    return FlipResult(
        flip_index=flip_index,
        flip_elapsed=flip_elapsed,
        wasted_elapsed=wasted,
        snapshots=len(entries),
        probes=probes,
        unknown=unknown,
        capture=capture,
    )


# ---------------------------------------------------------------------------
# Capture
# ---------------------------------------------------------------------------


def _run(cmd: Sequence[str], *, timeout: float = 120.0) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ)
    # Same trap the recorder hits: a host-wide amd64 pin makes docker look for
    # an amd64 variant of a local arm64 image and then try to pull it.
    env.pop("DOCKER_DEFAULT_PLATFORM", None)
    return subprocess.run(
        list(cmd), capture_output=True, text=True, timeout=timeout, env=env
    )


def container_for_trial(trial_dir: Path) -> str:
    """Harbor's container name for a trial directory.

    Harbor composes it from the trial name and lowercases it, because Docker
    requires lowercase names while trial ids are mixed case.
    """
    return f"{trial_dir.name.lower()}-main-1"


def _container_running(name: str) -> bool:
    try:
        done = _run(["docker", "inspect", "-f", "{{.State.Running}}", name], timeout=30)
    except (OSError, subprocess.SubprocessError):
        return False
    return done.returncode == 0 and done.stdout.strip() == "true"


def _exec(container: str, script: str, *, timeout: float = 180.0) -> tuple[int, str]:
    try:
        done = _run(["docker", "exec", container, "sh", "-c", script], timeout=timeout)
    except (OSError, subprocess.SubprocessError) as exc:
        return 1, str(exc)
    return done.returncode, (done.stdout or "") + (done.stderr or "")


def detect_workspace(container: str) -> str | None:
    """Which candidate directory holds the task, decided inside the container."""
    probe = " ".join(
        f'[ -d "{path}" ] && echo "{path}" && exit 0;' for path in WORKSPACE_CANDIDATES
    )
    code, out = _exec(container, probe + " exit 1", timeout=60)
    if code != 0:
        return None
    found = out.strip().splitlines()
    return found[0].strip() if found else None


def _init_script(workspace: str) -> str:
    excludes = "\n".join(SNAPSHOT_EXCLUDES)
    return (
        f"command -v git >/dev/null 2>&1 || exit 3; "
        f"mkdir -p {SNAP_GIT_DIR}/info && "
        f"git --git-dir={SNAP_GIT_DIR} --work-tree={workspace} init -q && "
        f"printf '%s\\n' '{excludes}' > {SNAP_GIT_DIR}/info/exclude && echo ok"
    )


def _commit_script(workspace: str, index: int) -> str:
    ident = " ".join(_GIT_IDENT)
    base = f"git {ident} --git-dir={SNAP_GIT_DIR} --work-tree={workspace}"
    return (
        f"{base} add -A >/dev/null 2>&1; "
        f"{base} commit -q --allow-empty --no-gpg-sign -m 'arena snapshot {index}' "
        f">/dev/null 2>&1; {base} rev-parse HEAD"
    )


def _patch_script(workspace: str, first_sha: str) -> str:
    base = f"git --git-dir={SNAP_GIT_DIR} --work-tree={workspace}"
    return f"{base} diff --binary {first_sha} HEAD"


@dataclass
class _Capture:
    container: str
    workspace: str
    first_sha: str | None
    index: int
    started_at: float


@dataclass
class SnapshotSupervisor:
    """Captures periodic workspace snapshots for every live trial in a match.

    Shaped like :class:`~arenabench.recorder.RecorderSupervisor` and for the
    same reason: Harbor never announces a trial, so this watches the jobs tree
    for the files a run already produces. An ``agent/`` subdirectory means a
    trial has begun; ``result.json`` means it has ended.
    """

    jobs_root: Path
    jobs: list[str]
    #: Seconds between snapshots. The floor on how precisely a flip can be
    #: located, and the thing to turn down when a task is fast.
    interval: float = 30.0
    poll_interval: float = 2.0
    max_start_attempts: int = 3
    #: Consecutive failed captures before a trial is retired. The start side
    #: has always given up (``max_start_attempts``); the capture side used to
    #: retry forever, so a container that went away mid-trial was still paying
    #: a ``docker exec`` — and its 180 s timeout — every interval for the rest
    #: of the run. Reset by any capture that lands, so a host that merely
    #: stumbles is never retired.
    max_capture_attempts: int = 5

    _active: dict[Path, _Capture] = field(default_factory=dict, init=False)
    _finished: set[Path] = field(default_factory=set, init=False)
    _failures: dict[Path, int] = field(default_factory=dict, init=False)
    _capture_failures: dict[Path, int] = field(default_factory=dict, init=False)
    _status: dict[Path, CaptureStatus] = field(default_factory=dict, init=False)
    _finalised: set[Path] = field(default_factory=set, init=False)
    _last: dict[Path, float] = field(default_factory=dict, init=False)
    _thread: threading.Thread | None = field(default=None, init=False)
    _stop: threading.Event = field(default_factory=threading.Event, init=False)
    _lock: threading.Lock = field(default_factory=threading.Lock, init=False)

    # -- lifecycle --------------------------------------------------------

    def start(self) -> None:
        if self._thread is not None:
            return
        self._stop.clear()
        self._thread = threading.Thread(
            target=self._watch, name="arena-snapshot", daemon=True
        )
        self._thread.start()

    def stop(self, timeout: float = 60.0) -> None:
        self._stop.set()
        thread = self._thread
        if thread is not None:
            thread.join(timeout=timeout)
        self._thread = None
        with self._lock:
            pending = list(self._status)
            self._active.clear()
        # A run can end while trials are still live — an abort, the wall clock,
        # or `result.json` simply never arriving. Closing their records here is
        # what stops "capture was still going" from being read later as
        # "capture finished and found nothing".
        for trial_dir in pending:
            self._finalise(trial_dir)

    @property
    def active_count(self) -> int:
        with self._lock:
            return len(self._active)

    def unmeasured(self) -> list[CaptureStatus]:
        """Every trial whose capture produced no snapshots at all.

        The machine-readable half of the loud line :meth:`_finalise` logs. A
        caller holding this list knows which trials' ``flip_index=None`` means
        "never measured" rather than "never solved", and can refuse to score
        them instead of recording a solved trial as a failure (#2196).
        """
        with self._lock:
            statuses = list(self._status.values())
        return [s for s in statuses if s.captured_nothing]

    # -- internals --------------------------------------------------------

    def _watch(self) -> None:
        while not self._stop.is_set():
            try:
                self._sweep()
            except Exception:  # pragma: no cover - a watcher must never die
                log.exception("snapshot sweep failed")
            self._stop.wait(self.poll_interval)

    def _sweep(self) -> None:
        for job in self.jobs:
            job_dir = self.jobs_root / job
            if not job_dir.is_dir():
                continue
            for trial_dir in sorted(p for p in job_dir.iterdir() if p.is_dir()):
                self._consider(trial_dir)

    def _consider(self, trial_dir: Path) -> None:
        if trial_dir in self._finished:
            return
        finished = (trial_dir / "result.json").exists()
        with self._lock:
            capture = self._active.get(trial_dir)

        if capture is not None:
            if finished:
                # One last snapshot: the graded state is the one that matters
                # most, and the interval will rarely have landed on it.
                self._snapshot(trial_dir, capture)
                with self._lock:
                    self._active.pop(trial_dir, None)
                self._finished.add(trial_dir)
                self._finalise(trial_dir)
                return
            if time.monotonic() - self._last.get(trial_dir, 0.0) >= self.interval:
                self._snapshot(trial_dir, capture)
            return

        if finished or not (trial_dir / "agent").is_dir():
            if finished:
                self._finished.add(trial_dir)
                # Only if a record exists: a trial capture never looked at is
                # not a trial capture failed on, and inventing a record for it
                # would be its own false signal.
                self._finalise(trial_dir)
            return
        self._begin(trial_dir)

    # -- the capture record ------------------------------------------------

    def _status_for(self, trial_dir: Path) -> CaptureStatus:
        with self._lock:
            status = self._status.get(trial_dir)
        return status or CaptureStatus(
            trial=trial_dir.name,
            state=CAPTURE_STARTING,
            snapshots=0,
            attempts=0,
            failures=0,
            reason="",
            at=time.time(),
        )

    def _write_status(self, trial_dir: Path, status: CaptureStatus) -> None:
        """Update the record in memory and on disk. Never called under the lock."""
        with self._lock:
            self._status[trial_dir] = status
        write_capture_status(trial_dir, status)

    def _finalise(self, trial_dir: Path) -> None:
        """Close a trial's capture record, loudly when it measured nothing.

        A trial that captured zero snapshots is a broken measurement, not a
        quiet edge case: replay has no manifest to search, reports
        ``flip_index=None``, and an operator reads that as "the agent never
        solved it". Saying so once at ``error`` — and leaving the same fact in
        ``capture.json`` for anything reading afterwards — is the difference
        between a wrong benchmark number and a line naming the reason.

        Never raises and never aborts the run: the module's standing invariant
        is that measurement must not be able to cost a benchmark.
        """
        if trial_dir in self._finalised or trial_dir not in self._status:
            return
        self._finalised.add(trial_dir)
        status = self._status_for(trial_dir)
        if status.state in TERMINAL_CAPTURE_STATES:
            state = status.state
        elif status.snapshots:
            state = CAPTURE_COMPLETE
        elif status.state == CAPTURE_STARTING:
            state = CAPTURE_UNAVAILABLE  # capture never began
        else:
            state = CAPTURE_FAILED  # it began, and produced nothing
        final = replace(status, state=state, at=time.time())
        self._write_status(trial_dir, final)
        if final.captured_nothing:
            log.error(
                "no snapshots captured for %s (%s after %d attempt(s)): %s — a flip "
                "replay of this trial cannot tell 'never solved' from 'never "
                "measured', so do not score it as either",
                final.trial,
                final.state,
                final.attempts,
                final.reason or "no failure was recorded",
            )

    def _fail(self, trial_dir: Path, detail: str) -> None:
        count = self._failures.get(trial_dir, 0) + 1
        self._failures[trial_dir] = count
        if count == 1:
            log.warning("snapshots unavailable for %s: %s", trial_dir.name, detail)
        retired = count >= self.max_start_attempts
        if retired:
            self._finished.add(trial_dir)
        status = self._status_for(trial_dir)
        self._write_status(
            trial_dir,
            replace(
                status,
                state=CAPTURE_UNAVAILABLE if retired else CAPTURE_STARTING,
                attempts=status.attempts + 1,
                failures=count,
                reason=detail,
                at=time.time(),
            ),
        )
        if retired:
            self._finalise(trial_dir)

    def _capture_failed(self, trial_dir: Path, detail: str) -> None:
        """A capture attempt produced no snapshot. Escalate the way `_fail` does.

        Shaped deliberately like :meth:`_fail`, because the asymmetry between
        them *was* the bug: the start side escalated its first failure to
        ``warning`` and retired the trial after N attempts, while the capture
        side logged at ``debug`` and retried forever — so "capture is broken"
        and "this host is slow" were the same observable, an empty manifest and
        silence (#2196).

        Once at ``warning`` and the rest at ``debug`` is the same anti-spam
        shape ``_fail`` uses: the first failure is the diagnosis, and a wedged
        container would otherwise fill the run log with the same line every
        interval.
        """
        failures = self._capture_failures.get(trial_dir, 0) + 1
        self._capture_failures[trial_dir] = failures
        if failures == 1:
            log.warning("snapshot capture failed for %s: %s", trial_dir.name, detail)
        else:
            log.debug(
                "snapshot capture failed for %s (%d in a row): %s",
                trial_dir.name,
                failures,
                detail,
            )
        status = self._status_for(trial_dir)
        retired = failures >= self.max_capture_attempts
        self._write_status(
            trial_dir,
            replace(
                status,
                state=CAPTURE_FAILED if retired else CAPTURE_CAPTURING,
                attempts=status.attempts + 1,
                failures=failures,
                reason=detail,
                at=time.time(),
            ),
        )
        if not retired:
            return
        log.warning(
            "giving up on snapshots for %s after %d consecutive failures: %s",
            trial_dir.name,
            failures,
            detail,
        )
        self._finished.add(trial_dir)
        with self._lock:
            self._active.pop(trial_dir, None)
        self._finalise(trial_dir)

    def _captured(self, trial_dir: Path) -> None:
        """One snapshot reached the manifest: the trial is measurable again."""
        self._capture_failures.pop(trial_dir, None)
        status = self._status_for(trial_dir)
        self._write_status(
            trial_dir,
            replace(
                status,
                state=CAPTURE_CAPTURING,
                snapshots=status.snapshots + 1,
                attempts=status.attempts + 1,
                failures=0,
                at=time.time(),
            ),
        )

    def _begin(self, trial_dir: Path) -> None:
        container = container_for_trial(trial_dir)
        if not _container_running(container):
            self._fail(trial_dir, f"container {container} is not running")
            return
        workspace = detect_workspace(container)
        if workspace is None:
            self._fail(trial_dir, "no candidate workspace directory in the image")
            return
        code, out = _exec(container, _init_script(workspace), timeout=120)
        if code != 0:
            reason = "git is not installed in the task image" if code == 3 else out[-200:]
            self._fail(trial_dir, reason)
            return
        capture = _Capture(
            container=container,
            workspace=workspace,
            first_sha=None,
            index=0,
            started_at=time.time(),
        )
        with self._lock:
            self._active[trial_dir] = capture
        (trial_dir / SNAPSHOT_DIRNAME).mkdir(parents=True, exist_ok=True)
        self._snapshot(trial_dir, capture)

    def _snapshot(self, trial_dir: Path, capture: _Capture) -> None:
        self._last[trial_dir] = time.monotonic()
        code, out = _exec(
            capture.container, _commit_script(capture.workspace, capture.index)
        )
        if code != 0:
            self._capture_failed(
                trial_dir,
                f"the snapshot commit exited {code}: {out.strip()[-200:] or '(no output)'}",
            )
            return
        sha = out.strip().splitlines()[-1].strip() if out.strip() else ""
        if not sha:
            # A genuinely different fault from the branch above, and previously
            # recorded nowhere at all — not even at `debug`: the commit
            # *succeeded* and `rev-parse HEAD` still printed nothing, which is
            # what the snapshot repo disappearing out from under a live
            # container looks like. Named separately so the two cannot be
            # confused after the fact.
            self._capture_failed(
                trial_dir,
                "the snapshot commit succeeded but `git rev-parse HEAD` printed no "
                "sha — the snapshot repository is gone or unreadable",
            )
            return

        snap_dir = trial_dir / SNAPSHOT_DIRNAME
        patch_name: str | None = None
        if capture.first_sha is None:
            capture.first_sha = sha
        else:
            code, patch = _exec(
                capture.container,
                _patch_script(capture.workspace, capture.first_sha),
                timeout=300,
            )
            if code == 0 and patch.strip():
                patch_name = f"{capture.index:04d}.patch"
                try:
                    (snap_dir / patch_name).write_text(patch, encoding="utf-8")
                except OSError as exc:
                    log.debug("could not write patch for %s: %s", trial_dir.name, exc)
                    patch_name = None

        now = time.time()
        entry = SnapshotEntry(
            index=capture.index,
            at=now,
            sha=sha,
            patch=patch_name,
            elapsed=max(0.0, now - capture.started_at),
            workspace=capture.workspace,
        )
        try:
            with (snap_dir / MANIFEST_NAME).open("a", encoding="utf-8") as handle:
                handle.write(json.dumps(entry.to_json()) + "\n")
        except OSError as exc:
            # The snapshot was taken and is now unreachable: the manifest is
            # the only index into the patches. From the manifest's point of
            # view — which is the only view replay has — this attempt captured
            # nothing, so it is counted as the failure it is.
            self._capture_failed(trial_dir, f"could not append the manifest: {exc}")
        else:
            self._captured(trial_dir)
        capture.index += 1


def snapshots_available() -> bool:
    """Whether this host can capture snapshots at all."""
    return shutil.which("docker") is not None


def unpatched_indices(entries: Iterable[SnapshotEntry]) -> list[int]:
    """Snapshots with no patch — identical to the trial's first snapshot.

    Replaying one of these means replaying the pristine task state, which is
    only worth a verifier run for the very first entry.
    """
    return [e.index for e in entries if e.patch is None]
