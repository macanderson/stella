# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""A cloud run reassembles into ONE match, without losing a single trial.

``arenabench cloud run`` submits one Batch job per (attempt, task, seat), so a
10-task 2-attempt run fetches down as twenty independent contests and the
89-task two-arm shape as 178 of them. These tests pin the fold back:

* the assembled directory is **one** match carrying every task and every seat;
* every per-trial artifact is still reachable afterwards, both at its original
  path and through the match — the traces are what every bench conclusion in
  this project comes from, so an assembler that summarised them away would be
  worse than no assembler at all; and
* the thing that comes out is what ``arenabench serve`` already knows how to
  open, proven by restoring it through the real ``MatchRunner`` rather than by
  asserting on the shape of a JSON file.

Everything here is hermetic: the fetched tree is built in ``tmp_path`` and no
test touches S3.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from arenabench.assemble import (
    ASSEMBLY_NAME,
    AssembleError,
    assemble_run,
    discover_trials,
    link_name,
    match_id_for,
    seat_for_job_dir,
)
from arenabench.registry import DEFAULT_REGISTRY
from arenabench.runner import MatchRunner

TASKS = ("nginx-request-logging", "fix-git", "regex-log")
SEATS = (
    ("stella", "Stella (bare loop)", "stella-bare-loop"),
    ("claude-code", "Claude Code", "claude-code"),
)
#: The spec id the container recorded — deliberately different from the run
#: directory name, because that is the real situation: two runs of one template
#: share a spec id and must not share a match directory.
SLICE_MATCH_ID = "2ebd602f8122"

MATCH_TOML = """
[match]
name = "assembly witness"
dataset = "terminal-bench-2.1"
sut_ref = ""
tasks = [{tasks}]
attempts = 2
concurrency = 1

[[contestant]]
id = "stella"
name = "Stella (bare loop)"
agent = "stella"

  [contestant.engine]
  api = "openrouter"
  model = "anthropic/claude-sonnet-5"
  bare_loop = true

[[contestant]]
id = "claude-code"
name = "Claude Code"
agent = "claude-code"

  [contestant.engine]
  api = "anthropic"
  model = "claude-sonnet-5"
""".format(tasks=", ".join(f'"{task}"' for task in TASKS))


def _write_trial(
    run_dir: Path,
    job_id: str,
    slug: str,
    task: str,
    attempt: int,
    *,
    reward: float = 1.0,
) -> Path:
    """One Batch job's fetched artifact tree, exactly as `cloud fetch` lays it.

    The shape is the one observed in ``arenabench-cloud/<run>/<job-uuid>/`` on
    a real ``--artifacts`` fetch: a per-job ``results.json`` beside the whole
    container workspace under ``ws/``.
    """
    trial = (
        run_dir
        / job_id
        / "ws"
        / "matches"
        / SLICE_MATCH_ID
        / "jobs"
        / f"{SLICE_MATCH_ID}-{slug}"
        / f"{task}__a{attempt}xyz"
    )
    (trial / "agent").mkdir(parents=True)
    (trial / "verifier").mkdir(parents=True)
    (trial / "agent" / "stella-events.jsonl").write_text(
        json.dumps({"type": "step_usage", "role": "worker", "job": job_id}) + "\n",
        encoding="utf-8",
    )
    (trial / "verifier" / "reward.txt").write_text(f"{reward}\n", encoding="utf-8")
    (trial / "result.json").write_text(
        json.dumps({"verifier_result": {"reward": reward}}), encoding="utf-8"
    )
    match_root = trial.parent.parent.parent
    (match_root / "provenance.json").write_text(
        json.dumps({"schema": 1, "comparability_key": "tb2.1/harbor0.6.1"}),
        encoding="utf-8",
    )
    (trial.parent.parent / f"{SLICE_MATCH_ID}-{slug}.seat.json").write_text(
        json.dumps({"seat": slug}), encoding="utf-8"
    )
    (run_dir / job_id / "results.json").write_text(
        json.dumps({"match": {"id": SLICE_MATCH_ID}, "rows": []}), encoding="utf-8"
    )
    return trial


def fetched_run(root: Path, name: str = "s5b2") -> Path:
    """A whole fetched run: 3 tasks x 2 attempts x 2 seats = 12 Batch jobs."""
    run_dir = root / name
    run_dir.mkdir(parents=True)
    (run_dir / "match.toml").write_text(MATCH_TOML, encoding="utf-8")
    index = 0
    for attempt in (1, 2):
        for task in TASKS:
            for _agent, _name, slug in SEATS:
                _write_trial(run_dir, f"job-{index:03d}", slug, task, attempt)
                index += 1
    return run_dir


