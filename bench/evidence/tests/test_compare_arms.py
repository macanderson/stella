"""Tests for the authored-witness A/B comparison (#1284).

This script decides whether the strongest rung of Stella's verification ladder
becomes the default for every benchmark run. Two failure modes matter more than
arithmetic bugs, and both produce a well-formed, confident report:

* comparing two runs that are not the two arms of one experiment — different
  SUTs, the same arm twice, or evidence too old to say which arm it ran;
* comparing a treatment arm that *declared* the witness rung and never
  exercised it, which is #1147 with the numbers still printing.

Both refuse here. The rest is the pairing, the two outcomes, and the
preregistered decision rule, which must not be reachable by any input that has
not earned it.
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


def _row(task: str, *, arm: str, passed: bool, **extra) -> dict:
    """One trial row in the shape `score_dev_baseline.py extract` writes."""
    row = {
        "task_name": task,
        "trial_id": f"{task}__aaa",
        "accuracy": 1.0 if passed else 0.0,
        "reward": 1.0 if passed else 0.0,
        "assurance_arm": arm,
        "source_commit": "c" * 40,
        "binary_sha256": "b" * 64,
        "engine_posture_sha256": "off-digest" if arm == "witness-off" else "on-digest",
        "witness_author_model": (
            None if arm == "witness-off" else "openrouter/deepseek/deepseek-v4-pro"
        ),
        "witness_authored_state": (
            "unavailable" if arm == "witness-off" else "authored"
        ),
        "witness_authored_count": 0 if arm == "witness-off" else 1,
        "self_verdict_passed": passed,
        "self_verdict_deterministic": False,
        "usd": 1.0,
    }
    row.update(extra)
    return row


def _arms(control_rows: list[dict], treatment_rows: list[dict]):
    return (
        {row["task_name"]: row for row in control_rows},
        {row["task_name"]: row for row in treatment_rows},
    )


# --------------------------------------------------------------------------
# Refusals — before any number is allowed to mean something
# --------------------------------------------------------------------------


def test_two_runs_of_the_same_arm_are_not_an_experiment() -> None:
    control, treatment = _arms(
        [_row("a", arm="witness-off", passed=True)],
        [_row("b", arm="witness-off", passed=True)],
    )
    report = compare_arms.compare(control, treatment, tasks=2)

    assert report["comparable"] is False
    assert report["decision"]["verdict"] == "refused"
    assert any("treatment arm should be 'witness-on'" in r for r in report["refusals"])


def test_evidence_that_cannot_say_which_arm_it_ran_is_refused() -> None:
    """v1 evidence predates the assurance block, so it cannot be an arm."""
    stale = _row("a", arm="witness-off", passed=True)
    del stale["assurance_arm"]
    control, treatment = _arms([stale], [_row("a", arm="witness-on", passed=True)])
    report = compare_arms.compare(control, treatment, tasks=1)

    assert report["comparable"] is False
    assert any("pre-#1007 evidence" in r for r in report["refusals"])


def test_two_different_suts_are_two_experiments() -> None:
    control, treatment = _arms(
        [_row("a", arm="witness-off", passed=False)],
        [_row("a", arm="witness-on", passed=True, source_commit="d" * 40)],
    )
    report = compare_arms.compare(control, treatment, tasks=1)

    assert report["comparable"] is False
    assert any("same source_commit" in r for r in report["refusals"])


def test_a_treatment_arm_that_never_authored_a_witness_is_refused() -> None:
    """#1147: the posture declared the rung, model validation dropped the pin.

    The digest said witness-on and the control arm executed. Nothing in the
    posture can catch that — only the run's own proof stream can, so the
    comparison reads the stream rather than the declaration.
    """
    control, treatment = _arms(
        [_row("a", arm="witness-off", passed=False)],
        [
            _row(
                "a",
                arm="witness-on",
                passed=True,
                witness_authored_state="unavailable",
                witness_authored_count=0,
            )
        ],
    )
    report = compare_arms.compare(control, treatment, tasks=1)

    assert report["comparable"] is False
    assert any("authored no witness on any task" in r for r in report["refusals"])
    # The numbers are still printed: a refusal nobody can check is not one.
    assert report["primary_outcome"]["gained"] == 1


def test_a_refused_comparison_never_reaches_a_verdict() -> None:
    """No input that fails a refusal may produce adopt/reject/inconclusive."""
    control, treatment = _arms(
        [_row("a", arm="witness-off", passed=False)],
        [_row("a", arm="witness-off", passed=True)],
    )
    report = compare_arms.compare(control, treatment, tasks=1)

    assert report["decision"]["verdict"] == "refused"


# --------------------------------------------------------------------------
# Pairing
# --------------------------------------------------------------------------


def test_pairing_splits_gained_from_lost() -> None:
    control, treatment = _arms(
        [
            _row("both", arm="witness-off", passed=True),
            _row("gained", arm="witness-off", passed=False),
            _row("lost", arm="witness-off", passed=True),
            _row("neither", arm="witness-off", passed=False),
        ],
        [
            _row("both", arm="witness-on", passed=True),
            _row("gained", arm="witness-on", passed=True),
            _row("lost", arm="witness-on", passed=False),
            _row("neither", arm="witness-on", passed=False),
        ],
    )
    primary = compare_arms.compare(control, treatment, tasks=4)["primary_outcome"]

    assert (primary["both_pass"], primary["gained"]) == (1, 1)
    assert (primary["lost"], primary["both_fail"]) == (1, 1)
    assert primary["delta_passes"] == 0
    assert primary["control_passes"] == primary["treatment_passes"] == 2


def test_a_task_missing_from_an_arm_scores_zero_there(capsys) -> None:
    """Same rule as the scorer: a trial that never reported is a failure.

    Dropping it instead would let an arm that crashed on its hardest tasks
    compare its remaining ones against the other arm's full set.
    """
    control, treatment = _arms(
        [_row("a", arm="witness-off", passed=True), _row("b", arm="witness-off", passed=True)],
        [_row("a", arm="witness-on", passed=True)],
    )
    report = compare_arms.compare(control, treatment, tasks=2)
    primary = report["primary_outcome"]

    assert primary["lost"] == 1
    assert primary["treatment_passes"] == 1
    assert report["identity"]["tasks_only_in_control"] == ["b"]


def test_a_task_no_arm_covered_still_counts_against_the_denominator() -> None:
    control, treatment = _arms(
        [_row("a", arm="witness-off", passed=True)],
        [_row("a", arm="witness-on", passed=True)],
    )
    primary = compare_arms.compare(control, treatment, tasks=89)["primary_outcome"]

    assert primary["tasks_missing_from_both_arms"] == 88
    assert primary["both_fail"] == 88
    assert primary["treatment_accuracy"] == 1 / 89


def test_more_tasks_than_the_declared_denominator_is_refused() -> None:
    control, treatment = _arms(
        [_row("a", arm="witness-off", passed=True), _row("b", arm="witness-off", passed=True)],
        [_row("a", arm="witness-on", passed=True), _row("b", arm="witness-on", passed=True)],
    )
    report = compare_arms.compare(control, treatment, tasks=1)

    assert any("more than the declared denominator" in r for r in report["refusals"])


def test_mcnemar_uses_only_the_discordant_pairs() -> None:
    # Ten tasks moved one way and none the other: as one-sided as it gets.
    assert compare_arms.mcnemar_exact_p(10, 0) < 0.01
    # A wash, however large.
    assert compare_arms.mcnemar_exact_p(5, 5) == 1.0
    # Nothing moved: there is no test to run, and 1.0 would read as "measured".
    assert compare_arms.mcnemar_exact_p(0, 0) is None


# --------------------------------------------------------------------------
# The secondary outcome: wrong "this passed" calls
# --------------------------------------------------------------------------


def test_a_wrong_this_passed_call_is_counted_and_named() -> None:
    rows = [
        # Claimed pass, graded zero — the failure the rung exists to remove.
        _row("lied", arm="witness-off", passed=False, self_verdict_passed=True),
        _row("honest-fail", arm="witness-off", passed=False, self_verdict_passed=False),
        _row("honest-pass", arm="witness-off", passed=True, self_verdict_passed=True),
    ]
    stats = compare_arms.self_verdict_stats(rows)

    assert stats["false_pass"] == 1
    assert stats["false_pass_tasks"] == ["lied"]
    assert stats["false_fail"] == 0
    assert stats["reported_by"] == 3
    assert stats["agreement_rate"] == 2 / 3


def test_a_trial_that_made_no_claim_is_not_a_wrong_one() -> None:
    """Tri-state through the whole chain: silence is not a verdict.

    A trial killed before the ladder closed reports `self_verdict_passed: null`.
    Reading that as "claimed failure" would credit the agent with honesty it
    never displayed, on exactly the trials most likely to be re-read.
    """
    rows = [
        _row("interrupted", arm="witness-off", passed=False, self_verdict_passed=None),
        _row("judged", arm="witness-off", passed=True, self_verdict_passed=True),
    ]
    stats = compare_arms.self_verdict_stats(rows)

    assert stats["reported_by"] == 1
    assert stats["silent_trials"] == 1
    assert stats["agreement_rate"] == 1.0


def test_a_flip_oracle_verdict_is_not_counted_as_a_model_opinion() -> None:
    """The deterministic rung and the model judge are different instruments.

    #1284 quotes the *judge's* agreement rate. Averaging a flip-oracle verdict
    into it reports the ladder's aggregate as the judge's reliability.
    """
    rows = [
        _row(
            "oracle",
            arm="witness-off",
            passed=False,
            self_verdict_passed=True,
            self_verdict_deterministic=True,
        ),
        _row(
            "opinion",
            arm="witness-off",
            passed=False,
            self_verdict_passed=True,
            self_verdict_deterministic=False,
        ),
    ]
    stats = compare_arms.self_verdict_stats(rows)

    assert stats["false_pass"] == 2
    assert stats["model_judge_only"]["reported_by"] == 1
    assert stats["model_judge_only"]["false_pass_tasks"] == ["opinion"]


def test_fixed_and_introduced_false_passes_are_named_not_netted() -> None:
    """Two counts that cancel are not "no change" — they are two changes."""
    control, treatment = _arms(
        [
            _row("was-lied-about", arm="witness-off", passed=False, self_verdict_passed=True),
            _row("clean", arm="witness-off", passed=False, self_verdict_passed=False),
        ],
        [
            _row("was-lied-about", arm="witness-on", passed=False, self_verdict_passed=False),
            _row("clean", arm="witness-on", passed=False, self_verdict_passed=True),
        ],
    )
    verdicts = compare_arms.compare(control, treatment, tasks=2)["self_verdict"]

    assert verdicts["false_pass_delta"] == 0
    assert verdicts["false_passes_fixed"] == ["was-lied-about"]
    assert verdicts["false_passes_introduced"] == ["clean"]


# --------------------------------------------------------------------------
# Cost, and the preregistered decision rule
# --------------------------------------------------------------------------


def test_a_partial_spend_total_withholds_the_ratio() -> None:
    """Two partial sums do not make a cost comparison — the same discipline
    `score.json` applies to `usd_total`."""
    silent = _row("b", arm="witness-on", passed=True)
    del silent["usd"]
    control, treatment = _arms(
        [_row("a", arm="witness-off", passed=True), _row("b", arm="witness-off", passed=True)],
        [_row("a", arm="witness-on", passed=True), silent],
    )
    cost = compare_arms.compare(control, treatment, tasks=2)["cost"]

    assert cost["usd_ratio"] is None
    assert "partial" in cost["usd_ratio_note"]
    assert cost["treatment"]["usd_reported_by"] == 1


def _decide(gained: int, lost: int, false_pass_delta: int, ratio: float | None) -> dict:
    primary = {
        "gained": gained,
        "lost": lost,
        "mcnemar_exact_p": compare_arms.mcnemar_exact_p(gained, lost),
    }
    return compare_arms.decide(
        primary, {"false_pass_delta": false_pass_delta}, {"usd_ratio": ratio}
    )


def test_more_wrong_passed_calls_is_a_rejection_however_many_tasks_it_won() -> None:
    """The rung's purpose is the honesty of the verdict, not only the score."""
    decision = _decide(gained=8, lost=0, false_pass_delta=3, ratio=1.0)

    assert decision["verdict"] == "reject"
    assert any("wrong 'this passed'" in r for r in decision["reasons"])


