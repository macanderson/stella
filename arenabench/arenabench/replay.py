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
    detect_workspace,
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
        """Bring the probe container's workspace to this snapshot's state.

        The patch is applied **on the host**, not in the container, and the
        result is copied back. Two reasons, both found by running this against
        a real image rather than reasoning about it:

        * ``git`` is not guaranteed to exist in a task image. The capture side
          gets away with assuming it because Stella's adapter installs git into
          the *task* container; a replay probe is a fresh container from the
          pristine image and gets no such help. Applying in-container failed
          with ``sh: 1: git: not found`` and — far worse than crashing —
          reported the snapshot as *unknown*, which surfaced as
          ``flip_index=None``, i.e. "the agent never solved it".
        * Snapshots are captured with ``--work-tree=<workspace>``, so the paths
          inside the patch are workspace-relative. Applying them at ``/``, as
          this used to, would put every file in the wrong place even where git
          was present.
        """
        patch = self.trial_dir / "arena" / "snapshots" / str(entry.patch)
        if not patch.is_file():
            log.warning("snapshot %d: patch file missing", entry.index)
            return False
        # The workspace the snapshot was *taken of*, not whatever this fresh
        # container happens to probe as. Falls back to probing only for
        # manifests written before the field existed.
        workspace = entry.workspace or detect_workspace(name)
        if workspace is None:
            log.warning("snapshot %d: no workspace found in the probe", entry.index)
            return False
        # A directory the agent created will not exist in the pristine image.
        _run(["docker", "exec", name, "mkdir", "-p", workspace], timeout=120)

        stage = self.scratch / f"{name}-ws"
        shutil.rmtree(stage, ignore_errors=True)
        stage.mkdir(parents=True, exist_ok=True)
        out = _run(["docker", "cp", f"{name}:{workspace}/.", str(stage)], timeout=900)
        if out.returncode != 0:
            log.warning("snapshot %d: could not stage the workspace", entry.index)
            return False

        applied = _run(
            ["git", "-C", str(stage), "apply", "--binary", "--whitespace=nowarn", str(patch)],
            timeout=600,
        )
        if applied.returncode != 0:
            log.warning(
                "snapshot %d: patch did not apply: %s", entry.index, applied.stderr[-300:]
            )
            return False

        back = _run(["docker", "cp", f"{stage}/.", f"{name}:{workspace}"], timeout=900)
        if back.returncode != 0:
            log.warning("snapshot %d: could not restore the workspace", entry.index)
            return False
        shutil.rmtree(stage, ignore_errors=True)
        return True


def _require_bind_mount(scratch: Path, image: str) -> None:
    """Fail early if the container runtime cannot see ``scratch``.

    The verifier reports its verdict by writing ``/logs/verifier/reward.txt``
    into a bind mount. If that mount is invisible to the host, no reward file
    ever appears and *every* probe returns unknown — which summarises as
    ``flip_index=None``, indistinguishable from an agent that never solved
    anything. The cause is mundane and easy to hit: a VM-backed Docker (Colima,
    Docker Desktop) shares only some host paths, and a macOS temp directory
    under ``/var/folders`` is typically not one of them. Measured: a marker
    file written there is simply absent inside the container.

    One container start to convert a silent, wrong benchmark number into a
    sentence naming the fix.
    """
    marker = scratch / ".arena-mount-probe"
    try:
        marker.write_text("ok", encoding="utf-8")
    except OSError as exc:
        raise ReplayError(f"scratch directory is not writable: {exc}") from exc
    seen = _run(
        ["docker", "run", "--rm", "-v", f"{scratch}:/probe", image,
         "sh", "-c", "cat /probe/.arena-mount-probe 2>/dev/null"],
        timeout=600,
    )
    marker.unlink(missing_ok=True)
    if seen.stdout.strip() != "ok":
        raise ReplayError(
            f"the container runtime cannot read {scratch} — the verifier's reward "
            "would be invisible and every snapshot would read as 'did not pass'. "
            "Pass --scratch (or place the workspace) under a directory your Docker "
            "shares with the host, e.g. somewhere under your home directory."
        )


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
    # Patches are applied host-side (see `_Probe._apply`), so host git is a
    # hard precondition. Raised here rather than discovered per-probe: without
    # it every patched snapshot returns *unknown*, and a run of unknowns
    # summarises as `flip_index=None`, which reads as "the agent never solved
    # it" rather than "this could not be measured".
    if shutil.which("git") is None:
        raise ReplayError("git is not available on this host; replay applies patches locally")
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
    _require_bind_mount(scratch, image)
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
