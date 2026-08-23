"""The banned-behavior ledger, against fixture traces. No network, no S3.

Two halves, and the second is the one that matters. A gate that aborts is easy;
a gate that stays quiet on a healthy run is the whole product, because the
first false abort is the last time anyone leaves it switched on. So every
banned behavior here is tested from both sides — it fires on a trace that
exhibits it, and the ledger returns *clear* on a trace that does not, including
one that fires several report-only detectors on the way past.
"""

from __future__ import annotations

import json

import detectors
import pytest
from banned import (
    BANNED_EXIT,
    FIRST_TRIAL,
    LEDGER,
    Banned,
    ReportOnly,
    Trip,
    Warrant,
    evaluate,
    main,
    trials_affected,
)
from fixtures import ok, tool_pair, write_run
from run_trace import load_run


def _verdict(root, **kwargs):
    return evaluate(load_run(root, "testrun"), **kwargs)


def _usage(step=0, role="worker", model="anthropic/claude-sonnet-5"):
    return {
        "ts": 1,
        "type": "step_usage",
        "step": step,
        "role": role,
        "model": model,
        "complete": True,
    }


def _healthy_trial(task):
    """A trial that worked: one completed model call, one useful grep."""
    return {
        "task": task,
        "reward": 1,
        "events": [
            _usage(),
            *tool_pair("c1", "grep", {"pattern": "def _hkey|def _hval"}, ok("app.py:12: def _hkey")),
        ],
    }


# --------------------------------------------------------------------------
# The ledger's own contract
# --------------------------------------------------------------------------


def test_ledger_is_total_over_the_detector_registry():
    """Every detector must declare whether it stops a run.

    This is the forcing function. Without it the ledger decays into the set of
    behaviors somebody thought of once, and a detector added next month is
    silently report-only because nobody was asked.
    """
    registered = {d.code for d in detectors.DETECTORS}
    missing = sorted(registered - set(LEDGER))
    assert not missing, (
        f"detectors with no ledger disposition: {missing}. Add a `Banned(...)` "
        "row with the PR that fixed it, or a `ReportOnly(...)` row saying why it "
        "must not stop a run."
    )


def test_ledger_names_no_detector_that_does_not_exist():
    registered = {d.code for d in detectors.DETECTORS}
    stale = sorted(set(LEDGER) - registered)
    assert not stale, f"ledger rows for detectors that no longer exist: {stale}"


def test_a_fixed_regression_warrant_must_cite_the_fix():
    """An entry with no fix behind it is not a banned behavior."""
    with pytest.raises(ValueError, match="cite the PR or issue"):
        Warrant(kind="fixed-regression", citation="", note="something looked bad")


def test_every_banned_row_carries_a_warrant_and_evidence():
    for code, disposition in LEDGER.items():
        if not isinstance(disposition, Banned):
            continue
        assert disposition.evidence, f"{code} aborts runs with no evidence recorded"
        if disposition.warrant.kind == "fixed-regression":
            assert disposition.warrant.citation.startswith("#"), code


def test_a_report_only_row_says_why():
    for code, disposition in LEDGER.items():
        if isinstance(disposition, ReportOnly):
            assert len(disposition.reason) > 40, f"{code} declines to abort with no reason"


def test_an_incoherent_trip_is_rejected():
    with pytest.raises(ValueError, match="incoherent trip"):
        Trip(at_least=5, of_first=2)


# --------------------------------------------------------------------------
# The silence half — a healthy run must come back clear
# --------------------------------------------------------------------------