def test_losing_tasks_significantly_is_a_rejection() -> None:
    decision = _decide(gained=0, lost=9, false_pass_delta=-2, ratio=1.0)

    assert decision["verdict"] == "reject"


def test_a_significant_gain_within_the_cost_ceiling_is_adopted() -> None:
    decision = _decide(gained=9, lost=0, false_pass_delta=-4, ratio=1.4)

    assert decision["verdict"] == "adopt"


def test_a_gain_that_costs_too_much_is_not_adopted() -> None:
    decision = _decide(gained=9, lost=0, false_pass_delta=-4, ratio=2.2)

    assert decision["verdict"] == "inconclusive"
    assert any("above the 1.5x ceiling" in r for r in decision["reasons"])


def test_a_gain_whose_cost_could_not_be_compared_is_not_adopted() -> None:
    """Unknown cost is not affordable cost."""
    decision = _decide(gained=9, lost=0, false_pass_delta=-4, ratio=None)

    assert decision["verdict"] == "inconclusive"


def test_a_small_gain_with_no_honesty_improvement_is_inconclusive() -> None:
    decision = _decide(gained=2, lost=1, false_pass_delta=0, ratio=1.1)

    assert decision["verdict"] == "inconclusive"


# --------------------------------------------------------------------------
# End to end, through the CLI
# --------------------------------------------------------------------------


