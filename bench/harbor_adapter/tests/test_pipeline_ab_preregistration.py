"""The pipeline A/B's committed plan against the adapter that will run it.

`bench/evidence/pipeline-ab/preregistration.json` fixes the two arms of the
run `doc:pipeline-as-plugins` §7 owes, before any money is spent. Two of the
values it fixes are not facts about the experiment at all — they are facts
about this adapter: the `loop_mode` string each arm's trials will carry, which
`compare_arms.py --treatment-fired` reads to prove the treatment arm ran the
path under test and the control arm did not.

A preregistration is worth what it can still be checked against. Written out
by hand it is two strings in a JSON file, and `loop_mode_name` can move
underneath them with nothing to notice: the run then launches with a
`--treatment-fired` argument that matches no trial, and every treatment trial
is refused for not reporting a path it did report. So the plan's two strings
are held here to what the adapter actually returns for each arm's declared
`STELLA_PIPELINE` value.

It lives in this suite rather than beside the analysis tests because the
question is about the adapter. `bench/evidence/tests` runs `--no-project`, so
recomputing a published score never needs the harness installed; importing
`stella_harbor` there would cost that.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor.loop_mode import PIPELINE_ENV, loop_mode_name  # noqa: E402

#: Repository root: this file is bench/harbor_adapter/tests/<name>.py.
_REPO_ROOT = Path(__file__).resolve().parents[3]

_PREREGISTRATION = _REPO_ROOT / "bench" / "evidence" / "pipeline-ab" / "preregistration.json"


def _plan() -> dict:
    return json.loads(_PREREGISTRATION.read_text())


def _declared_pipeline_id(arm: dict) -> str:
    """The `--pipeline` id an arm's `invocation` names.

    The plan states each arm as the command a person types, which is the form
    a reader checks. `STELLA_PIPELINE` is how the adapter is told the same
    thing, so the id is read back out of the invocation rather than recorded
    twice.
    """
    tokens = arm["invocation"].split()
    assert "--pipeline" in tokens, f"arm invocation names no pipeline: {arm['invocation']!r}"
    return tokens[tokens.index("--pipeline") + 1]


def _reader(pipeline: str):
    def read(key: str) -> str | None:
        return pipeline if key == PIPELINE_ENV else None

    return read


class TestLoopModeValues:
    """Each arm's declared `loop_mode` is what this adapter will publish."""

    @pytest.mark.parametrize("arm_name", ["control", "treatment"])
    def test_the_declared_value_is_what_the_adapter_publishes(self, arm_name: str) -> None:
        plan = _plan()
        declared = plan["to_be_recorded_before_the_first_paid_call"][
            "loop_mode_field_values"
        ][arm_name]
        pipeline_id = _declared_pipeline_id(plan["arms"][arm_name])

        assert declared == loop_mode_name(_reader(pipeline_id)), (
            f"the {arm_name} arm's preregistered loop_mode value differs "
            "from what loop_mode_name returns for its declared "
            "--pipeline id. Launching on this plan would refuse every "
            "treatment trial for not reporting a path it did report."
        )

    def test_the_two_arms_are_told_apart_by_the_value(self) -> None:
        """`--treatment-fired` can only separate two distinct strings.

        `check_path_fired` asks whether each treatment trial carries the
        declared value and whether any control trial does. One string for
        both arms makes every trial pass the first test and every control
        trial fail the second, so the check reports nothing about the run.
        """
        values = _plan()["to_be_recorded_before_the_first_paid_call"][
            "loop_mode_field_values"
        ]
        assert values["control"] != values["treatment"]

    def test_the_analysis_mode_names_the_treatment_value(self) -> None:
        """The command in the plan is the command that will be run.

        `analysis.mode` is copied onto the launch line by whoever runs this,
        so a `--treatment-fired` argument spelling a different value than the
        arm below it is a refusal waiting to happen at the far end of a paid
        run.
        """
        plan = _plan()
        treatment = plan["to_be_recorded_before_the_first_paid_call"][
            "loop_mode_field_values"
        ]["treatment"]
        assert f"--treatment-fired loop_mode={treatment}" in plan["analysis"]["mode"]


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
