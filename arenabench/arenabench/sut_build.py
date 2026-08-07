# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Building a Stella SUT for a chosen commit, out of the launch path.

A release cross-compile of Stella is minutes of work: `cargo zigbuild` against
a glibc 2.17 floor so the artifact execs inside every task image. That is far
too long to sit inside "start match", so a build is its own action with its own
lifecycle, and a match only ever *finds* a binary that either exists or does
not (:func:`arenabench.sut.binary_for`).

**Builds are cached by commit, not by ref.** Two matches naming ``main`` a week
apart want two different binaries; two matches naming the same commit want the
same one, and rebuilding it would only invite the two to differ. The cache key
is therefore the resolved commit and the artifact lands at
``<sut root>/<commit>/stella`` beside the commit id and the SHA-256 that
identify it.

**The build runs in a detached worktree, never in the operator's checkout.**
`bench/evidence/run/build_sut.sh` refuses to build unless every Rust input
matches ``origin/main``, which is the right rule for its own job and the wrong
one here: the whole feature is building a *chosen* ref, including a branch that
by definition differs from main. So this module checks the commit out into a
throwaway worktree and builds there. That also means a build cannot disturb
whatever the operator is editing — the failure mode of building in-place is
that a benchmark silently measures uncommitted local edits.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import threading
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .sut import (
    SutUnavailableError,
    binary_for,
    file_sha256,
    resolve_ref,
    stella_repo,
    sut_root,
)

__all__ = ["BuildRecord", "SutBuilder"]

#: Target triple and glibc floor, mirrored from `bench/evidence/run/env.sh`.
#: Constants rather than settings: an operator able to raise the floor to
#: silence an error is the failure being prevented — a binary built against a
#: newer glibc runs on the build host and dies inside the task container, where
#: it is indistinguishable from an agent that cannot code.
TARGET_TRIPLE = "x86_64-unknown-linux-gnu"
GLIBC_FLOOR = "2.17"

#: How many finished builds to keep in the in-memory log.
_HISTORY = 20


@dataclass
class BuildRecord:
    """One build attempt, from queued to done."""

    id: str
    ref: str
    commit: str = ""
    #: ``queued`` | ``building`` | ``done`` | ``failed``
    status: str = "queued"
    started_at: float = field(default_factory=time.time)
    finished_at: float = 0.0
    binary: str = ""
    sha256: str = ""
    error: str = ""
    #: Tail of the build log, so a failure explains itself in the UI rather
    #: than sending the operator to a file they have to find first.
    log_tail: list[str] = field(default_factory=list)
    #: True when the commit was already built and nothing was compiled.
    cached: bool = False

    def to_json(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "ref": self.ref,
            "commit": self.commit,
            "short": self.commit[:8] if self.commit else "",
            "status": self.status,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "elapsed": (self.finished_at or time.time()) - self.started_at,
            "binary": self.binary,
            "sha256": self.sha256,
            "error": self.error,
            "log_tail": list(self.log_tail),
            "cached": self.cached,
            "done": self.status in ("done", "failed"),
        }


