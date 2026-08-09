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
from arenabench.model import Engine, MatchSpec

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
        received — a `base_url` on an agent with nowhere to put one, or a
        per-role pipeline on an agent that runs a single loop.
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

    def test_no_committed_template_declares_a_per_trial_ceiling(
        self, path: Path
    ) -> None:
        """The regression this repository actually shipped, checked on the files.

        All four of these templates once carried the same asymmetry: a Claude
        Code seat whose comment said the comparator honours no spend cap, and a
        Stella seat a few lines below declaring `max_tokens` and `budget_usd`.
        Match `5292a68cdabf` ran that shape, and all three of Stella's losses
        were the budget guard firing rather than the agent failing (#2411).

        `_engine_from_toml` refuses the keys now, so a template carrying one
        fails `_spec` before reaching this assertion. What this adds is a check
        on the *text*, which still catches the key arriving inside a table the
        loader has not been taught to walk.
        """
        offenders = [
            f"line {number}: {line.strip()}"
            for number, line in enumerate(
                path.read_text(encoding="utf-8").splitlines(), start=1
            )
            # A commented mention is the file explaining the rule rather than
            # breaking it, and every one of these templates now carries such a
            # sentence.
            if not line.lstrip().startswith("#")
            and ("budget_usd" in line or "max_tokens" in line)
        ]
        assert not offenders, f"{path.name} declares a per-trial ceiling: {offenders}"


class TestPerTrialCeilingsAreRefused:
    """#2411: a ceiling one seat carries and the other does not is a handicap.

    The witness for the change. Before it, every declaration below parsed into
    an `Engine` field, was exported to the agent, and decided matches.
    """

    def _template(self, engine_extra: dict, *, role: bool = False) -> dict:
        engine: dict = {"api": "openrouter", "model": "z-ai/glm-5.2"}
        if role:
            engine["roles"] = {"worker": engine_extra}
        else:
            engine.update(engine_extra)
        return {
            "match": {"dataset": "terminal-bench-2.1"},
            "contestant": [
                {"id": "stella", "name": "Stella", "agent": "stella", "engine": engine}
            ],
        }

    @pytest.mark.parametrize(
        "key, value", [("budget_usd", 1.0), ("max_tokens", 64000)]
    )
    def test_an_engine_level_ceiling_is_refused_by_name(
        self, key: str, value: object
    ) -> None:
        with pytest.raises(Exception) as caught:
            match_from_toml(self._template({key: value}))
        assert key in str(caught.value)

    def test_a_role_level_output_cap_is_refused_too(self) -> None:
        """The hole an engine-level check alone would leave open."""
        with pytest.raises(Exception) as caught:
            match_from_toml(self._template({"max_tokens": 64000}, role=True))
        assert "max_tokens" in str(caught.value)

    def test_an_empty_ceiling_is_still_refused(self) -> None:
        """`budget_usd = ""` reads as "no cap" to a loader and as a live knob to
        the next person editing the file, so the word is what gets refused."""
        with pytest.raises(Exception) as caught:
            match_from_toml(self._template({"budget_usd": ""}))
        assert "budget_usd" in str(caught.value)

    def test_an_archived_spec_that_ran_capped_still_loads(self) -> None:
        """Refusal is for authoring; reading history is not authoring.

        `runner.py` restores a match from its `spec.json`, and the specs worth
        reading most are precisely the ones that ran under a cap —
        `5292a68cdabf` is the evidence for this change. Refusing them would
        make the argument unreadable in support of its own conclusion.
        """
        engine = Engine.from_json(
            {
                "api": "openrouter",
                "model": "z-ai/glm-5.2",
                "budget_usd": 1.0,
                "max_tokens": 32000,
            }
        )
        assert engine.model == "z-ai/glm-5.2"
        assert not hasattr(engine, "budget_usd")
        assert "budget_usd" not in engine.to_json()
        assert "max_tokens" not in engine.to_json()