# --------------------------------------------------------------------------
# the pure decisions
# --------------------------------------------------------------------------


def test_the_match_id_is_the_run_directory_not_the_spec_id(tmp_path: Path) -> None:
    """Two runs of one template share a spec id; they must not share a match.

    Naming the assembled match after the spec would make the second run of a
    template silently shadow the first in the operator's arena — the failure
    this whole change exists to end, one directory deeper.
    """
    run_dir = fetched_run(tmp_path)
    assert match_id_for(run_dir) == "s5b2"
    assert match_id_for(run_dir) != SLICE_MATCH_ID


@pytest.mark.parametrize(
    ("job_dir", "expected"),
    [
        ("2ebd602f8122-stella-bare-loop", "stella-bare-loop"),
        ("2ebd602f8122-claude-code", "claude-code"),
        ("2ebd602f8122-unknown-seat", None),
    ],
)
def test_a_job_directory_names_its_seat(job_dir: str, expected: str | None) -> None:
    assert seat_for_job_dir(job_dir, ["stella-bare-loop", "claude-code"]) == expected


def test_the_longest_matching_slug_wins() -> None:
    """`stella` must never claim a directory belonging to `stella-bare`."""
    assert seat_for_job_dir("abc-stella-bare", ["stella", "stella-bare"]) == (
        "stella-bare"
    )


def test_a_colliding_trial_name_keeps_its_task() -> None:
    """Harbor's suffix is unique per container, not across a cloud fan-out.

    A collision must not drop a trial and must not change what task it is:
    every reader recovers the task with `rsplit("__", 1)`, so the
    disambiguator goes after the suffix.
    """
    name = link_name({"fix-git__abc"}, "fix-git__abc")
    assert name == "fix-git__abcx2"
    assert name.rsplit("__", 1)[0] == "fix-git"
    assert link_name({"fix-git__abc", "fix-git__abcx2"}, "fix-git__abc") == (
        "fix-git__abcx3"
    )


def test_discovery_reads_the_disk_not_the_submission_record(tmp_path: Path) -> None:
    """What was submitted and what came back are different questions.

    `jobs.json` answers the first; a cell can only be filled by the second.
    """
    run_dir = fetched_run(tmp_path)
    (run_dir / "jobs.json").unlink(missing_ok=True)
    trials = discover_trials(run_dir, ["stella-bare-loop", "claude-code"])
    assert len(trials) == 12
    assert {t.task for t in trials} == set(TASKS)
    assert {t.seat_slug for t in trials} == {"stella-bare-loop", "claude-code"}


# --------------------------------------------------------------------------
# assembly
# --------------------------------------------------------------------------


def test_a_fanned_out_run_assembles_into_exactly_one_match(tmp_path: Path) -> None:
    """Twelve Batch jobs, one match — the whole point.

    Before this, the same run produced twelve match records locally; at the
    89-task two-arm scale, 178.
    """
    run_dir = fetched_run(tmp_path)
    assembly = assemble_run(run_dir)

    matches = sorted((run_dir / "matches").iterdir())
    assert [p.name for p in matches] == ["s5b2"]
    assert assembly.trials == 12
    assert assembly.linked == {"stella-bare-loop": 6, "claude-code": 6}

    spec = json.loads((assembly.match_dir / "spec.json").read_text(encoding="utf-8"))
    assert spec["id"] == "s5b2"
    assert tuple(spec["tasks"]) == TASKS
    assert [c["slug"] for c in spec["contestants"]] == [
        "stella-bare-loop",
        "claude-code",
    ]


