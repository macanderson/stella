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

import json
import logging
import os
import shutil
import subprocess
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .agents import (
    credential_env_for,
    launch_flags,
    launch_model,
    missing_credentials,
    resolve_agent,
    routes_directly,
)
from .harbor_agent import ARENA_ENGINE_ENV
from .model import DIMENSIONS, Contestant, MatchSpec
from .recorder import RecorderSupervisor
from .registry import Dataset, Registry
from .telemetry import MetricsReader, TrialMetrics, aggregate, leaders

__all__ = ["Match", "MatchRunner", "ContestantRun"]

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
        self.runs: dict[str, ContestantRun] = {}
        self.recorder: RecorderSupervisor | None = None
        self.note = ""
        self._metrics = MetricsReader()
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
        video = trial / "arena" / "recording.mp4"
        return video if video.exists() else None


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

        if shutil.which("harbor") is None:
            raise RuntimeError(
                "`harbor` is not on PATH. Install it, or point ArenaBench at a "
                "virtualenv that has it."
            )

        command = [
            "harbor", "run",
            "--env", "docker",
            "--dataset", match.dataset.harbor_id,
            *launch_flags(contestant),
            "--model", launch_model(contestant),
            "--job-name", job_name,
            "--jobs-dir", str(match.jobs_root),
            "--n-attempts", str(match.spec.attempts),
            "--n-concurrent", str(match.spec.concurrency),
            "--max-retries", "0",
        ]
        for task in match.spec.tasks:
            command += ["--include-task-name", f"{match.dataset.namespace}/{task}"]

        env = _base_environment()
        env.update(contestant.env)
        env.update(self._agent_environment(contestant, run))

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

        match.finished_at = time.time()
        if match.status != "cancelled":
            match.status = "finished"
        log.info("match %s finished", match.spec.id)

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
                try:
                    process.terminate()
                except OSError:
                    pass
        if match.recorder is not None:
            match.recorder.stop()
