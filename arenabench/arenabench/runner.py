# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Running a match: one Harbor process per contestant, all on the same tasks.

The execution model is deliberately boring. Each contestant gets its own
``harbor run`` subprocess, its own job directory, and its own environment; they
run concurrently and never share state. Fairness comes from all of them being
handed the identical task list from the identical pinned dataset, not from any
coordination between them.

That independence is what makes the rest possible: a contestant can be added,
crash, or be cancelled without touching the others, and the scoreboard is
simply a read over whatever job directories currently exist.

Credential handling is the one place with real care. A contestant's pasted
``.env`` becomes the environment of *its* subprocess and nothing else — never
the arena's own environment, never another contestant's, never a log line, and
never a field the HTTP API can read back. :meth:`Match.snapshot` returns key
names only.
"""

from __future__ import annotations

import contextlib
import json
import logging
import os
import subprocess
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from . import harbor, provenance
from .agents import (
    credential_env_for,
    launch_flags,
    launch_model,
    missing_credentials,
    resolve_agent,
    routes_directly,
)
from .artifacts import mp4_is_finalized
from .harbor_agent import ARENA_ENGINE_ENV
from .model import DIMENSIONS, Contestant, MatchSpec, screen_env
from .monitor import Detection, MatchWatcher
from .recorder import RecorderSupervisor
from .registry import Dataset, Registry
from .snapshot import SnapshotSupervisor
from .telemetry import (
    MetricsReader,
    TrialMetrics,
    aggregate,
    leaders,
    seat_manifest_path,
)

__all__ = ["ContestantRun", "Match", "MatchRunner"]

log = logging.getLogger("arenabench.runner")

#: Environment variables that must never be inherited by a contestant's Harbor
#: process from the arena's own environment. Every one of them is a credential
#: or a routing override that would silently make two seats identical — the
#: exact failure a head-to-head cannot survive, because it produces a clean
#: number for a contest that never happened.
_SCRUBBED_PREFIXES = ("STELLA_", "ANTHROPIC_", "OPENAI_", "OPENROUTER_", "ZAI_", "Z_AI_")
_SCRUBBED_EXACT = frozenset(
    {
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "DEEPSEEK_API_KEY",
        "MOONSHOT_API_KEY",
        "XAI_API_KEY",
        "MISTRAL_API_KEY",
        # Claude Code's subscription credential. Not caught by the
        # ANTHROPIC_ prefix above, and every bit as capable of letting an
        # ambient host login stand in for a seat that was never given one.
        "CLAUDE_CODE_OAUTH_TOKEN",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
    }
)


def _base_environment() -> dict[str, str]:
    """The arena's environment with every ambient credential removed.

    Deliberately subtractive. A contestant starts from a clean slate and gets
    exactly the keys its operator pasted, so "which credential did seat 2 use"
    always has an answer, and an ambient key on the host can never quietly
    stand in for a missing one.
    """
    env = {
        key: value
        for key, value in os.environ.items()
        if key not in _SCRUBBED_EXACT
        and not any(key.startswith(prefix) for prefix in _SCRUBBED_PREFIXES)
    }
    # Task images publish linux/amd64 only; a multi-arch base would otherwise
    # build arm64 on Apple silicon and then fail to exec the agent binary.
    env.setdefault("DOCKER_DEFAULT_PLATFORM", "linux/amd64")
    return env


def _provenance_for(match: Match) -> provenance.Provenance:
    """The apparatus this match is about to run on.

    Harbor is asked for its version here rather than trusted from config: the
    binary on PATH can change between one match and the next, and the record
    has to say what actually graded *these* trials.
    """
    from . import __version__

    try:
        version: str | None = harbor.harbor_version()
        binary = harbor.harbor_bin()
    except harbor.HarborUnavailableError:
        # The launch below will fail and say so properly. The record still
        # gets written, unknown rather than absent, so a half-started match
        # is not mistaken later for one that ran on the current Harbor.
        version, binary = None, ""
    return provenance.Provenance(
        dataset_key=match.spec.dataset,
        dataset_ref=match.dataset.harbor_id,
        dataset_digest=match.dataset.digest,
        harbor_version=version,
        harbor_bin=binary,
        arenabench_version=__version__,
        source=provenance.SOURCE_RECORDED,
    )


@dataclass
class ContestantRun:
    """One contestant's Harbor process and everything read back from it."""

    contestant: Contestant
    job_name: str
    job_dir: Path
    log_path: Path
    process: subprocess.Popen | None = None
    started_at: float | None = None
    finished_at: float | None = None
    exit_code: int | None = None
    error: str = ""
    #: Engine knobs this agent will not honour, surfaced to the operator.
    warnings: list[str] = field(default_factory=list)
    #: Things ArenaBench did *for* this seat that it was not literally asked
    #: to do — chiefly aliasing a provider-native key into the variable the
    #: agent reads. Separate from :attr:`warnings` because nothing is wrong;
    #: it is here so that no part of a contestant's routing is invisible.
    notes: list[str] = field(default_factory=list)

    @property
    def state(self) -> str:
        if self.error:
            return "error"
        if self.process is None:
            return "pending"
        if self.process.poll() is None:
            return "running"
        return "done" if self.exit_code in (0, None) else "failed"