def test_every_per_trial_artifact_is_still_reachable(tmp_path: Path) -> None:
    """Assembly must not consume the traces it exists to make readable.

    Both halves are asserted: the original fetched path still holds the bytes,
    and the same bytes are reachable through the match. A summary that
    replaced them would be exactly the projection CLAUDE.md forbids drawing a
    bench conclusion from.
    """
    run_dir = fetched_run(tmp_path)
    assembly = assemble_run(run_dir)

    original = (
        run_dir
        / "job-000"
        / "ws"
        / "matches"
        / SLICE_MATCH_ID
        / "jobs"
        / f"{SLICE_MATCH_ID}-stella-bare-loop"
        / f"{TASKS[0]}__a1xyz"
        / "agent"
        / "stella-events.jsonl"
    )
    assert original.is_file(), "the fetched artifact was moved or destroyed"

    job_dir = assembly.match_dir / "jobs" / "s5b2-stella-bare-loop"
    linked = sorted(job_dir.iterdir())
    assert len(linked) == 6
    through_match = job_dir / f"{TASKS[0]}__a1xyz" / "agent" / "stella-events.jsonl"
    assert through_match.read_bytes() == original.read_bytes()
    assert (job_dir / f"{TASKS[0]}__a1xyz" / "verifier" / "reward.txt").is_file()

    manifest = json.loads(
        (assembly.match_dir / ASSEMBLY_NAME).read_text(encoding="utf-8")
    )
    assert len(manifest["cells"]) == 12
    cell = next(c for c in manifest["cells"] if c["trial"] == f"{TASKS[0]}__a1xyz")
    assert cell["job_id"] == "job-000"
    # The manifest is the audit trail from a cell back to the Batch job that
    # produced it — an inference from a directory name is not one.
    assert (assembly.match_dir / cell["artifacts"]).resolve() == original.parent.parent


def test_the_symlinks_are_relative_so_the_tree_can_be_moved(tmp_path: Path) -> None:
    """A fetched run gets copied to another machine to be read; it must work."""
    run_dir = fetched_run(tmp_path)
    assemble_run(run_dir)
    link = (
        run_dir / "matches" / "s5b2" / "jobs" / "s5b2-claude-code" / f"{TASKS[1]}__a1xyz"
    )
    import os

    assert not os.path.isabs(os.readlink(link))


def test_assembling_twice_is_idempotent(tmp_path: Path) -> None:
    """Re-running after a fix must not double every cell."""
    run_dir = fetched_run(tmp_path)
    assemble_run(run_dir)
    second = assemble_run(run_dir)
    job_dir = second.match_dir / "jobs" / "s5b2-stella-bare-loop"
    assert len(list(job_dir.iterdir())) == 6
    assert second.linked == {"stella-bare-loop": 6, "claude-code": 6}


def test_a_results_only_fetch_says_so_instead_of_assembling_nothing(
    tmp_path: Path,
) -> None:
    """Silence here would read as "the run had no trials", which is a lie."""
    run_dir = tmp_path / "resultsonly"
    (run_dir / "000-stella-bare-loop-fix-git").mkdir(parents=True)
    (run_dir / "000-stella-bare-loop-fix-git" / "results.json").write_text(
        "{}", encoding="utf-8"
    )
    (run_dir / "match.toml").write_text(MATCH_TOML, encoding="utf-8")
    assembly = assemble_run(run_dir)
    assert assembly.trials == 0
    assert any("--artifacts" in w for w in assembly.warnings)


def test_a_missing_whole_match_spec_refuses_rather_than_guesses(
    tmp_path: Path,
) -> None:
    """The assembled match is the submitted one or it is not published.

    Synthesising a spec from the slices would invent a contest nobody
    submitted — and it is the slices' disagreement that would be invented away.
    """
    run_dir = fetched_run(tmp_path)
    (run_dir / "match.toml").unlink()
    with pytest.raises(AssembleError, match="match.toml"):
        assemble_run(run_dir)


def test_a_task_with_no_trial_is_named_not_dropped(tmp_path: Path) -> None:
    run_dir = tmp_path / "partial"
    run_dir.mkdir()
    (run_dir / "match.toml").write_text(MATCH_TOML, encoding="utf-8")
    _write_trial(run_dir, "job-000", "stella-bare-loop", TASKS[0], 1)
    assembly = assemble_run(run_dir)
    assert assembly.trials == 1
    assert any(TASKS[1] in w and "no trial artifact" in w for w in assembly.warnings)


def test_a_missing_task_names_its_job_state_when_the_fetch_recorded_one(
    tmp_path: Path,
) -> None:
    """#3218: "no artifact" is two facts — a job still RUNNING at fetch time
    (the artifact is coming; re-fetch) and a terminal job that uploaded
    nothing (it never will). The h2h891 analysis read a short match as
    complete because the warning could not tell them apart."""
    run_dir = tmp_path / "partial"
    run_dir.mkdir()
    (run_dir / "match.toml").write_text(MATCH_TOML, encoding="utf-8")
    _write_trial(run_dir, "job-000", "stella-bare-loop", TASKS[0], 1)
    (run_dir / "jobs.json").write_text(
        json.dumps(
            {
                "run_id": "partial",
                "jobs": [
                    {"id": "a", "task": TASKS[0], "state": "SUCCEEDED"},
                    {"id": "b", "task": TASKS[1], "state": "RUNNING"},
                    {"id": "c", "task": TASKS[2], "state": "FAILED"},
                ],
            }
        ),
        encoding="utf-8",
    )
    assembly = assemble_run(run_dir)
    warning = next(w for w in assembly.warnings if "no trial artifact" in w)
    assert f"{TASKS[1]} (still RUNNING in Batch at fetch time — re-fetch)" in warning
    assert f"{TASKS[2]} (terminal, no artifact uploaded)" in warning


