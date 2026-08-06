# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Flip detection end to end, against real containers.

`replay_flip` shipped in #1533 without ever being run, and running it found
three bugs — all of which produced the *same* silent wrong answer,
`flip_index=None`, which an operator reads as "the agent never solved it":

1. patches were applied with ``git apply`` **inside** the probe container, but
   a task image is not guaranteed to ship git (the capture side only gets away
   with it because Stella's adapter installs git into the *task* container);
2. they were applied at ``/`` although snapshots are captured with
   ``--work-tree=<workspace>``, so their paths are workspace-relative;
3. the workspace was re-probed in the fresh container instead of being taken
   from the manifest, and a directory the agent created does not exist in the
   pristine image — so the patch applied cleanly to the wrong place.

None of the three raised. That is what this test exists to prevent.

The task here is synthetic, so this needs no dataset, no credentials and no
spend: an image, a ``tests/test.sh`` that passes once a file exists, and
snapshots taken either side of that file appearing. The flip is therefore
known in advance, which is what makes the answer checkable.
"""

from __future__ import annotations

import shutil
import subprocess
import time
from pathlib import Path

import pytest

from arenabench.replay import replay_flip
from arenabench.snapshot import SnapshotSupervisor, container_for_trial, load_manifest

IMAGE = "debian:stable-slim"


def _ready() -> bool:
    if shutil.which("docker") is None or shutil.which("git") is None:
        return False
    try:
        probe = subprocess.run(["docker", "info"], capture_output=True, timeout=30)
        return probe.returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


pytestmark = pytest.mark.skipif(not _ready(), reason="docker and git are required")


def _sh(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(list(args), capture_output=True, text=True, timeout=900)


def _write_task(task: Path) -> None:
    (task / "tests").mkdir(parents=True)
    (task / "task.toml").write_text(
        f'[task]\nname = "synthetic/flip"\n\n[verifier]\ntimeout_sec = 300.0\n\n'
        f'[environment]\ndocker_image = "{IMAGE}"\n',
        encoding="utf-8",
    )
    # The real contract: the verifier writes its reward to this exact path.
    (task / "tests" / "test.sh").write_text(
        "#!/bin/bash\n"
        "mkdir -p /logs/verifier\n"
        "if [ -f /app/solution.txt ]; then echo 1 > /logs/verifier/reward.txt;\n"
        "else echo 0 > /logs/verifier/reward.txt; fi\n",
        encoding="utf-8",
    )


@pytest.fixture
def bindable_root():
    """A working directory the container runtime can actually bind-mount.

    NOT `tmp_path`. A VM-backed Docker shares only some host paths, and pytest's
    temp directory lives under macOS's `/var/folders`, which Colima does not
    share — measured: a marker file written there is simply absent inside the
    container. The verifier would then write its reward into a mount the host
    cannot read, every probe would return unknown, and the run would report
    `flip_index=None`: "the agent never solved it".
    """
    root = Path.home() / ".arenabench" / "test-replay"
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    try:
        yield root
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_flip_lands_where_the_solution_actually_appeared(bindable_root: Path):
    jobs = bindable_root / "jobs"
    trial = jobs / "seat" / "demo__RpL1"
    (trial / "agent").mkdir(parents=True)
    task = bindable_root / "task"
    _write_task(task)

    name = container_for_trial(trial)
    _sh("docker", "rm", "-f", name)
    if _sh("docker", "run", "-d", "--name", name, IMAGE, "sleep", "900").returncode != 0:
        pytest.skip(f"could not start {IMAGE}")
    try:
        _sh("docker", "exec", name, "sh", "-c", "mkdir -p /app && echo start > /app/notes.txt")
        installed = _sh(
            "docker", "exec", name, "sh", "-c",
            "apt-get update -qq && apt-get install -y -qq git",
        )
        if installed.returncode != 0:
            pytest.skip("could not install git into the capture container")

        sup = SnapshotSupervisor(
            jobs_root=jobs, jobs=["seat"], interval=2.0, poll_interval=0.5
        )
        sup.start()
        try:
            time.sleep(5)  # snapshots taken while unsolved
            _sh("docker", "exec", name, "sh", "-c", "echo done > /app/solution.txt")
            solved_at = len(load_manifest(trial))
            time.sleep(5)  # snapshots taken after solving
            (trial / "result.json").write_text('{"reward": 1}', encoding="utf-8")
            time.sleep(3)
        finally:
            sup.stop(timeout=20)
    finally:
        _sh("docker", "rm", "-f", name)

    entries = load_manifest(trial)
    assert len(entries) >= 4, "too few snapshots to bisect meaningfully"
    # Bug 3: the workspace must travel with the snapshot, not be re-derived.
    assert entries[-1].workspace == "/app"

    result = replay_flip(trial, task, verifier_timeout=300.0, confirm_window=2)

    assert result.unknown == 0, "a probe could not run — see bugs 1 and 2"
    assert result.flip_index is not None, "the task was definitely solved"
    assert result.flip_index >= solved_at, (
        f"flip {result.flip_index} precedes the solution at {solved_at}"
    )
    assert result.wasted_elapsed is not None and result.wasted_elapsed > 0
    # Bisection, not a scan: each probe is a full verifier run.
    assert result.probes < result.snapshots
