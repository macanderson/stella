# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The cloud scoreboard's pure half — no AWS account, no network."""

from __future__ import annotations

import io
import json
from typing import Any

from arenabench.cloud_watch import (
    Cell,
    CloudScoreboard,
    assemble,
    cell_for,
    render_html,
    reward_of,
)


def _results(reward: float | None, *, finished: bool = True) -> dict:
    body: dict = {"verifier_result": {"rewards": {}}}
    if reward is not None:
        body["verifier_result"]["rewards"]["reward"] = reward
    if finished:
        body["finished_at"] = "2026-08-08T00:00:00Z"
    return body


def test_the_verifier_reward_is_the_score() -> None:
    assert reward_of(_results(1.0)) == 1.0
    assert reward_of(_results(0.0)) == 0.0


def test_a_trial_still_in_flight_reports_nothing_rather_than_zero() -> None:
    """The confusion this whole module is careful about: an unknown is not a
    loss. A fabricated zero is indistinguishable from a real one."""
    assert reward_of(_results(None, finished=False)) is None
    assert reward_of(None) is None
    assert reward_of({}) is None


def test_an_attempted_trial_with_no_reward_is_a_real_zero() -> None:
    assert reward_of(_results(None, finished=True)) == 0.0
    assert reward_of({"exception_info": {"exception_type": "AgentTimeoutError"}}) == 0.0


def test_a_verifier_timeout_with_no_reward_is_a_void_not_a_zero() -> None:
    """#3264: the grader ran out of time on a trial the agent may well have
    solved. Scoring the absence as 0.0 blames the agent for the harness —
    selectively on the heaviest tasks. Observed on run preregA2,
    torch-tensor-parallelism: agent completed in ~200s of a 900s budget,
    VerifierTimeoutError, no reward on disk."""
    dead_grader = {
        "exception_info": {"exception_type": "VerifierTimeoutError"},
        "finished_at": "2026-08-14T00:00:00Z",
    }
    assert reward_of(dead_grader) is None
    cell = cell_for("FAILED", dead_grader)
    assert cell.state == "void"
    assert "VerifierTimeoutError" in cell.detail
    # The whole infrastructure family voids the same way, and an agent-named
    # exception does not.
    assert reward_of({"exception_info": {"exception_type": "HealthcheckError"}}) is None
    assert (
        reward_of({"exception_info": {"exception_type": "NonZeroAgentExitCodeError"}})
        == 0.0
    )


def test_a_boolean_is_not_a_score() -> None:
    """`bool` is an `int` in Python, so a flag would silently read as 0.0/1.0."""
    assert reward_of({"verifier_result": {"rewards": {"reward": True}}}) is None


def test_the_harbor_envelope_is_accepted_and_averaged() -> None:
    envelope = {"results": [_results(1.0), _results(0.0)]}
    assert reward_of(envelope) == 0.5


def test_a_failed_container_that_still_scored_keeps_its_reward() -> None:
    """Harbor records a non-zero agent exit as a loss and Stella's verdict
    exits non-zero by design — reading the exit code instead of the reward
    systematically under-counts Stella."""
    cell = cell_for("FAILED", _results(1.0))
    assert cell.reward == 1.0
    assert cell.state == "won"


def test_a_succeeded_container_with_no_results_is_unknown_not_zero() -> None:
    """An infrastructure death is not an agent loss and must not average
    into one."""
    cell = cell_for("SUCCEEDED", None)
    assert cell.reward is None
    assert cell.state == "unknown"


def test_queued_and_running_are_distinguished() -> None:
    assert cell_for("RUNNABLE", None).state == "queued"
    assert cell_for("RUNNING", None).state == "running"


def _jobs() -> list[dict]:
    return [
        {"id": "j0", "label": "000-cc-fix-git", "contestant_id": "claude-code",
         "task": "fix-git"},
        {"id": "j1", "label": "001-stella-fix-git", "contestant_id": "stella",
         "task": "fix-git"},
        {"id": "j2", "label": "002-cc-regex-log", "contestant_id": "claude-code",
         "task": "regex-log"},
        {"id": "j3", "label": "003-stella-regex-log", "contestant_id": "stella",
         "task": "regex-log"},
    ]