class Match:
    """Live state of one contest."""

    def __init__(self, spec: MatchSpec, dataset: Dataset, workspace: Path) -> None:
        self.spec = spec
        self.dataset = dataset
        self.workspace = workspace
        self.jobs_root = workspace / "jobs"
        self.jobs_root.mkdir(parents=True, exist_ok=True)
        self.created_at = time.time()
        self.started_at: float | None = None
        self.finished_at: float | None = None
        self.status = "created"  # created | running | finished | cancelled | failed
        #: What this match's numbers were measured with. Set at `start`, and
        #: read back from disk for a match reconstructed after the fact.
        self.provenance: provenance.Provenance | None = None
        self.runs: dict[str, ContestantRun] = {}
        self.recorder: RecorderSupervisor | None = None
        self.snapshots: SnapshotSupervisor | None = None
        self.note = ""
        self._metrics = MetricsReader()
        # The same watcher `arenabench watch` runs, held for the match's
        # lifetime so its (agent, task, rule) dedup carries across snapshots.
        # The web UI is the product for exploring a live match; a detection
        # that only ever reached a terminal was the exact silence #1480 was
        # filed about, one surface over (#1569).
        self._watcher = MatchWatcher(self.workspace)
        self._detections: list[Detection] = []
        self._lock = threading.Lock()

    # -- reading ----------------------------------------------------------

    def trial_dirs(self, contestant_id: str) -> dict[str, Path]:
        """Task name -> trial directory, for one contestant."""
        run = self.runs.get(contestant_id)
        if run is None or not run.job_dir.is_dir():
            return {}
        found: dict[str, Path] = {}
        for entry in sorted(run.job_dir.iterdir()):
            if entry.is_dir():
                # Harbor names trials `<task>__<attempt>`; collapse attempts to
                # the task so the grid stays one row per task.
                found.setdefault(entry.name.rsplit("__", 1)[0], entry)
        return found

    def metrics_for(self, contestant_id: str) -> dict[str, TrialMetrics]:
        return {
            task: self._metrics.read(path, task)
            for task, path in self.trial_dirs(contestant_id).items()
        }

    def snapshot(self) -> dict[str, Any]:
        """The whole contest as JSON. No secrets, by construction."""
        # The monitor's view rides every snapshot. `scan` returns only
        # detections not yet reported, so accumulation happens here, under the
        # lock — several SSE streams snapshot concurrently, and a detection
        # must land in the list exactly once. The watcher attributes by
        # job-dir slug; the payload speaks contestant ids like every other
        # snapshot field.
        with self._lock:
            self._detections.extend(self._watcher.scan())
            detections = list(self._detections)
        slug_to_id = {c.slug: c.id for c in self.spec.contestants}

        by_contestant: dict[str, dict[str, TrialMetrics]] = {}
        totals: dict[str, dict[str, Any]] = {}
        for contestant in self.spec.contestants:
            metrics = self.metrics_for(contestant.id)
            by_contestant[contestant.id] = metrics
            totals[contestant.id] = aggregate(metrics.values())

        tasks = list(self.spec.tasks)
        if not tasks:
            seen: set[str] = set()
            for metrics in by_contestant.values():
                seen.update(metrics)
            tasks = sorted(seen)

        rows = [
            {
                "task": task,
                "cells": {
                    contestant.id: (
                        by_contestant[contestant.id][task].to_json()
                        if task in by_contestant[contestant.id]
                        else None
                    )
                    for contestant in self.spec.contestants
                },
            }
            for task in tasks
        ]

        return {
            "match": self.spec.to_json(),
            "dataset": self.dataset.to_json(),
            # Rides every snapshot so a viewer can never read a number without
            # the apparatus that produced it being one field away.
            "provenance": self.provenance.to_json() if self.provenance else None,
            "status": self.status,
            "note": self.note,
            "created_at": self.created_at,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "elapsed": (
                (self.finished_at or time.time()) - self.started_at
                if self.started_at
                else 0.0
            ),
            "recording": self.recorder is not None,
            "recording_active": self.recorder.active_count if self.recorder else 0,
            "snapshotting": self.snapshots is not None,
            "contestants": [
                {
                    **contestant.redacted(),
                    "state": self.runs[contestant.id].state
                    if contestant.id in self.runs
                    else "pending",
                    "warnings": self.runs[contestant.id].warnings
                    if contestant.id in self.runs
                    else [],
                    "notes": self.runs[contestant.id].notes
                    if contestant.id in self.runs
                    else [],
                    "error": self.runs[contestant.id].error
                    if contestant.id in self.runs
                    else "",
                    "totals": totals[contestant.id],
                }
                for contestant in self.spec.contestants
            ],
            "rows": rows,
            # Severity semantics are the agent monitor protocol's
            # (doc:agent-monitor-protocol): critical means this match's
            # numbers are invalid and must not be published, and the UI is
            # expected to say so rather than tint a cell. An arm whose job
            # dir matches no contestant slug is reported under its raw name —
            # an unrecognised arm is still an arm.
            "detections": [
                {
                    "contestant": slug_to_id.get(d.agent, d.agent),
                    "task": d.task,
                    "rule": d.rule,
                    "severity": d.severity,
                    "evidence": d.evidence,
                    "data": d.data,
                }
                for d in detections
            ],
            "leaders": leaders(totals, DIMENSIONS),
            "dimensions": [
                {
                    "key": d.key,
                    "label": d.label,
                    "direction": d.direction,
                    "unit": d.unit,
                    "blurb": d.blurb,
                }
                for d in DIMENSIONS
            ],
        }

    def events_path(self, contestant_id: str, task: str) -> Path | None:
        trial = self.trial_dirs(contestant_id).get(task)
        if trial is None:
            return None
        return trial / "agent" / "stella-events.jsonl"

    def video_path(self, contestant_id: str, task: str) -> Path | None:
        trial = self.trial_dirs(contestant_id).get(task)
        if trial is None:
            return None
        # Only a finalised recording is served. A file that exists but has no
        # `moov` yet cannot be played, and handing one to a browser produces a
        # broken player rather than an honest "not ready".
        video = trial / "arena" / "recording.mp4"
        return video if mp4_is_finalized(video) else None

    def trial_dir_for(self, contestant_id: str, task: str) -> Path | None:
        """The trial directory itself, for browsing everything it produced."""
        return self.trial_dirs(contestant_id).get(task)


