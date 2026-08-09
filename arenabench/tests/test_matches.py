# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The committed match files under ``matches/``, checked as configuration.

These files are launched — locally by an operator and, through
``arenabench cloud run``, onto paid AWS Batch capacity. Until now nothing read
them outside a run, so every mistake in one was discovered by spending money
on it: a template that does not parse fails at launch, and the more expensive
shape fails *nowhere at all* — a knob the seat's agent quietly ignores, or a
pipeline role pinned on an arm that runs no pipeline. Both produce a complete
run with plausible numbers, described by a file that says it measured
something else.

The suite deliberately reads the directory rather than a list: a match file
nobody added here would otherwise be the one that ships unchecked.
"""

from __future__ import annotations

import tomllib
from pathlib import Path

import pytest

from arenabench.agents import resolve_agent
from arenabench.config import match_from_toml
from arenabench.model import MatchSpec

MATCHES = Path(__file__).resolve().parent.parent / "matches"


def _match_files() -> list[Path]:
    files = sorted(MATCHES.glob("*.toml"))
    assert files, f"no match templates under {MATCHES}"
    return files


def _spec(path: Path) -> MatchSpec:
    return match_from_toml(tomllib.loads(path.read_text(encoding="utf-8")))


@pytest.mark.parametrize("path", _match_files(), ids=lambda p: p.name)
class TestCommittedMatches:
    def test_parses(self, path: Path) -> None:
        """`match_from_toml` refuses a bad template; that refusal belongs here,
        not at the moment a submission has already reserved capacity."""
        spec = _spec(path)
        assert spec.contestants, f"{path.name} seats nobody"
        assert spec.dataset

    def test_every_declared_knob_is_one_its_agent_honours(self, path: Path) -> None:
        """A setting the agent ignores is a claim the run cannot support.

        `unhonoured` reports only knobs the operator actually set, so this
        fails exactly when a file describes a configuration its seat never
        received — a `base_url` on an agent with nowhere to put one, a
        `budget_usd` on an agent that honours no spend cap.
        """
        for seat in _spec(path).contestants:
            missed = resolve_agent(seat.agent).unhonoured(seat.engine)
            assert not missed, f"{path.name}: seat {seat.id!r} sets {missed}"

    def test_a_bare_loop_seat_pins_no_pipeline_roles(self, path: Path) -> None:
        """`bare_loop` settles triage, witness and verify at once, because all
        three ARE the pipeline. A role override beside it is read by nothing:
        the run is a single model, while the file — and the scoreboard's
        published identity — names three.

        Parse cannot catch this. `Engine` carries both fields independently
        and the runner simply never looks at `roles` once `STELLA_NO_PIPELINE`
        is set, so the mismatch survives all the way into the results.
        """
        for seat in _spec(path).contestants:
            if seat.engine.bare_loop:
                assert not seat.engine.roles, (
                    f"{path.name}: seat {seat.id!r} runs the bare loop but pins "
                    f"roles {sorted(seat.engine.roles)}"
                )