def _write(path: Path, rows: list[dict]) -> Path:
    path.write_text("".join(json.dumps(row) + "\n" for row in rows))
    return path


def test_cli_reports_and_exits_nonzero_on_a_refusal(tmp_path: Path, capsys) -> None:
    control = _write(tmp_path / "off.jsonl", [_row("a", arm="witness-off", passed=False)])
    treatment = _write(tmp_path / "on.jsonl", [_row("a", arm="witness-off", passed=True)])

    code = compare_arms.main([str(control), str(treatment), "--tasks", "1"])
    out = json.loads(capsys.readouterr().out)

    assert code == 1
    assert out["comparable"] is False


def test_cli_writes_a_table_that_names_every_task(tmp_path: Path, capsys) -> None:
    control = _write(
        tmp_path / "off.jsonl",
        [
            _row("alpha", arm="witness-off", passed=False, self_verdict_passed=True),
            _row("beta", arm="witness-off", passed=True),
        ],
    )
    treatment = _write(
        tmp_path / "on.jsonl",
        [
            _row("alpha", arm="witness-on", passed=True),
            _row("beta", arm="witness-on", passed=True),
        ],
    )
    table = tmp_path / "results.md"

    code = compare_arms.main(
        [str(control), str(treatment), "--tasks", "2", "--markdown", str(table)]
    )
    report = json.loads(capsys.readouterr().out)
    rendered = table.read_text()

    assert code == 0
    assert report["decision"]["verdict"] in {"adopt", "inconclusive"}
    assert report["self_verdict"]["false_passes_fixed"] == ["alpha"]
    assert "`alpha`" in rendered and "`beta`" in rendered


def test_a_duplicated_task_in_one_arm_is_a_hard_error(tmp_path: Path) -> None:
    """Two rows for one task means a resume or a rerun, both forbidden."""
    path = _write(
        tmp_path / "off.jsonl",
        [
            _row("a", arm="witness-off", passed=False),
            _row("a", arm="witness-off", passed=True),
        ],
    )
    try:
        compare_arms.load_arm(path)
    except ValueError as exc:
        assert "more than one row" in str(exc)
    else:  # pragma: no cover - the assertion is the test
        raise AssertionError("a duplicated task must not load")
