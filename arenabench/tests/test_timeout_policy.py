# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The submit-time banner for a non-stock `agent_timeout_multiplier` (#3256).

A sibling of `test_arena.py` rather than more lines in it — that file sits at
1479/1500 lines, within a page of the file-size ceiling.

Every recorded Terminal-Bench 2.1 number to date used a 2x agent timeout,
which tbench.ai's "submissions may not modify timeouts or resources" rule
makes non-comparable to a published leaderboard row (`docs/timeout-policy.md`).
`_launch` is the one place an operator is looking at the moment the choice is
made, so this is where the run gets marked non-submittable rather than leaving
it to be rediscovered when a number is quoted later.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from arenabench.model import MatchSpec
from arenabench.preflight import non_stock_timeout_notice
from arenabench.registry import DEFAULT_REGISTRY
from arenabench.runner import MatchRunner


def test_the_stock_multiplier_produces_no_notice() -> None:
    assert non_stock_timeout_notice(1.0) is None


def test_a_raised_multiplier_names_itself_and_the_leaderboard() -> None:
    notice = non_stock_timeout_notice(2.0)
    assert notice is not None
    assert "agent_timeout_multiplier=2.0" in notice
    assert "non-submittable" in notice
    assert "tbench.ai" in notice


def _fake_harbor(monkeypatch: pytest.MonkeyPatch) -> None:
    """Stand in for an installed Harbor — see `test_arena.py`'s `_fake_harbor`
    for the full reasoning; this is the minimal subset `_launch` needs to
    reach argv construction without touching a real binary."""
    monkeypatch.setattr(
        "arenabench.harbor.harbor_bin", lambda dataset_key=None: "/usr/bin/harbor"
    )
    monkeypatch.setattr("arenabench.harbor.harbor_version", lambda binary=None: "0.20.0")
    monkeypatch.setattr(
        "arenabench.harbor.supports_agent_import_path", lambda binary=None: False
    )
    monkeypatch.setattr(
        "arenabench.adapter.stella_seat_problem", lambda binary=None, **_kw: None
    )


class _FakePopen:
    def __init__(self, command: list[str], **_kwargs: object) -> None:
        self.command = command
        self.returncode = None

    def poll(self) -> None:
        return None


def _launch_with_multiplier(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, multiplier: float
):
    monkeypatch.setenv("ARENABENCH_DATASETS", str(tmp_path / "nope"))
    _fake_harbor(monkeypatch)
    monkeypatch.setattr("arenabench.runner.subprocess.Popen", _FakePopen)
    spec = MatchSpec.from_json(
        {
            "dataset": "terminal-bench-2.1",
            "tasks": ["alpha"],
            "sut_ref": "",
            "agent_timeout_multiplier": multiplier,
            "contestants": [
                {
                    "name": "s",
                    "agent": "stella",
                    "engine": {"api": "openrouter", "model": "x/y"},
                }
            ],
        }
    )
    runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / "ws")
    match = runner.create(spec)
    return runner._launch(match, spec.contestants[0], runner.resolve_harbor(match))


def test_a_non_stock_multiplier_notes_the_run_as_non_submittable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    run = _launch_with_multiplier(tmp_path, monkeypatch, 2.0)
    assert any(
        "non-submittable" in note and "agent_timeout_multiplier=2.0" in note
        for note in run.notes
    ), run.notes


def test_a_stock_multiplier_records_no_banner(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    run = _launch_with_multiplier(tmp_path, monkeypatch, 1.0)
    assert not any("submittable" in note for note in run.notes), run.notes
