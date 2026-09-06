"""Tests for a comparison whose two arms are two builds.

One build cannot answer every question. To compare a path that was cut against
the path that took its place, you need a build of each. The default comparison
refuses that, and it is right to: an unnoticed split between two builds is the
cheapest way to get a number that looks fine and means nothing.

So the split is declared, and the declaration buys checks the default mode
cannot make. Each arm must report the commit it was declared on. The two builds
must differ. And the treatment arm must show, per trial, that it ran the path
under test. A build that *can* run a path is not a run that *did*.
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

_EVIDENCE = Path(__file__).resolve().parent.parent


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, _EVIDENCE / f"{name}.py")
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


compare_arms = _load("compare_arms")

OLD = "a" * 40
NEW = "b" * 40
PATH_UNDER_TEST = "wrapper:witness-v1"


def _cross(**overrides) -> "compare_arms.CrossSut":
    fields = {
        "control_commit": OLD,
        "treatment_commit": NEW,
        "fired_field": "loop_mode",
        "fired_value": PATH_UNDER_TEST,
    }
    fields.update(overrides)
    return compare_arms.CrossSut(**fields)


def _row(task: str, *, build: str, passed: bool, **extra) -> dict:
    """One trial row in the shape `score_dev_baseline.py extract` writes."""
    row = {
        "task_name": task,
        "trial_id": f"{task}__aaa",
        "accuracy": 1.0 if passed else 0.0,
        "reward": 1.0 if passed else 0.0,
        # The posture is held still across both builds, so both arms declare
        # the same assurance arm. It says which tiers the posture asks for. It
        # does not say which loop the binary ran.
        "assurance_arm": "witness-off",
        "source_commit": build,
        "binary_sha256": build[0] * 64,
        "engine_posture_sha256": "one-posture",
        "witness_author_model": None,
        "witness_authored_state": "unavailable",
        "witness_authored_count": 0,
        "self_verdict_passed": passed,
        "self_verdict_deterministic": False,
        "usd": 1.0,
    }
    row.update(extra)
    return row


def _control(task: str, *, passed: bool = False, **extra) -> dict:
    extra.setdefault("loop_mode", "staged_pipeline")
    return _row(task, build=OLD, passed=passed, **extra)


def _treatment(task: str, *, passed: bool = True, **extra) -> dict:
    extra.setdefault("loop_mode", PATH_UNDER_TEST)
    return _row(task, build=NEW, passed=passed, **extra)


def _arms(control_rows: list[dict], treatment_rows: list[dict]):
    return (
        {row["task_name"]: row for row in control_rows},
        {row["task_name"]: row for row in treatment_rows},
    )


def _write(path: Path, rows: list[dict]) -> Path:
    path.write_text("".join(json.dumps(row) + "\n" for row in rows))
    return path


# --------------------------------------------------------------------------
# The default mode still refuses a split it was not told about
# --------------------------------------------------------------------------


def test_two_builds_are_refused_when_nobody_declared_them() -> None:
    control, treatment = _arms([_control("a")], [_treatment("a")])

    report = compare_arms.compare(control, treatment, tasks=1)

    assert report["comparable"] is False
    assert any("source_commit" in reason for reason in report["refusals"])


# --------------------------------------------------------------------------
# Declared, and therefore checkable
# --------------------------------------------------------------------------


def test_a_declared_two_build_comparison_is_allowed_to_mean_something() -> None:
    control, treatment = _arms(
        [_control("a"), _control("b", passed=True)],
        [_treatment("a"), _treatment("b")],
    )

    report = compare_arms.compare(control, treatment, tasks=2, cross_sut=_cross())

    assert report["comparable"] is True
    assert report["refusals"] == []
    assert report["schema"] == "stella-cross-sut-ab-comparison-v1"
    assert report["path_fired"]["treatment_trials_on_the_path"] == 2
    assert "witness_tier" not in report


def test_the_report_says_the_two_arms_are_confounded() -> None:
    """The delta belongs to the arm, never to a change inside the range."""
    control, treatment = _arms([_control("a")], [_treatment("a")])

    report = compare_arms.compare(control, treatment, tasks=1, cross_sut=_cross())
    rendered = compare_arms.markdown(report)

    assert report["identity"]["cross_sut"]["confounded"] is True
    assert report["confound"] == compare_arms.CROSS_SUT_CONFOUND
    assert "Confounded" in rendered


def test_a_build_from_another_tree_is_refused() -> None:
    other = "c" * 40
    control, treatment = _arms(
        [_control("a")], [_row("a", build=other, passed=True, loop_mode=PATH_UNDER_TEST)]
    )

    report = compare_arms.compare(control, treatment, tasks=1, cross_sut=_cross())

    assert report["comparable"] is False
    assert any(NEW in reason and other in reason for reason in report["refusals"])


def test_one_binary_running_both_arms_is_refused() -> None:
    """Two commits with one binary hash means one build ran both arms."""
    control, treatment = _arms(
        [_control("a", binary_sha256="z" * 64)],
        [_treatment("a", binary_sha256="z" * 64)],
    )

    report = compare_arms.compare(control, treatment, tasks=1, cross_sut=_cross())

    assert report["comparable"] is False
    assert any("one arm run twice" in reason for reason in report["refusals"])


def test_a_treatment_trial_that_cannot_say_which_path_it_ran_is_refused() -> None:
    control, treatment = _arms(
        [_control("a"), _control("b")],
        [_treatment("a"), _row("b", build=NEW, passed=True)],
    )

    report = compare_arms.compare(control, treatment, tasks=2, cross_sut=_cross())

    assert report["comparable"] is False
    assert any("1 treatment trial(s)" in reason for reason in report["refusals"])


def test_a_control_trial_on_the_path_under_test_is_refused() -> None:
    """Both arms on one path is one arm, whatever the two builds say."""
    control, treatment = _arms(
        [_control("a", loop_mode=PATH_UNDER_TEST)], [_treatment("a")]
    )

    report = compare_arms.compare(control, treatment, tasks=1, cross_sut=_cross())

    assert report["comparable"] is False
    assert any("both arms ran the path" in reason for reason in report["refusals"])


def test_a_posture_that_moved_with_the_build_is_refused() -> None:
    """Only the build may differ. A second change makes the arms two changes."""
    control, treatment = _arms(
        [_control("a")], [_treatment("a", assurance_arm="witness-on")]
    )

    report = compare_arms.compare(control, treatment, tasks=1, cross_sut=_cross())

    assert report["comparable"] is False
    assert any("assurance arms" in reason for reason in report["refusals"])


# --------------------------------------------------------------------------
# The declaration itself
# --------------------------------------------------------------------------


def test_a_short_commit_hash_is_refused() -> None:
    try:
        compare_arms.parse_cross_sut("a6d3db4f6:" + NEW, "loop_mode=x")
    except ValueError as exc:
        assert "40-character" in str(exc)
    else:  # pragma: no cover - the assertion is the test
        raise AssertionError("a short hash must not be accepted")


def test_one_commit_named_twice_is_refused() -> None:
    try:
        compare_arms.parse_cross_sut(f"{OLD}:{OLD}", "loop_mode=x")
    except ValueError as exc:
        assert "one arm run twice" in str(exc)
    else:  # pragma: no cover - the assertion is the test
        raise AssertionError("one commit cannot be two arms")


def test_a_value_holding_an_equals_sign_survives_parsing() -> None:
    declared = compare_arms.parse_cross_sut(f"{OLD}:{NEW}", "loop_mode=a=b")

    assert declared.fired_field == "loop_mode"
    assert declared.fired_value == "a=b"


def test_the_two_flags_are_one_declaration(tmp_path: Path, capsys) -> None:
    """Half a declaration would run the mode with no proof of the path."""
    control = _write(tmp_path / "control.jsonl", [_control("a")])
    treatment = _write(tmp_path / "treatment.jsonl", [_treatment("a")])

    code = compare_arms.main(
        [str(control), str(treatment), "--tasks", "1", "--cross-sut", f"{OLD}:{NEW}"]
    )

    assert code == 2
    assert "pass both or neither" in capsys.readouterr().err


def test_the_cli_runs_a_declared_two_build_comparison(tmp_path: Path, capsys) -> None:
    control = _write(tmp_path / "control.jsonl", [_control("a"), _control("b")])
    treatment = _write(tmp_path / "treatment.jsonl", [_treatment("a"), _treatment("b")])

    code = compare_arms.main(
        [
            str(control),
            str(treatment),
            "--tasks",
            "2",
            "--cross-sut",
            f"{OLD}:{NEW}",
            "--treatment-fired",
            f"loop_mode={PATH_UNDER_TEST}",
        ]
    )
    report = json.loads(capsys.readouterr().out)

    assert code == 0
    assert report["comparable"] is True
    assert report["identity"]["cross_sut"]["declared_control_commit"] == OLD
