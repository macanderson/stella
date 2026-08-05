# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Re-run a task's own verifier against a trial's snapshots, after the fact.

The trial is over and its container is gone. What survives is a series of
patches against the pristine task state (see :mod:`arenabench.snapshot`), so a
snapshot is replayed by starting a fresh container from the task's image,
applying its patch, and running the task's real verifier — the same
``/tests/test.sh`` Harbor runs, reading the same ``/logs/verifier/reward.txt``.
Nothing here reimplements grading; it relocates it in time.

**This is expensive and the search is shaped around that.** One probe pays the
verifier's full setup — for ``sqlite-with-gcov`` that measured ~142 seconds,
most of it ``apt-get`` and a ``uv`` install. Probing forty snapshots serially
would take longer than the trial did. So the flip is found by bisection
(``log2(n)`` probes), with a short confirming walk backwards to catch an agent
that solved the task, broke it, and fixed it again — the one case bisection
cannot see.

**Replay is never run inside a live trial.** The verifier's test files would be
visible to the agent, which would hand it the answer key. Everything here runs
against containers the agent has no access to, after it is dead.
"""

from __future__ import annotations

import json
import logging
import os
import shutil
import subprocess
import uuid
from dataclasses import dataclass
from pathlib import Path

from .snapshot import (
    FlipResult,
    SnapshotEntry,
    bisect_first_pass,
    confirm_first_pass,
    load_manifest,
    read_task_image,
    summarise_flip,
)
from .telemetry import FLIP_NAME

__all__ = ["ReplayError", "replay_flip"]

log = logging.getLogger("arenabench.replay")

#: Where the task's verifier expects to find things, fixed by the dataset's
#: own contract rather than by us.
TESTS_MOUNT = "/tests"
LOGS_MOUNT = "/logs"
REWARD_PATH = "verifier/reward.txt"


class ReplayError(RuntimeError):
    """Replay could not run at all — as opposed to a snapshot that failed."""


def _run(
    cmd: list[str], *, timeout: float, cwd: Path | None = None
) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ)
    env.pop("DOCKER_DEFAULT_PLATFORM", None)
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=timeout,
        env=env,
        cwd=str(cwd) if cwd else None,
    )


@dataclass
class _Probe:
    """One verifier run against one snapshot, in a throwaway container."""

    image: str
    task_dir: Path
    trial_dir: Path
    scratch: Path
    verifier_timeout: float

    def __call__(self, entry: SnapshotEntry) -> bool | None:
        name = f"arena-replay-{uuid.uuid4().hex[:12]}"
        logs = self.scratch / name
        (logs / "verifier").mkdir(parents=True, exist_ok=True)
        try:
            return self._probe_in(name, entry, logs)
        except (OSError, subprocess.SubprocessError) as exc:
            log.warning("snapshot %d: probe could not run: %s", entry.index, exc)
            return None
        finally:
            _run(["docker", "rm", "-f", name], timeout=120)

    def _probe_in(self, name: str, entry: SnapshotEntry, logs: Path) -> bool | None:
        started = _run(
            [
                "docker", "run", "-d", "--name", name,
                "-v", f"{self.task_dir / 'tests'}:{TESTS_MOUNT}:ro",
                "-v", f"{logs}:{LOGS_MOUNT}",
                "--entrypoint", "sh",
                self.image,
                "-c", "sleep infinity",
            ],
            timeout=600,
        )
        if started.returncode != 0:
            log.warning(
                "snapshot %d: container would not start: %s",
                entry.index,
                started.stderr[-300:],
            )
            return None

        if entry.patch and not self._apply(name, entry):
            return None

        done = _run(
            ["docker", "exec", name, "bash", TESTS_MOUNT + "/test.sh"],
            timeout=self.verifier_timeout,
        )
        reward = (logs / REWARD_PATH).read_text(encoding="utf-8").strip() if (
            logs / REWARD_PATH
        ).exists() else ""
        if reward:
            return reward.startswith("1")
        # No reward file: the verifier itself did not complete. Unknown, not a
        # failure — scoring it 0 would move the flip earlier than the truth.
        log.debug("snapshot %d: no reward file (rc=%s)", entry.index, done.returncode)
        return None

    def _apply(self, name: str, entry: SnapshotEntry) -> bool:
        patch = self.trial_dir / "arena" / "snapshots" / str(entry.patch)
        if not patch.is_file():
            log.warning("snapshot %d: patch file missing", entry.index)
            return False
        copied = _run(["docker", "cp", str(patch), f"{name}:/tmp/snap.patch"], timeout=300)
        if copied.returncode != 0:
            log.warning("snapshot %d: could not copy patch in", entry.index)
            return False
        applied = _run(
            [
                "docker", "exec", "-w", "/", name, "sh", "-c",
                "cd / && git apply --binary --unsafe-paths --directory=/ /tmp/snap.patch"
                " 2>/dev/null || git apply --binary /tmp/snap.patch",
            ],
            timeout=600,
        )
        if applied.returncode != 0:
            log.warning(
                "snapshot %d: patch did not apply: %s", entry.index, applied.stderr[-300:]
            )
            return False
        return True


def replay_flip(
    trial_dir: Path,
    task_dir: Path,
    *,
    verifier_timeout: float = 1800.0,
    confirm_window: int = 3,
    scratch: Path | None = None,
) -> FlipResult:
    """Find the earliest snapshot of ``trial_dir`` that the task's tests pass.

    Writes ``arena/flip.json`` beside the snapshots and returns the summary.
    """
    if shutil.which("docker") is None:
        raise ReplayError("docker is not available on this host")
    entries = load_manifest(trial_dir)
    if not entries:
        raise ReplayError(f"no snapshots captured for {trial_dir.name}")
    image = read_task_image(task_dir)
    if not image:
        raise ReplayError(f"no docker_image in {task_dir / 'task.toml'}")
    if not (task_dir / "tests" / "test.sh").is_file():
        raise ReplayError(f"no tests/test.sh under {task_dir}")

    scratch = scratch or (trial_dir / "arena" / "replay")
    scratch.mkdir(parents=True, exist_ok=True)
    probe = _Probe(
        image=image,
        task_dir=task_dir,
        trial_dir=trial_dir,
        scratch=scratch,
        verifier_timeout=verifier_timeout,
    )

    seen: dict[int, bool | None] = {}

    def probe_index(index: int) -> bool | None:
        if index not in seen:
            log.info(
                "probing snapshot %d/%d (t+%.0fs)",
                index,
                len(entries) - 1,
                entries[index].elapsed,
            )
            seen[index] = probe(entries[index])
        return seen[index]

    answer = bisect_first_pass(len(entries), probe_index)
    if answer is not None and confirm_window > 0:
        answer = confirm_first_pass(answer, probe_index, window=confirm_window)

    result = summarise_flip(
        entries,
        answer,
        probes=len(seen),
        unknown=sum(1 for v in seen.values() if v is None),
    )
    try:
        (trial_dir / FLIP_NAME).write_text(
            json.dumps(result.to_json(), indent=2) + "\n", encoding="utf-8"
        )
    except OSError as exc:  # pragma: no cover - reporting must not fail the run
        log.warning("could not write flip.json: %s", exc)
    return result
