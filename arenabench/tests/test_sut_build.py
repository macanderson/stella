# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The builder must never discard a requested build.

The defect these pin (#2138): ``SutBuilder.start`` executes one build at a
time by design, but while a build was running a request for a *different*
commit came back as the active build's record — and the new request was
dropped entirely. With per-seat pins (#2082) the natural operator flow is
"build champion, build challenger, launch the twin match", and the second
click silently built nothing: the caller polled record A to completion while
ref B was never resolved into a build, so the match then refused at preflight
for a binary nobody was ever going to stage.

The builds themselves are stubbed behind a gate the test controls — the
contract under test is queueing, not cross-compilation, and holding the gate
closed is what makes "a second request arrives while a build is in flight" a
state a test can occupy for as long as it needs.
"""

from __future__ import annotations

import threading
import time
from collections.abc import Callable
from pathlib import Path

import pytest

from arenabench import sut, sut_build
from arenabench.sut_build import BuildRecord, SutBuilder

COMMIT_A = "a" * 40
COMMIT_B = "b" * 40


@pytest.fixture
def refs(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> dict[str, str]:
    """An isolated stage root, and a resolver that needs no git repository.

    Resolution itself is proven by ``test_sut``; here refs map straight to
    fixed commits so every test states which commit a click meant.
    """
    monkeypatch.setenv("ARENABENCH_HOME", str(tmp_path / "home"))
    mapping = {"refA": COMMIT_A, "refB": COMMIT_B}

    def resolve(ref: str) -> str:
        try:
            return mapping[ref]
        except KeyError:
            raise sut.SutUnavailableError(f"unknown ref {ref!r}") from None

    monkeypatch.setattr(sut_build, "resolve_ref", resolve)
    return mapping


def _stage(commit: str) -> tuple[Path, str]:
    """Stage a stand-in artifact exactly where a real build lands one."""
    directory = sut.sut_root() / commit
    directory.mkdir(parents=True, exist_ok=True)
    target = directory / "stella"
    target.write_text("#!/bin/sh\n")
    target.chmod(0o755)
    sha = sut.file_sha256(target)
    (directory / "sut_commit.txt").write_text(commit + "\n", encoding="utf-8")
    (directory / "binary_sha256.txt").write_text(sha + "\n", encoding="utf-8")
    return target, sha


def _wait(predicate: Callable[[], bool], timeout: float = 30.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.01)
    raise AssertionError("condition not reached in time")


def _done(builder: SutBuilder, build_id: str) -> bool:
    record = builder.get(build_id)
    return bool(record and record["done"])


class GatedBuilder(SutBuilder):
    """The real builder with the cross-compile replaced by a gate.

    ``_build`` stages its artifact exactly where the real one lands, so
    ``binary_for`` sees a finished build, but blocks until the test opens the
    gate — the in-flight state every test here is about.
    """

    def __init__(self) -> None:
        super().__init__()
        self.gate = threading.Event()
        self.build_order: list[str] = []
        self.fail_commits: set[str] = set()

    def _build(self, record: BuildRecord) -> tuple[Path, str]:
        self.build_order.append(record.commit)
        assert self.gate.wait(timeout=30.0), "test gate never opened"
        if record.commit in self.fail_commits:
            raise sut.SutUnavailableError("cargo zigbuild exited 101 (stub)")
        return _stage(record.commit)


class TestASecondRefQueuesInsteadOfVanishing:
    """The witness for #2138: two clicks, two refs, two builds."""

    def test_a_distinct_commit_queues_behind_the_active_build(
        self, refs: dict[str, str]
    ):
        builder = GatedBuilder()
        first = builder.start("refA")
        _wait(lambda: builder.build_order == [COMMIT_A])

        second = builder.start("refB")
        assert second["id"] != first["id"], (
            "the second request came back as the first build's record — "
            "ref B was dropped, which is the #2138 defect"
        )
        assert second["ref"] == "refB"
        assert second["commit"] == COMMIT_B
        assert second["status"] == "queued"
        # Execution stays strictly serial: nothing for B runs while A holds
        # the build slot (#2082's laptop constraint stands).
        assert builder.build_order == [COMMIT_A]

        builder.gate.set()
        _wait(lambda: _done(builder, second["id"]))
        assert builder.build_order == [COMMIT_A, COMMIT_B]
        assert builder.get(first["id"])["status"] == "done"
        assert builder.get(second["id"])["status"] == "done"
        for commit in (COMMIT_A, COMMIT_B):
            staged = sut.binary_for(commit)
            assert staged is not None and staged.commit == commit, (
                "a twin match needs both binaries staged under their own "
                "commits"
            )

    def test_a_failed_active_build_still_promotes_the_queue(
        self, refs: dict[str, str]
    ):
        builder = GatedBuilder()
        builder.fail_commits.add(COMMIT_A)
        first = builder.start("refA")
        second = builder.start("refB")

        builder.gate.set()
        _wait(lambda: _done(builder, second["id"]))
        assert builder.get(first["id"])["status"] == "failed"
        assert builder.get(second["id"])["status"] == "done"
        assert sut.binary_for(COMMIT_B) is not None, (
            "one ref failing to compile must not strand the request behind it"
        )


class TestTheSameCommitNeverBuildsTwice:
    """The dedupe half of the contract, unchanged from before the queue."""

    def test_the_active_commit_dedupes_to_the_running_record(
        self, refs: dict[str, str]
    ):
        builder = GatedBuilder()
        first = builder.start("refA")
        again = builder.start("refA")
        assert again["id"] == first["id"]

        builder.gate.set()
        _wait(lambda: _done(builder, first["id"]))
        assert builder.build_order == [COMMIT_A]

    def test_a_queued_commit_dedupes_to_the_queued_record(
        self, refs: dict[str, str]
    ):
        builder = GatedBuilder()
        builder.start("refA")
        queued = builder.start("refB")
        again = builder.start("refB")
        assert again["id"] == queued["id"], (
            "a twin of a queued build would compile the same commit twice"
        )

        builder.gate.set()
        _wait(lambda: _done(builder, queued["id"]))
        assert builder.build_order == [COMMIT_A, COMMIT_B]

    def test_an_already_staged_commit_returns_a_cached_record(
        self, refs: dict[str, str]
    ):
        _stage(COMMIT_A)
        builder = GatedBuilder()
        record = builder.start("refA")
        assert record["cached"] and record["status"] == "done"
        assert builder.build_order == []

    def test_a_queued_commit_staged_in_the_meantime_is_not_rebuilt(
        self, refs: dict[str, str]
    ):
        builder = GatedBuilder()
        builder.start("refA")
        _wait(lambda: builder.build_order == [COMMIT_A])
        second = builder.start("refB")
        # e.g. `build_sut.sh` staged the same commit while the queue waited.
        _stage(COMMIT_B)

        builder.gate.set()
        _wait(lambda: _done(builder, second["id"]))
        record = builder.get(second["id"])
        assert record["status"] == "done" and record["cached"], (
            "rebuilding a commit that is already staged invites the two "
            "artifacts to differ"
        )
        assert builder.build_order == [COMMIT_A]


class TestHistoryTrimmingNeverDropsLiveBuilds:
    """Trimming the finished-build log must not become a second silent drop."""

    def test_a_flood_of_cached_starts_evicts_neither_active_nor_queued(
        self, refs: dict[str, str]
    ):
        builder = GatedBuilder()
        first = builder.start("refA")
        _wait(lambda: builder.build_order == [COMMIT_A])
        second = builder.start("refB")

        # Enough finished records to trim well past the history window while
        # A runs and B waits.
        for index in range(sut_build._HISTORY + 5):
            commit = f"{index:040x}"
            refs[f"cached{index}"] = commit
            _stage(commit)
            assert builder.start(f"cached{index}")["cached"]

        assert builder.get(first["id"]) is not None, (
            "evicting the active build 404s every poll watching it"
        )
        assert builder.get(second["id"]) is not None, (
            "evicting a queued build drops the request — the #2138 defect "
            "wearing a different hat"
        )

        builder.gate.set()
        _wait(lambda: _done(builder, second["id"]))
        assert sut.binary_for(COMMIT_B) is not None


def test_an_unresolvable_ref_is_a_synchronous_error(refs: dict[str, str]):
    """A bad ref fails in the click that caused it, never in the worker."""
    with pytest.raises(ValueError):
        GatedBuilder().start("nonesuch")
