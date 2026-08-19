# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""A match found on disk must not claim to be over while it is still running.

``MatchRunner.restore_from_disk`` adopts every match directory this process did
not launch, and ``ArenaServer._refresh_from_disk`` calls it behind every
listing request. Adoption used to stamp ``status = "finished"`` unconditionally,
so a match ``arenabench run`` started in another process appeared in the arena
*complete*, seconds after it started — and so did every match still running
across an arena restart.

The cosmetic half is a green pill over a contest in flight. The expensive half
is that :func:`arenabench.reap.reap_finished` sweeps the containers of any
match recorded over, so a match wrongly believed finished has its **running
trial containers removed** by the next launch — which is the mis-scored loss
of #2329 and #2326 reached from a third direction, this time caused by the
arena's own bookkeeping rather than by a leak.

Every test here is a witness for one rung of :func:`arenabench.adopt.status_for`:
a live container, a complete set of results, fresh files, stale files — and for
the sweep that keeps an adopted match's status moving after the first read.
"""

from __future__ import annotations

import json
import os
import time
from pathlib import Path

import pytest

from arenabench import adopt, reap
from arenabench.registry import DEFAULT_REGISTRY
from arenabench.runner import MatchRunner

MATCH_ID = "adopted"
SEAT = "stella"
TASKS = ("alpha", "beta")

RESULT = {"verifier_result": {"rewards": {"reward": 1.0}}}


def _workspace(tmp_path: Path, *, settled: tuple[str, ...]) -> Path:
    """A workspace holding one adopted match whose ``settled`` tasks finished.

    Every task gets a trial directory with an event stream — that is what makes
    it a trial that *started* — and only the settled ones get the
    ``result.json`` Harbor writes when a trial ends.
    """
    workspace = tmp_path / "ws"
    match_dir = workspace / "matches" / MATCH_ID
    job_dir = match_dir / "jobs" / f"{MATCH_ID}-{SEAT}"
    (match_dir / f"{SEAT}.log").parent.mkdir(parents=True, exist_ok=True)
    (match_dir / f"{SEAT}.log").write_text("launching\n", encoding="utf-8")
    match_dir.joinpath("spec.json").write_text(
        json.dumps(
            {
                "id": MATCH_ID,
                "name": MATCH_ID,
                "dataset": "terminal-bench-2.1",
                "tasks": list(TASKS),
                "sut_ref": "",
                "contestants": [
                    {
                        "id": SEAT,
                        "name": SEAT,
                        "agent": "stella",
                        "engine": {"api": "openrouter", "model": "z-ai/glm-5.2"},
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    for task in TASKS:
        trial = job_dir / f"{task}__1"
        (trial / "agent").mkdir(parents=True)
        (trial / "agent" / "stella-events.jsonl").write_text(
            '{"type":"stage","name":"execute"}\n', encoding="utf-8"
        )
        if task in settled:
            (trial / "result.json").write_text(json.dumps(RESULT), encoding="utf-8")
    return workspace


def _backdate(workspace: Path, seconds: float) -> None:
    """Age every file in the workspace, so "quiet for N minutes" is testable
    without sleeping for N minutes."""
    when = time.time() - seconds
    for path in workspace.rglob("*"):
        os.utime(path, (when, when))


@pytest.fixture(autouse=True)
def no_daemon(monkeypatch: pytest.MonkeyPatch) -> None:
    """No containers unless a test says so. A host running the suite may well
    have unrelated containers up, and a status derived from them would make
    these tests depend on the machine."""
    monkeypatch.setattr(reap, "list_containers", list)


def _adopt(workspace: Path) -> tuple[MatchRunner, object]:
    runner = MatchRunner(DEFAULT_REGISTRY, workspace)
    assert runner.restore_from_disk() == 1
    return runner, runner.matches[MATCH_ID]


class TestAnAdoptedMatchReportsWhatTheArtifactsSay:
    def test_a_match_with_a_result_for_every_task_is_finished(
        self, tmp_path: Path
    ) -> None:
        _runner, match = _adopt(_workspace(tmp_path, settled=TASKS))
        assert match.status == "finished"
        assert match.finished_at is not None

    def test_a_match_still_writing_is_running_not_finished(
        self, tmp_path: Path
    ) -> None:
        """The witness for the defect itself.

        One task has no `result.json` and the files were written moments ago:
        something is driving this match. Adoption reported "finished".
        """
        _runner, match = _adopt(_workspace(tmp_path, settled=("alpha",)))
        assert match.status == "running"
        assert "1 of 2 trials" in match.note

    def test_a_running_match_has_no_finish_time(self, tmp_path: Path) -> None:
        """`snapshot` reports `finished_at - started_at` as the elapsed clock
        whenever `finished_at` is set, so stamping one on a live match freezes
        its duration at the moment it was adopted."""
        _runner, match = _adopt(_workspace(tmp_path, settled=("alpha",)))
        assert match.finished_at is None
        assert match.snapshot()["elapsed"] > 0

    def test_a_partly_abandoned_match_is_finished_and_says_what_is_missing(
        self, tmp_path: Path
    ) -> None:
        """Unsettled *and* quiet is not running — but a contest that produced
        most of its results did not *fail*.

        The bar for `failed` is the one `_settle` already uses: no trial ever
        ran. Anything stricter relabels most of a real archive — a run where 3
        of 88 trials wedged is a partial result, not a failure — so the count
        goes in the note, where an operator reads it, and the status stays the
        one every other reader of this match already agrees on.
        """
        workspace = _workspace(tmp_path, settled=("alpha",))
        _backdate(workspace, adopt.STALE_AFTER_SECONDS + 60)
        _runner, match = _adopt(workspace)
        assert match.status == "finished"
        assert "1 of 2 trials" in match.note
        assert "never produced a result" in match.note

    def test_a_match_that_produced_nothing_at_all_is_failed(
        self, tmp_path: Path
    ) -> None:
        """The other half of `_settle`'s rule: no trial ever produced a result,
        so no contest happened, and "finished" would put a green pill over
        0/N."""
        workspace = _workspace(tmp_path, settled=())
        _backdate(workspace, adopt.STALE_AFTER_SECONDS + 60)
        _runner, match = _adopt(workspace)
        assert match.status == "failed"
        assert "none of its 2 trials" in match.note

    def test_a_live_container_outranks_every_file_signal(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """Every artifact says this match is over — a result for every task,
        nothing written in an hour — and a container of its own trial is
        running. The container is an observation and the files are an
        inference, so the container wins."""
        workspace = _workspace(tmp_path, settled=TASKS)
        _backdate(workspace, 3600)
        monkeypatch.setattr(
            reap,
            "list_containers",
            lambda: [reap.Container("alpha__1-main-1", "alpha__1", running=True)],
        )
        _runner, match = _adopt(workspace)
        assert match.status == "running"


class TestAnAdoptedMatchKeepsBeingRead:
    """Nothing in this process supervises an adopted match, so the sweep that
    adopted it is also the only thing that can ever move it on."""

    def test_a_later_sweep_promotes_a_finished_match(self, tmp_path: Path) -> None:
        workspace = _workspace(tmp_path, settled=("alpha",))
        runner, match = _adopt(workspace)
        assert match.status == "running"

        trial = workspace / "matches" / MATCH_ID / "jobs" / f"{MATCH_ID}-{SEAT}"
        (trial / "beta__1" / "result.json").write_text(
            json.dumps(RESULT), encoding="utf-8"
        )
        runner.restore_from_disk()

        assert runner.matches[MATCH_ID].status == "finished"
        assert runner.matches[MATCH_ID].finished_at is not None

    def test_a_launched_match_is_never_second_guessed(self, tmp_path: Path) -> None:
        """A match this process launched has a poll loop that owns its status.
        Re-deriving that from file timestamps would fight the supervisor — and
        a match between trials writes nothing, so it would lose."""
        workspace = _workspace(tmp_path, settled=())
        runner, match = _adopt(workspace)
        match.adopted = False
        match.status = "running"
        match.note = "owned by the poll loop"

        _backdate(workspace, adopt.STALE_AFTER_SECONDS + 60)
        runner.restore_from_disk()

        assert runner.matches[MATCH_ID].status == "running"
        assert runner.matches[MATCH_ID].note == "owned by the poll loop"


class TestTheStatusIsWhatDecidesAReap:
    """Why this is not a cosmetic bug: the status gates container removal."""

    def test_a_live_adopted_match_is_not_swept_by_the_next_launch(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """End to end. Under the old unconditional `finished`, adopting a
        running match and then starting another one removed the running
        match's containers — every trial it had left scored as an agent loss.
        """
        workspace = _workspace(tmp_path, settled=("alpha",))
        live = [reap.Container("beta__1-main-1", "beta__1", running=True)]
        monkeypatch.setattr(reap, "list_containers", lambda: list(live))
        removed: list[str] = []
        monkeypatch.setattr(
            reap, "remove_containers", lambda names: removed.extend(names) or names
        )

        _runner, match = _adopt(workspace)
        assert match.status == "running"
        assert reap.reap_finished([match]) == []
        assert removed == []