def test_assemble_builds_the_grid_and_totals_only_scored_cells() -> None:
    board = assemble(
        "contest-1",
        _jobs(),
        {"j0": "SUCCEEDED", "j1": "SUCCEEDED", "j2": "FAILED", "j3": "RUNNING"},
        {
            "000-cc-fix-git": _results(1.0),
            "001-stella-fix-git": _results(1.0),
            "002-cc-regex-log": _results(0.0),
            # j3 is still running: no results at all.
        },
    )
    assert board.contestants == ["claude-code", "stella"]
    assert board.tasks == ["fix-git", "regex-log"]
    assert board.totals() == {"claude-code": 1.0, "stella": 1.0}
    assert board.settled() == 3
    assert board.cells[("regex-log", "stella")].state == "running"


def test_render_is_self_contained_and_escapes_its_inputs() -> None:
    board = assemble(
        "<script>alert(1)</script>",
        [{"id": "j", "label": "l", "contestant_id": "stella", "task": "t&t"}],
        {"j": "RUNNING"},
        {},
    )
    page = render_html(board)
    assert "<script>alert(1)</script>" not in page
    assert "&lt;script&gt;" in page
    assert "t&amp;t" in page
    # No CDN, no build step — an artifact that needs the network to render is
    # useless on a rig that has none.
    assert "http://" not in page.replace("http-equiv", "")
    assert "https://" not in page


def test_a_cell_with_no_reward_never_prints_a_number() -> None:
    assert Cell(state="queued").text == "QUEUED"
    assert Cell(state="won", reward=1.0).text == "✓ 1.0"


class _FakeS3:
    """An S3 holding exactly the keys the runner writes, and nothing else.

    ``get_object`` raising on an unknown key is the fidelity that matters: the
    scoreboard treats any exception as "not written yet", so a fake that
    returned an empty body for a missing key would hide the bug this proves.
    """

    def __init__(self, objects: dict[str, bytes]) -> None:
        self.objects = objects
        self.asked: list[str] = []

    def get_object(self, Bucket: str, Key: str) -> dict:  # noqa: N803 - boto3 spelling
        self.asked.append(Key)
        if Key not in self.objects:
            raise KeyError(Key)
        return {"Body": io.BytesIO(self.objects[Key])}


class _FakeExecutor:
    def __init__(self, s3: _FakeS3, jobs: list[dict], states: dict[str, str]) -> None:
        self.s3 = s3
        self.jobs = jobs
        self.states = states

    def load_jobs(self, run_id: str) -> dict:
        return {"jobs": self.jobs}

    def job_states(self, ids: Any) -> dict[str, str]:
        return self.states

    def run_rows(self, run_id: str) -> list[dict]:
        return []

    def bucket(self) -> str:
        return "arenabench-artifacts-test"

    def _client(self, name: str) -> _FakeS3:
        return self.s3


def test_the_scoreboard_finds_a_result_written_under_the_job_id() -> None:
    """The live watch path must address S3 by job id, not by trial label.

    ``entrypoint.sh`` writes ``runs/<run>/$JOB_ID/results.json`` where
    ``JOB_ID`` is the Batch job's UUID; the label only ever names the trial's
    directory on disk after a fetch. Reading by label asks for a key nothing
    writes, and the miss is swallowed as "still running" — so a whole run
    renders empty and never says why.

    The fixture's ids and labels are deliberately different strings. Make them
    equal and this test passes against the bug.
    """
    jobs = _jobs()
    s3 = _FakeS3(
        {
            "runs/contest-1/j0/results.json": json.dumps(_results(1.0)).encode(),
            "runs/contest-1/j1/results.json": json.dumps(_results(0.0)).encode(),
        }
    )
    board = CloudScoreboard(
        _FakeExecutor(s3, jobs, {j["id"]: "SUCCEEDED" for j in jobs}),
        "contest-1",
    ).fetch()

    assert board.cells[("fix-git", "claude-code")].reward == 1.0
    assert board.cells[("fix-git", "stella")].reward == 0.0
    assert board.settled() == 2

    # And it asked for the keys that exist, rather than finding them by luck.
    assert "runs/contest-1/j0/results.json" in s3.asked
    assert not [k for k in s3.asked if "cc-fix-git" in k]
