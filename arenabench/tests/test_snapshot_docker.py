# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Snapshot capture against a real container.

The rest of the snapshot suite is the pure half — the search, the manifest, the
box walk. None of it touches Docker, which left the genuinely risky parts
unexercised: whether the container name is derived the way Harbor spells it,
whether the workspace is found, whether the detached ``--git-dir`` invocation
actually produces a patch, and whether the workspace survives untouched.

That last one is why this test exists at all. ``fix-git`` is a task in this
dataset whose whole subject is repository state; a snapshotter that wrote
``.git`` into the workspace would silently rewrite the task it was measuring
and every number derived from it would be quietly wrong. A unit test cannot see
that — only a real filesystem in a real container can.

Skipped without Docker, so it costs nothing in CI. Uses ``alpine`` and no
benchmark, so it needs no dataset, no credentials and no spend.
"""

from __future__ import annotations

import shutil
import subprocess
import time
from pathlib import Path

import pytest

from arenabench.snapshot import (
    SnapshotSupervisor,
    container_for_trial,
    detect_workspace,
    load_manifest,
)

IMAGE = "alpine:3.20"


def _docker_ready() -> bool:
    if shutil.which("docker") is None:
        return False
    try:
        probe = subprocess.run(["docker", "info"], capture_output=True, timeout=30)
        return probe.returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


pytestmark = pytest.mark.skipif(not _docker_ready(), reason="docker is not available")


def _sh(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(list(args), capture_output=True, text=True, timeout=600)


@pytest.fixture
def container(tmp_path: Path):
    """A container named the way Harbor names a trial's, holding a workspace."""
    trial = tmp_path / "jobs" / "seat" / "demo__AbC123"
    (trial / "agent").mkdir(parents=True)
    name = container_for_trial(trial)
    _sh("docker", "rm", "-f", name)
    started = _sh("docker", "run", "-d", "--name", name, IMAGE, "sh", "-c", "sleep 600")
    if started.returncode != 0:
        pytest.skip(f"could not start {IMAGE}: {started.stderr[:200]}")
    _sh("docker", "exec", name, "sh", "-c", "mkdir -p /app && echo one > /app/a.txt")
    if _sh("docker", "exec", name, "sh", "-c", "apk add --no-cache git").returncode != 0:
        _sh("docker", "rm", "-f", name)
        pytest.skip("could not install git in the test container")
    try:
        yield name, trial, tmp_path / "jobs"
    finally:
        _sh("docker", "rm", "-f", name)


def test_the_container_name_matches_how_harbor_spells_it(container):
    name, _trial, _ = container
    # Trial ids are mixed case; Docker names are not. A mismatch here means
    # silently zero snapshots, indistinguishable from the feature being off.
    assert name == "demo__abc123-main-1"
    assert _sh("docker", "inspect", "-f", "{{.State.Running}}", name).stdout.strip() == "true"


def test_the_workspace_is_found_inside_the_image(container):
    name, _, _ = container
    assert detect_workspace(name) == "/app"


def test_capture_produces_patches_and_leaves_the_workspace_untouched(container):
    name, trial, jobs_root = container
    sup = SnapshotSupervisor(
        jobs_root=jobs_root, jobs=["seat"], interval=2.0, poll_interval=0.5
    )
    sup.start()
    try:
        time.sleep(4)
        _sh("docker", "exec", name, "sh", "-c", "echo two >> /app/a.txt; echo new > /app/b.txt")
        time.sleep(4)
        # `result.json` appearing is how the supervisor learns a trial ended.
        (trial / "result.json").write_text('{"reward": 1}', encoding="utf-8")
        time.sleep(3)
    finally:
        sup.stop(timeout=20)

    entries = load_manifest(trial)
    assert len(entries) >= 2, "expected repeated capture over the interval"

    patched = [e for e in entries if e.patch]
    assert patched, "no patch produced, so nothing could ever be replayed"
    body = (trial / "arena" / "snapshots" / str(patched[-1].patch)).read_text(
        encoding="utf-8", errors="replace"
    )
    assert "b.txt" in body, "the patch is missing the file the agent created"
    assert "+two" in body, "the patch is missing the agent's edit"

    # The whole point: capture must be invisible to the task being measured.
    listing = _sh("docker", "exec", name, "sh", "-c", "ls -a /app").stdout.split()
    assert ".git" not in listing, "snapshotter wrote .git into the workspace it measures"
    assert {"a.txt", "b.txt"} <= set(listing)