def test_a_copied_match_survives_its_fetch_directory_being_deleted(
    tmp_path: Path,
) -> None:
    """#3261: a symlink-only match is only as durable as a scratch folder
    nothing owns — two loaded matches were found with every trial link
    dangling while the match still listed its arms. --copy buys a match that
    is actually a match."""
    import shutil

    run_dir = fetched_run(tmp_path / "fetched")
    workspace = tmp_path / "arena"
    assembly = assemble_run(run_dir, workspace=workspace, copy=True)
    assert assembly.trials == 12

    shutil.rmtree(run_dir)

    jobs_root = assembly.match_dir / "jobs"
    trial_dirs = [
        entry
        for job in jobs_root.iterdir()
        if job.is_dir()
        for entry in job.iterdir()
    ]
    assert trial_dirs, "the copied match lost its cells"
    for entry in trial_dirs:
        assert not entry.is_symlink()
        assert (entry / "result.json").is_file(), f"{entry} is not readable"


def test_the_manifest_names_its_source_and_recovery(tmp_path: Path) -> None:
    run_dir = fetched_run(tmp_path / "fetched")
    (run_dir / "jobs.json").write_text(
        json.dumps({"run_id": "s5b2", "jobs": []}), encoding="utf-8"
    )
    assembly = assemble_run(run_dir, workspace=tmp_path / "arena")
    manifest = json.loads((assembly.match_dir / "assembly.json").read_text())
    assert manifest["copied"] is False
    assert manifest["source"]["run_id"] == "s5b2"
    assert "cloud fetch s5b2 --artifacts" in manifest["source"]["recover"]


def test_reassembling_a_copied_match_without_copy_keeps_the_copies(
    tmp_path: Path,
) -> None:
    run_dir = fetched_run(tmp_path / "fetched")
    workspace = tmp_path / "arena"
    assemble_run(run_dir, workspace=workspace, copy=True)
    assembly = assemble_run(run_dir, workspace=workspace)
    jobs_root = assembly.match_dir / "jobs"
    for job in jobs_root.iterdir():
        if not job.is_dir():
            continue
        for entry in job.iterdir():
            assert not entry.is_symlink(), (
                "a durable copy must not be downgraded to a link that can "
                "dangle"
            )


# --------------------------------------------------------------------------
# the assembled match is one `arenabench serve` opens
# --------------------------------------------------------------------------


def test_the_assembled_match_opens_as_one_contest(tmp_path: Path) -> None:
    """The witness: restore through the real server path, not a shape assert.

    `MatchRunner.restore_from_disk` is what `arenabench serve` runs, so this
    fails if the assembled layout is anything the arena cannot open — and the
    snapshot it produces is the grid the operator sees: one row per task, one
    column per seat, attempts already collapsed to a representative by the same
    measured-beats-void rule a local run uses.
    """
    run_dir = fetched_run(tmp_path)
    assemble_run(run_dir)

    runner = MatchRunner(DEFAULT_REGISTRY, run_dir)
    assert runner.restore_from_disk() == 1

    match = runner.matches["s5b2"]
    snapshot = match.snapshot()
    assert [row["task"] for row in snapshot["rows"]] == list(TASKS)
    for row in snapshot["rows"]:
        assert set(row["cells"]) == {"stella", "claude-code"}
        for cell in row["cells"].values():
            assert cell is not None, "a linked trial produced no cell"
    for entry in snapshot["contestants"]:
        assert entry["totals"]["trials"] == len(TASKS)


def test_the_events_of_an_assembled_cell_are_servable(tmp_path: Path) -> None:
    """The transcript view reads `agent/stella-events.jsonl` off the trial dir."""
    run_dir = fetched_run(tmp_path)
    assemble_run(run_dir)
    runner = MatchRunner(DEFAULT_REGISTRY, run_dir)
    runner.restore_from_disk()
    match = runner.matches["s5b2"]
    events = match.events_path("stella", TASKS[0])
    assert events is not None and events.is_file()
    assert "step_usage" in events.read_text(encoding="utf-8")
