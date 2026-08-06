# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The flip search, and the manifest it reads.

Everything here is the pure half — no Docker, no containers. The expensive
half is one `docker run` per probe and is exercised by running a real match;
what must be correct without a container is *which* snapshots get probed and
what is concluded from the answers, because a wrong index here silently moves
the number an operator reads.
"""

from __future__ import annotations

import json

from arenabench.model import MatchSpec
from arenabench.snapshot import (
    SNAPSHOT_DIRNAME,
    SnapshotEntry,
    bisect_first_pass,
    confirm_first_pass,
    container_for_trial,
    load_manifest,
    read_task_image,
    summarise_flip,
    unpatched_indices,
)


def _entries(count: int, *, step: float = 30.0) -> list[SnapshotEntry]:
    return [
        SnapshotEntry(
            index=i,
            at=1000.0 + i * step,
            sha=f"sha{i}",
            patch=f"{i:04d}.patch",
            elapsed=i * step,
        )
        for i in range(count)
    ]


# -- the search -------------------------------------------------------------


def test_bisect_finds_the_first_passing_snapshot():
    # Passes from index 6 onwards.
    calls: list[int] = []

    def probe(i: int) -> bool:
        calls.append(i)
        return i >= 6

    assert bisect_first_pass(20, probe) == 6
    # log2(20) is ~4.3; a scan would be 7 probes to reach index 6 alone.
    assert len(calls) <= 6


def test_bisect_returns_none_when_nothing_passes():
    assert bisect_first_pass(16, lambda i: False) is None


def test_bisect_handles_a_single_snapshot():
    assert bisect_first_pass(1, lambda i: True) == 0
    assert bisect_first_pass(1, lambda i: False) is None
    assert bisect_first_pass(0, lambda i: True) is None


def test_unknown_probe_does_not_read_as_failure():
    """A verifier that could not run must not pin the search.

    Scoring an un-runnable probe as a failure would push the answer later than
    the truth and silently inflate "wasted" time.
    """

    def probe(i: int) -> bool | None:
        if i == 8:
            return None
        return i >= 4

    assert bisect_first_pass(16, probe) == 4


def test_confirm_walks_back_past_a_broken_then_fixed_solution():
    """Bisection cannot see a solution that was broken and re-fixed.

    Pass at 2, broken at 3-5, passing again from 6. A bisection can land on 6;
    the confirming walk must find the earlier 2.
    """
    passing = {2, 6, 7, 8, 9}
    assert confirm_first_pass(6, lambda i: i in passing, window=1) == 6
    # A window wide enough to cross the broken stretch reaches the real flip.
    passing_contiguous = {4, 5, 6, 7}
    assert confirm_first_pass(6, lambda i: i in passing_contiguous, window=3) == 4


def test_confirm_stops_at_zero():
    assert confirm_first_pass(1, lambda i: True, window=5) == 0


# -- summarising ------------------------------------------------------------


def test_summarise_reports_the_time_spent_after_passing():
    entries = _entries(10)  # 0..270s
    result = summarise_flip(entries, 3, probes=4, unknown=0)
    assert result.flip_elapsed == 90.0
    assert result.wasted_elapsed == 180.0
    assert result.snapshots == 10


def test_summarise_with_no_flip_reports_no_waste():
    result = summarise_flip(_entries(5), None, probes=3, unknown=1)
    assert result.flip_index is None
    assert result.wasted_elapsed is None
    assert result.unknown == 1
    assert json.loads(json.dumps(result.to_json()))["snapshots"] == 5


def test_summarise_ignores_an_out_of_range_index():
    result = summarise_flip(_entries(3), 9, probes=1, unknown=0)
    assert result.flip_elapsed is None


# -- manifest ---------------------------------------------------------------


def test_manifest_round_trips_and_sorts(tmp_path):
    snap = tmp_path / SNAPSHOT_DIRNAME
    snap.mkdir(parents=True)
    written = _entries(3)
    with (snap / "manifest.jsonl").open("w", encoding="utf-8") as handle:
        for entry in reversed(written):  # out of order on disk
            handle.write(json.dumps(entry.to_json()) + "\n")
    loaded = load_manifest(tmp_path)
    assert [e.index for e in loaded] == [0, 1, 2]
    assert loaded[1].sha == "sha1"


def test_manifest_survives_a_torn_last_line(tmp_path):
    """The watcher appends while the run is live; the tail can be half-written."""
    snap = tmp_path / SNAPSHOT_DIRNAME
    snap.mkdir(parents=True)
    good = json.dumps(_entries(1)[0].to_json())
    (snap / "manifest.jsonl").write_text(good + "\n{\"index\": 1, \"at\":", encoding="utf-8")
    assert [e.index for e in load_manifest(tmp_path)] == [0]


def test_manifest_missing_is_empty_not_an_error(tmp_path):
    assert load_manifest(tmp_path) == []


def test_the_workspace_round_trips(tmp_path):
    """Replay must apply a patch where it was taken, not where it guesses.

    Capture probes a container the agent has lived in; replay probes a fresh
    one from the pristine image, where a directory the agent created does not
    exist. When the two disagreed, the patch applied cleanly to the wrong
    directory and every probe read "did not pass" — reported as
    `flip_index=None`, i.e. "the agent never solved it".
    """
    snap = tmp_path / SNAPSHOT_DIRNAME
    snap.mkdir(parents=True)
    entry = SnapshotEntry(
        index=0, at=1.0, sha="abc", patch="0000.patch", elapsed=0.0, workspace="/app"
    )
    (snap / "manifest.jsonl").write_text(
        json.dumps(entry.to_json()) + "\n", encoding="utf-8"
    )
    assert load_manifest(tmp_path)[0].workspace == "/app"


def test_a_manifest_without_a_workspace_still_loads(tmp_path):
    """Manifests written before the field existed must keep working."""
    snap = tmp_path / SNAPSHOT_DIRNAME
    snap.mkdir(parents=True)
    (snap / "manifest.jsonl").write_text(
        json.dumps(
            {"index": 0, "at": 1.0, "sha": "abc", "patch": None, "elapsed": 0.0}
        )
        + "\n",
        encoding="utf-8",
    )
    loaded = load_manifest(tmp_path)
    assert len(loaded) == 1
    assert loaded[0].workspace == ""  # replay falls back to probing


def test_unpatched_indices_flags_snapshots_identical_to_the_first():
    entries = list(_entries(3))
    entries[0] = SnapshotEntry(index=0, at=1.0, sha="a", patch=None, elapsed=0.0)
    assert unpatched_indices(entries) == [0]


# -- task + container resolution -------------------------------------------


def test_read_task_image(tmp_path):
    (tmp_path / "task.toml").write_text(
        '[environment]\ndocker_image = "alexgshaw/sqlite-with-gcov:20251031"\ncpus = 1\n',
        encoding="utf-8",
    )
    assert read_task_image(tmp_path) == "alexgshaw/sqlite-with-gcov:20251031"


def test_read_task_image_missing(tmp_path):
    assert read_task_image(tmp_path) is None


def test_container_name_is_lowercased(tmp_path):
    """Harbor lowercases trial ids for Docker; the mapping must match exactly."""
    trial = tmp_path / "sqlite-with-gcov__n4nJCiw"
    assert container_for_trial(trial) == "sqlite-with-gcov__n4njciw-main-1"


# -- spec plumbing ----------------------------------------------------------


def test_match_spec_round_trips_snapshot_fields():
    spec = MatchSpec.from_json(
        {
            "name": "m",
            "dataset": "terminal-bench-2.1",
            "tasks": ["fix-git"],
            "contestants": [],
            "capture_snapshots": True,
            "snapshot_interval": 15.0,
        }
    )
    assert spec.capture_snapshots is True
    assert spec.snapshot_interval == 15.0
    assert MatchSpec.from_json(spec.to_json()).snapshot_interval == 15.0


def test_snapshot_interval_never_spins_the_watcher():
    """A zero interval would make the sweep a busy loop; it is clamped."""
    spec = MatchSpec.from_json(
        {"dataset": "d", "contestants": [], "snapshot_interval": 0}
    )
    assert spec.snapshot_interval >= 1.0


def test_capture_is_off_by_default():
    spec = MatchSpec.from_json({"dataset": "d", "contestants": []})
    assert spec.capture_snapshots is False