def test_a_healthy_run_is_clear(tmp_path):
    write_run(
        tmp_path,
        [_healthy_trial(f"alpha{i}__AAA") for i in range(5)],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    verdict = _verdict(tmp_path)
    assert not verdict.banned
    assert verdict.codes == ()
    assert verdict.settled == 5


def test_a_run_firing_only_report_only_detectors_is_clear(tmp_path):
    """The decisive case: real detectors fire and the run still proceeds.

    `tool-error-envelope` fires on 40% of the healthy `post1` run and
    `repeated-identical-tool-call` is a band metric. A gate that aborted on
    either would abort every arm we have.
    """
    envelope = (
        "PASSED test/test_a.py::one\nPASSED test/test_b.py::two\n"
        "PASSED test/test_c.py::three\nPASSED test/test_d.py::four\n"
        + "x" * 220
        + "\n[exit code: 1]"
    )
    repeated = tool_pair("r1", "bash", {"command": "ls"}, ok("a\nb")) + tool_pair(
        "r2", "bash", {"command": "ls"}, ok("a\nb")
    )
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 1,
                "events": [
                    _usage(),
                    *tool_pair("e1", "bash", {"command": "pytest"}, {"error": {"message": envelope}}),
                    *repeated,
                ],
            }
        ],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    fired = {f.detector for f in detectors.run_all(load_run(tmp_path, "testrun"))}
    assert "tool-error-envelope" in fired, "fixture no longer exercises the report-only path"
    assert "repeated-identical-tool-call" in fired

    verdict = _verdict(tmp_path)
    assert not verdict.banned, f"report-only detectors stopped a run: {verdict.codes}"


# --------------------------------------------------------------------------
# grep-ere-false-negative (#2989) — fires, and stays silent post-fix
# --------------------------------------------------------------------------


_DISCLOSED = (
    "(no matches)\n\n(searched with POSIX `grep -E`: ripgrep is not installed "
    "here, so `type` filters are ignored and the pattern was executed as an ERE.)"
)


def test_grep_ere_false_negative_stops_the_run(tmp_path):
    write_run(
        tmp_path,
        [
            {
                "task": "sanitize-git-repo__AAA",
                "reward": 0,
                "events": [
                    _usage(),
                    *tool_pair(
                        "g1",
                        "grep",
                        {"pattern": "AKIA[0-9A-Z]{16}|ghp_[A-Za-z0-9]{20,}"},
                        ok("(no matches)"),
                    ),
                ],
            }
        ],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    verdict = _verdict(tmp_path)
    assert verdict.banned
    assert "grep-ere-false-negative" in verdict.codes
    tripped = next(t for t in verdict.tripped if t.code == "grep-ere-false-negative")
    assert tripped.warrant.citation == "#2989"
    assert tripped.trip == FIRST_TRIAL
    assert "AKIA" in tripped.excerpt


def test_grep_zero_match_is_clear_once_the_fallback_discloses_itself(tmp_path):
    """The post-#2989 shape: same pattern, same empty answer, disclosed.

    This is the discrimination the detector exists to make. Without it the rule
    is "an empty grep aborts the run", which fires on every run in which the
    agent searched for something that genuinely is not there.
    """
    write_run(
        tmp_path,
        [
            {
                "task": "sanitize-git-repo__AAA",
                "reward": 1,
                "events": [
                    _usage(),
                    *tool_pair(
                        "g1",
                        "grep",
                        {"pattern": "AKIA[0-9A-Z]{16}|ghp_[A-Za-z0-9]{20,}"},
                        ok(_DISCLOSED),
                    ),
                ],
            }
        ],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    assert not _verdict(tmp_path).banned


def test_grep_zero_match_on_a_literal_pattern_is_clear(tmp_path):
    """No ERE metacharacter, so the fallback could execute it faithfully."""
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 1,
                "events": [
                    _usage(),
                    *tool_pair("g1", "grep", {"pattern": "def static_file"}, ok("(no matches)")),
                ],
            }
        ],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    assert not _verdict(tmp_path).banned


# --------------------------------------------------------------------------
# role-model-census-mismatch — and the comparison bug the gate caught
# --------------------------------------------------------------------------


def test_a_bare_model_name_matches_its_qualified_declaration(tmp_path):
    """`claude-sonnet-5` observed against `anthropic/claude-sonnet-5` declared.

    The witness for the false positive this gate found: `step_usage.model`
    carries the bare name (the provider rides in its own field), and comparing
    on a fixed two-segment tail called that a routing defect on 20 of 20 trials
    of a run whose telemetry and configuration agreed throughout.
    """
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 1,
                "events": [_usage(model="claude-sonnet-5")],
            }
        ],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    verdict = _verdict(tmp_path)
    assert not verdict.banned, f"a spelling difference stopped the run: {verdict.codes}"