class SutBuilder:
    """Owns SUT builds: one at a time, cached by commit.

    Serialised deliberately. Two concurrent release builds on a laptop thrash
    the disk and the CPU that a running match is also using, and the queue
    depth this feature actually needs is one.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._builds: dict[str, BuildRecord] = {}
        self._order: list[str] = []
        self._active: str | None = None

    # -- queries ----------------------------------------------------------

    def get(self, build_id: str) -> dict[str, Any] | None:
        with self._lock:
            record = self._builds.get(build_id)
            return record.to_json() if record else None

    def recent(self) -> list[dict[str, Any]]:
        with self._lock:
            return [self._builds[i].to_json() for i in reversed(self._order)]

    # -- command ----------------------------------------------------------

    def start(self, ref: str) -> dict[str, Any]:
        """Resolve ``ref`` and build it, unless it is already built or running.

        Resolution happens here rather than in the worker so a bad ref is an
        immediate, synchronous error the operator sees in the click that
        caused it, instead of a build that fails ten seconds later.
        """
        try:
            commit = resolve_ref(ref)
        except SutUnavailableError as exc:
            raise ValueError(str(exc)) from exc

        record = BuildRecord(id=uuid.uuid4().hex[:12], ref=ref, commit=commit)

        existing = binary_for(commit)
        if existing is not None:
            record.status = "done"
            record.cached = True
            record.binary = str(existing.path)
            record.sha256 = existing.sha256
            record.finished_at = time.time()
            self._remember(record)
            return record.to_json()

        with self._lock:
            if self._active is not None:
                active = self._builds[self._active]
                if active.status in ("queued", "building"):
                    # Report the running build rather than queueing behind it:
                    # the caller wants to watch something, and two builds of
                    # different commits at once is the case this refuses.
                    return active.to_json()
            self._active = record.id
        self._remember(record)
        threading.Thread(target=self._run, args=(record,), daemon=True).start()
        return record.to_json()

    # -- internals --------------------------------------------------------

    def _remember(self, record: BuildRecord) -> None:
        with self._lock:
            self._builds[record.id] = record
            self._order.append(record.id)
            while len(self._order) > _HISTORY:
                self._builds.pop(self._order.pop(0), None)

    def _run(self, record: BuildRecord) -> None:
        try:
            record.status = "building"
            binary, sha = self._build(record)
            record.binary, record.sha256 = str(binary), sha
            record.status = "done"
        except Exception as exc:  # noqa: BLE001 — surfaced to the operator
            record.status = "failed"
            record.error = str(exc)
        finally:
            record.finished_at = time.time()
            with self._lock:
                if self._active == record.id:
                    self._active = None

    def _build(self, record: BuildRecord) -> tuple[Path, str]:
        repo = stella_repo()
        if repo is None:
            raise SutUnavailableError(
                "no Stella checkout configured; set ARENABENCH_STELLA_REPO"
            )
        if shutil.which("cargo") is None:
            raise SutUnavailableError("cargo is not on PATH")

        worktree = Path(tempfile.mkdtemp(prefix="arenabench-sut-"))
        tree = worktree / "src"
        zig_cache = worktree / "zig"
        try:
            self._sh(record, repo, ["git", "worktree", "add", "--detach",
                                    str(tree), record.commit], timeout=600)
            (zig_cache / "global").mkdir(parents=True, exist_ok=True)
            (zig_cache / "local").mkdir(parents=True, exist_ok=True)

            env = {
                **os.environ,
                "ZIG_GLOBAL_CACHE_DIR": str(zig_cache / "global"),
                "ZIG_LOCAL_CACHE_DIR": str(zig_cache / "local"),
                # The binary reports this as its own source commit, which is
                # what makes a trial's telemetry self-describing.
                "STELLA_BUILD_GIT_SHA": record.commit,
            }
            self._sh(record, tree, ["rustup", "target", "add", TARGET_TRIPLE],
                     timeout=600, env=env, optional=True)
            self._sh(
                record,
                tree,
                [
                    "cargo", "zigbuild", "--release", "--locked",
                    "--target", f"{TARGET_TRIPLE}.{GLIBC_FLOOR}",
                    "--package", "stella-cli", "--bin", "stella",
                ],
                timeout=5400,
                env=env,
            )
            built = tree / "target" / TARGET_TRIPLE / "release" / "stella"
            if not built.is_file():
                raise SutUnavailableError(f"build produced no binary at {built}")

            # Stage atomically-ish: write into the commit's directory only
            # after the artifact exists, so a crashed build never leaves a
            # directory that `binary_for` would accept as a finished one.
            dest = sut_root() / record.commit
            dest.mkdir(parents=True, exist_ok=True)
            target = dest / "stella"
            shutil.copy2(built, target)
            target.chmod(0o755)
            sha = file_sha256(target)
            (dest / "sut_commit.txt").write_text(record.commit + "\n", encoding="utf-8")
            (dest / "binary_sha256.txt").write_text(sha + "\n", encoding="utf-8")
            return target, sha
        finally:
            # Detach the worktree from git's registry before deleting it, or
            # the repo accumulates stale entries `git worktree list` reports
            # forever.
            with_tree = tree.exists()
            if with_tree:
                subprocess.run(
                    ["git", "worktree", "remove", "--force", str(tree)],
                    cwd=str(repo), capture_output=True, check=False, timeout=120,
                )
            shutil.rmtree(worktree, ignore_errors=True)

    def _sh(
        self,
        record: BuildRecord,
        cwd: Path,
        command: list[str],
        *,
        timeout: float,
        env: dict[str, str] | None = None,
        optional: bool = False,
    ) -> None:
        record.log_tail.append(f"$ {' '.join(command)}")
        try:
            out = subprocess.run(
                command,
                cwd=str(cwd),
                capture_output=True,
                text=True,
                timeout=timeout,
                check=False,
                env=env,
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            if optional:
                record.log_tail.append(f"(skipped: {exc})")
                return
            raise SutUnavailableError(f"{command[0]} failed: {exc}") from exc
        tail = (out.stderr or out.stdout or "").strip().splitlines()[-12:]
        record.log_tail.extend(tail)
        del record.log_tail[:-40]
        if out.returncode != 0 and not optional:
            raise SutUnavailableError(
                f"{' '.join(command[:2])} exited {out.returncode}: "
                + " / ".join(tail[-3:])
            )
