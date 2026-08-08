"""Pulling a trial's ``.stella/private/`` telemetry out before teardown (#2109).

A Harbor trial's container is deleted with everything Stella recorded inside
it: the SQLite store ``stella observe`` and ``stella usage sync`` read, the
reflections that skill mining feeds on, and episodic context. Only the event
journal survived, and it is a projection — so bench runs contributed nothing
to local telemetry, skill mining, or the tool foundry.

Best-effort by contract: a graded trial is never failed over a telemetry
export, so every probe and download failure is skipped, and Harbor API
variance (sync vs async file helpers) is absorbed.
"""

from __future__ import annotations

import inspect
from pathlib import Path
from typing import Any

from harbor.environments.base import BaseEnvironment

#: Host-side directory (under the trial's ``agent/`` logs) receiving the
#: container's ``.stella/private/`` telemetry before teardown.
PRIVATE_TELEMETRY_DIR = "telemetry"
#: Container-side private state worth surviving the container: the SQLite
#: store `stella observe` and `stella usage sync` read, the reflection log
#: skill mining feeds on, and episodic context. SQLite WAL/SHM sidecars ride
#: along when present so an uncheckpointed database stays openable.
PRIVATE_TELEMETRY_FILES = ("store.db", "reflections.jsonl", "context.db")
#: Used when Harbor's task environment config names no working directory.
_DEFAULT_WORKDIR = "/app"


async def _call(method: Any, *args: Any) -> Any:
    """Invoke a Harbor file helper that may be either sync or async."""
    result = method(*args)
    if inspect.isawaitable(result):
        result = await result
    return result


async def export_private_telemetry(
    environment: BaseEnvironment, target_dir: Path
) -> None:
    """Copy the container's private telemetry into ``target_dir`` on the host.

    Each entry of :data:`PRIVATE_TELEMETRY_FILES` is pulled together with any
    SQLite ``-wal``/``-shm`` sidecar beside it. ``target_dir`` is created only
    once a file is actually found, so a trial that recorded nothing leaves no
    empty directory behind.
    """
    workdir = (
        getattr(getattr(environment, "task_env_config", None), "workdir", None)
        or _DEFAULT_WORKDIR
    )
    for base in PRIVATE_TELEMETRY_FILES:
        for name in (base, f"{base}-wal", f"{base}-shm"):
            source = f"{workdir}/.stella/private/{name}"
            try:
                if not await _call(environment.is_file, source):
                    continue
                target_dir.mkdir(parents=True, exist_ok=True)
                await _call(environment.download_file, source, target_dir / name)
            except Exception:  # noqa: BLE001 — export never fails a trial
                continue