def test_a_genuinely_different_model_stops_the_run(tmp_path):
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 1,
                "events": [_usage(model="claude-opus-5")],
            }
        ],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    verdict = _verdict(tmp_path)
    assert "role-model-census-mismatch" in verdict.codes


def test_a_different_provider_serving_the_same_model_stops_the_run(tmp_path):
    """Neither name is a suffix of the other, and which provider served it matters."""
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 1,
                "events": [_usage(model="bedrock/claude-sonnet-5")],
            }
        ],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    assert "role-model-census-mismatch" in _verdict(tmp_path).codes


# --------------------------------------------------------------------------
# void-model-call — decidable on trial one
# --------------------------------------------------------------------------


def test_a_trial_that_completed_no_model_call_stops_the_run(tmp_path):
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": [
                    {"ts": 1, "type": "reasoning", "delta": "thinking"},
                    {"ts": 2, "type": "reasoning", "delta": "more"},
                ],
            }
        ],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    verdict = _verdict(tmp_path)
    assert "void-model-call" in verdict.codes
    tripped = next(t for t in verdict.tripped if t.code == "void-model-call")
    assert tripped.warrant.kind == "measurement-invariant"
    assert tripped.trip == FIRST_TRIAL


# --------------------------------------------------------------------------
# graded-void-trial — the credential shape void-model-call cannot see
# --------------------------------------------------------------------------


def _void_trials(count, events=()):
    return [
        {"task": f"alpha{i}__AAA", "reward": 0, "events": list(events)}
        for i in range(count)
    ]