def _no_trial_ever_ran(match: Match) -> bool:
    """True when every seat died without producing a single trial directory.

    Both halves are required. A seat that exits nonzero after real trials
    (Harbor can fail late) still leaves a contest worth reading, and a seat
    that succeeded at all makes the match a contest by definition — neither
    is a failed match, however the other seat fared.
    """
    if not match.runs:
        return True
    every_seat_died = all(
        run.state in ("error", "failed") for run in match.runs.values()
    )
    return every_seat_died and not any(
        match.trial_dirs(contestant.id) for contestant in match.spec.contestants
    )


def _seat_failure_reason(run: ContestantRun) -> str:
    """The most specific one-line account of why a seat produced nothing."""
    if run.error:
        return run.error
    try:
        lines = run.log_path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        lines = []
    # The last non-empty line: a traceback ends with its exception, and a
    # one-line refusal ("Docker daemon is not running…") is its own last line.
    for line in reversed(lines):
        if line.strip():
            return line.strip()[:300]
    return f"exited with code {run.exit_code} and wrote nothing to its log"


def _failure_note(match: Match) -> str:
    """Why the match failed, phrased for the operator, not the seat logs."""
    reasons = {
        run.contestant.name: _seat_failure_reason(run)
        for run in match.runs.values()
    }
    unique = sorted(set(reasons.values()))
    if len(unique) == 1:
        return f"no trial ever ran — every seat failed: {unique[0]}"
    per_seat = "; ".join(f"{name}: {reason}" for name, reason in reasons.items())
    return f"no trial ever ran — {per_seat}"