def test_a_graded_trial_with_no_events_at_all_stops_the_run(tmp_path):
    """The expired-credential shape: the request is refused before anything streams.

    This is the case the whole gate was asked for — twenty dead trials, every
    one scored as a task loss — and it is the case `void-model-call` misses,
    because that detector requires `reasoning` deltas and there are none.
    """
    write_run(
        tmp_path,
        _void_trials(3),
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    verdict = _verdict(tmp_path)
    assert "graded-void-trial" in verdict.codes
    tripped = next(t for t in verdict.tripped if t.code == "graded-void-trial")
    assert tripped.trip == FIRST_TRIAL
    assert "0 completed model calls" in tripped.excerpt or "step_usage       : 0" in tripped.excerpt


def test_void_model_call_alone_would_have_missed_it(tmp_path):
    """Pins *why* the second row exists, so nobody merges the two detectors.

    If this ever starts firing, `void-model-call` has been widened and the
    `graded-void-trial` row's rationale needs rewriting rather than quietly
    becoming redundant.
    """
    write_run(
        tmp_path,
        _void_trials(1),
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    fired = {f.detector for f in detectors.run_all(load_run(tmp_path, "testrun"))}
    assert "void-model-call" not in fired
    assert "graded-void-trial" in fired


def test_a_void_trial_nobody_graded_is_not_a_measurement_defect(tmp_path):
    """No reward means no number was reported, so nothing was mismeasured."""
    write_run(
        tmp_path,
        [{"task": "alpha__AAA", "reward": None, "events": []}],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    assert "graded-void-trial" not in _verdict(tmp_path).codes


def test_a_trial_that_completed_a_call_is_clear(tmp_path):
    write_run(
        tmp_path,
        [_healthy_trial("alpha__AAA")],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    assert "graded-void-trial" not in _verdict(tmp_path).codes


# --------------------------------------------------------------------------
# agent-timeout — the row that proves the rate rule is needed
# --------------------------------------------------------------------------


def _timeout_trial(task, timed_out):
    spec = {"task": task, "reward": 0, "events": [_usage()]}
    if timed_out:
        spec["exception_class"] = "AgentTimeoutError"
        spec["exception"] = "AgentTimeoutError: agent execution timed out"
    return spec


def test_one_timeout_in_five_does_not_stop_the_run(tmp_path):
    """Measured on the healthy `post1` run: 16 of 20 solved, 1 trial timed out.

    A first-trial trip on this row would have aborted it.
    """
    write_run(
        tmp_path,
        [_timeout_trial(f"alpha{i}__AAA", i == 0) for i in range(5)],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    verdict = _verdict(tmp_path)
    assert "agent-timeout" not in verdict.codes
    assert not verdict.banned


def test_three_timeouts_in_five_stops_the_run(tmp_path):
    write_run(
        tmp_path,
        [_timeout_trial(f"alpha{i}__AAA", i < 3) for i in range(5)],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    verdict = _verdict(tmp_path)
    assert "agent-timeout" in verdict.codes
    tripped = next(t for t in verdict.tripped if t.code == "agent-timeout")
    assert tripped.trials == 3
    assert tripped.settled == 5


def test_a_rate_row_cannot_trip_before_its_denominator_exists(tmp_path):
    """One timeout in one settled trial is not a 100% timeout rate.

    Refusing to conclude here is the difference between a gate and a coin flip:
    the first job to finish is very often the one that failed fastest.
    """
    write_run(
        tmp_path,
        [_timeout_trial("alpha__AAA", True)],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    verdict = _verdict(tmp_path)
    assert verdict.settled == 1
    assert "agent-timeout" not in verdict.codes


# --------------------------------------------------------------------------
# Arithmetic, waivers, and the CLI contract
# --------------------------------------------------------------------------


def test_trials_affected_counts_trials_not_occurrences(tmp_path):
    """Eleven findings inside one trial is one trial.

    Counting occurrences instead would turn every per-call detector into a
    first-trial trip by accident, and quietly satisfy any rate threshold.
    """
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": [
                    _usage(),
                    *[
                        e
                        for i in range(4)
                        for e in tool_pair(
                            f"g{i}", "grep", {"pattern": f"a{i}|b"}, ok("(no matches)")
                        )
                    ],
                ],
            }
        ],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    finding = next(
        f
        for f in detectors.run_all(load_run(tmp_path, "testrun"))
        if f.detector == "grep-ere-false-negative"
    )
    assert finding.count == 4
    assert trials_affected(finding) == 1


def test_a_waiver_clears_the_run_and_is_recorded(tmp_path):
    """The escape hatch. A waived behavior is never invisible."""
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": [
                    _usage(),
                    *tool_pair("g1", "grep", {"pattern": "a|b"}, ok("(no matches)")),
                ],
            }
        ],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    assert _verdict(tmp_path).banned
    waived = _verdict(tmp_path, allowed=["grep-ere-false-negative"])
    assert not waived.banned
    assert waived.waived == ("grep-ere-false-negative",)
    assert "grep-ere-false-negative" in waived.render()


def test_cli_exits_banned_and_prints_json(tmp_path, capsys):
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": [
                    _usage(),
                    *tool_pair("g1", "grep", {"pattern": "a|b"}, ok("(no matches)")),
                ],
            }
        ],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    assert main([str(tmp_path)]) == BANNED_EXIT
    payload = json.loads(capsys.readouterr().out)
    assert payload["banned"] is True
    assert payload["codes"] == ["grep-ere-false-negative"]
    assert payload["tripped"][0]["warrant"]["citation"] == "#2989"


def test_cli_exits_clear_on_a_healthy_run(tmp_path, capsys):
    write_run(
        tmp_path,
        [_healthy_trial("alpha__AAA")],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    assert main([str(tmp_path)]) == 0
    assert json.loads(capsys.readouterr().out)["banned"] is False


def test_cli_reports_an_unreadable_run_without_aborting_anything(tmp_path):
    """A gate that cannot read the run has failed; it has not found a defect.

    Exit 2, never `BANNED_EXIT` — a broken checker must not stop a healthy run,
    because that is precisely how a gate gets switched off for good.
    """
    assert main([str(tmp_path / "does-not-exist")]) == 2