class MatchRunner:
    """Launches and supervises matches."""

    def __init__(self, registry: Registry, workspace: Path) -> None:
        self.registry = registry
        self.workspace = workspace
        self.matches: dict[str, Match] = {}
        self._lock = threading.Lock()

    # -- launching --------------------------------------------------------

    def create(self, spec: MatchSpec) -> Match:
        problems = spec.validate()
        if problems:
            raise ValueError("; ".join(problems))
        dataset = self.registry.get(spec.dataset)
        if dataset is None:
            raise ValueError(f"unknown dataset: {spec.dataset}")
        match = Match(spec, dataset, self.workspace / "matches" / spec.id)
        with self._lock:
            self.matches[spec.id] = match
        return match

    def start(self, match: Match) -> None:
        if match.status == "running":
            return
        match.status = "running"
        match.started_at = time.time()

        # Record the apparatus before the first trial, not after the last one:
        # a match that is killed mid-run still produced trials, and a trial
        # whose grading stack is unrecorded cannot be compared to anything
        # later (see `arenabench.provenance`).
        match.provenance = _provenance_for(match)
        provenance.write_match(match.workspace, match.provenance)

        for contestant in match.spec.contestants:
            try:
                run = self._launch(match, contestant)
            except Exception as exc:  # a bad seat must not abort the match
                log.exception("failed to launch %s", contestant.name)
                run = ContestantRun(
                    contestant=contestant,
                    job_name=f"{match.spec.id}-{contestant.slug}",
                    job_dir=match.jobs_root / f"{match.spec.id}-{contestant.slug}",
                    log_path=match.workspace / f"{contestant.slug}.log",
                    error=str(exc),
                )
            match.runs[contestant.id] = run

        if match.spec.record_video:
            supervisor = RecorderSupervisor(
                jobs_root=match.jobs_root,
                jobs=[run.job_name for run in match.runs.values()],
                contestant_by_job={
                    run.job_name: run.contestant.name for run in match.runs.values()
                },
            )
            supervisor.start()
            match.recorder = supervisor

        if match.spec.capture_snapshots:
            snapshots = SnapshotSupervisor(
                jobs_root=match.jobs_root,
                jobs=[run.job_name for run in match.runs.values()],
                interval=match.spec.snapshot_interval,
            )
            snapshots.start()
            match.snapshots = snapshots

        threading.Thread(
            target=self._await_completion, args=(match,), daemon=True
        ).start()

    def _launch(self, match: Match, contestant: Contestant) -> ContestantRun:
        spec = resolve_agent(contestant.agent)
        job_name = f"{match.spec.id}-{contestant.slug}"
        job_dir = match.jobs_root / job_name
        log_path = match.workspace / f"{contestant.slug}.log"
        log_path.parent.mkdir(parents=True, exist_ok=True)

        run = ContestantRun(
            contestant=contestant,
            job_name=job_name,
            job_dir=job_dir,
            log_path=log_path,
        )

        missing = missing_credentials(contestant)
        if missing:
            run.warnings.append(
                "no credential in this seat's env — expected one of: "
                + ", ".join(missing)
            )
        for knob in spec.unhonoured(contestant.engine):
            run.warnings.append(f"{spec.title} ignores {knob}")

        # Resolve Harbor and check it is new enough to grade THIS dataset
        # before anything is launched. A Harbor that predates a task setting
        # discards it and produces a complete run with a wrong score, so this
        # is a refusal, not a warning (see `arenabench.harbor`).
        harbor_exe = harbor.harbor_bin()
        harbor.require_for_dataset(match.dataset.min_harbor, match.dataset.title)
        run.notes.append(f"harbor {harbor.harbor_version()} ({harbor_exe})")

        # Prefer an offline export. Given a registry ref, Harbor resolves
        # every task against its backend at run time, and one failed lookup
        # raises out of the job and kills every remaining trial — for both
        # contestants at once, since they die independently but for the same
        # reason. The export carries the same bytes as the pinned digest, so
        # this costs no provenance and removes the network from the hot path.
        local_path = self.registry.local_run_path(match.spec.dataset)
        if local_path is not None:
            source = ["--path", str(local_path)]
            run.notes.append(f"tasks read offline from {local_path}")
        else:
            source = ["--dataset", match.dataset.harbor_id]
            run.warnings.append(
                "no local export: Harbor will resolve each task over the "
                "network mid-run, and one failed lookup ends the job. "
                f"`arenabench export {match.spec.dataset}` removes that."
            )

        command = [
            harbor_exe, "run",
            "--env", "docker",
            *source,
            *launch_flags(contestant),
            "--model", launch_model(contestant),
            "--job-name", job_name,
            "--jobs-dir", str(match.jobs_root),
            "--n-attempts", str(match.spec.attempts),
            "--n-concurrent", str(match.spec.concurrency),
            "--max-retries", "0",
        ]
        if match.spec.setup_timeout_multiplier != 1.0:
            command += [
                "--agent-setup-timeout-multiplier",
                str(match.spec.setup_timeout_multiplier),
            ]
        # Task names are namespaced by the *registry*, not by the task itself.
        # Filtering a ref'd dataset therefore needs `<namespace>/<task>`, while
        # an export on disk is just directories and answers to the bare name.
        # Sending the wrong form matches nothing and Harbor exits immediately —
        # loudly, at least, rather than by quietly running fewer tasks.
        for task in match.spec.tasks:
            command += [
                "--include-task-name",
                task if local_path is not None else f"{match.dataset.namespace}/{task}",
            ]

        # Screened here as well as where the seat was parsed, because this is
        # the line that actually hands a mapping to `Popen`: a seat assembled by
        # some other path must not be able to put `PATH` or `PYTHONPATH` in it.
        seat_env, ignored = screen_env(contestant.env)
        ignored = sorted({*ignored, *contestant.ignored_env})
        if ignored:
            run.warnings.append(
                "env keys ignored — a seat may carry credentials and endpoints "
                "only: " + ", ".join(ignored)
            )
        env = _base_environment()
        env.update(seat_env)
        env.update(self._agent_environment(contestant, run))

        # The seat's launch record — the only artifact that can say which
        # route this job's trials were actually served by. `launch_model`
        # hands a directly-routed seat the bare model id and every other seat
        # the qualified one, so the ids Harbor and the event streams record
        # cannot be told apart by route; pricing reads this instead (#1498).
        # Best-effort by design: a seat that cannot record its route still
        # runs, warns, and prices at the route-blind gateway rate.
        record = {
            "schema": 1,
            "contestant": contestant.id,
            "agent": contestant.agent,
            "api": contestant.engine.api,
            "model": contestant.engine.model,
            "qualified_model": contestant.engine.qualified_model,
            "launch_model": launch_model(contestant),
            "base_url": contestant.engine.base_url,
        }
        try:
            seat_manifest_path(job_dir).write_text(
                json.dumps(record, indent=2) + "\n", encoding="utf-8"
            )
        except OSError as exc:
            run.warnings.append(
                f"could not record the seat's route ({exc}); trials will "
                "price at the route-blind gateway rate"
            )

        log.info(
            "launching %s: %s (%d tasks)",
            contestant.name,
            " ".join(command[:8]),
            len(match.spec.tasks),
        )
        handle = log_path.open("wb")
        run.process = subprocess.Popen(
            command,
            stdout=handle,
            stderr=subprocess.STDOUT,
            env=env,
            cwd=str(match.workspace),
            start_new_session=True,
        )
        run.started_at = time.time()
        return run

    def _routing_environment(
        self, contestant: Contestant, run: ContestantRun
    ) -> dict[str, str]:
        """Point a Harbor built-in at a provider endpoint of the seat's choosing.

        Harbor's Claude Code agent already reads ``ANTHROPIC_BASE_URL`` and
        forwards the model name to that endpoint unchanged when one is set —
        which is why a GLM seat is possible at all. What it cannot do is guess
        that a seat declaring ``api: zai`` means its ``ZAI_API_KEY`` to be the
        bearer token, since the variable Claude Code reads has an Anthropic
        name. ArenaBench does that one translation, and records it on the seat.

        An operator who pastes the agent's own token variable is left alone:
        an explicit choice outranks an inference every time.
        """
        spec = resolve_agent(contestant.agent)
        if not routes_directly(contestant):
            return {}

        base_url = (contestant.engine.base_url or "").strip()
        env = {spec.base_url_env: base_url}  # type: ignore[dict-item]
        run.notes.append(f"routed at {base_url} via {spec.base_url_env}")

        if not spec.token_env or any(
            contestant.env.get(name) for name in spec.token_env
        ):
            return env

        for source in credential_env_for(contestant.engine.api):
            token = contestant.env.get(source)
            if token:
                env[spec.token_env[0]] = token
                run.notes.append(f"{source} supplied as {spec.token_env[0]}")
                break
        return env

    def _agent_environment(
        self, contestant: Contestant, run: ContestantRun
    ) -> dict[str, str]:
        """Agent-specific environment, layered over the operator's ``.env``."""
        spec = resolve_agent(contestant.agent)
        env: dict[str, str] = {}
        if spec.extra_env:
            env.update(spec.extra_env)
        if contestant.agent != "stella":
            env.update(self._routing_environment(contestant, run))
            return env

        # The engine config the ArenaBench Stella adapter reads back.
        env[ARENA_ENGINE_ENV] = json.dumps(contestant.engine.to_json())
        engine = contestant.engine
        if engine.budget_usd is not None:
            env["STELLA_BUDGET"] = str(engine.budget_usd)
        if engine.base_url:
            env["STELLA_BASE_URL"] = engine.base_url
        env.setdefault("STELLA_DISABLE_REFLECTION", "1")

        # Both packages must be importable by the Harbor subprocess: the
        # ArenaBench adapter and the stella_harbor base it subclasses.
        roots = [str(Path(__file__).resolve().parent.parent)]
        adapter = os.environ.get("ARENABENCH_STELLA_ADAPTER")
        if adapter:
            roots.append(str(Path(adapter).expanduser().resolve()))
        existing = os.environ.get("PYTHONPATH", "")
        if existing:
            roots.append(existing)
        env["PYTHONPATH"] = os.pathsep.join(roots)

        binary = os.environ.get("STELLA_BINARY")
        if binary:
            env["STELLA_BINARY"] = binary
        return env

    # -- supervision ------------------------------------------------------

    def _await_completion(self, match: Match) -> None:
        while True:
            alive = [
                run
                for run in match.runs.values()
                if run.process is not None and run.process.poll() is None
            ]
            if not alive:
                break
            time.sleep(2.0)

        for run in match.runs.values():
            if run.process is not None:
                run.exit_code = run.process.returncode
                run.finished_at = time.time()

        if match.recorder is not None:
            match.recorder.stop()
        if match.snapshots is not None:
            match.snapshots.stop()

        match.finished_at = time.time()
        if match.status != "cancelled":
            # "finished" is a claim that a contest happened. A match whose
            # every seat died without producing a single trial — Docker down,
            # a bad Harbor, a refused launch — proved nothing, and stamping it
            # finished puts a green pill over 0/0 trials two seconds in. That
            # match failed, and the note says why so the operator does not
            # have to open the seat logs to learn it.
            if _no_trial_ever_ran(match):
                match.status = "failed"
                match.note = _failure_note(match)
            else:
                match.status = "finished"
        log.info("match %s %s", match.spec.id, match.status)

    def cancel(self, match: Match) -> None:
        match.status = "cancelled"
        match.note = "cancelled by operator"
        for run in match.runs.values():
            process = run.process
            if process is None or process.poll() is not None:
                continue
            try:
                # The whole process group: Harbor spawns Docker and helper
                # children, and terminating only the parent leaves containers
                # running and the job directory growing after "cancel".
                os.killpg(os.getpgid(process.pid), 15)
            except (OSError, ProcessLookupError):
                with contextlib.suppress(OSError):
                    process.terminate()
        if match.recorder is not None:
            match.recorder.stop()
        if match.snapshots is not None:
            match.snapshots.stop()
