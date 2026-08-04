"""Tests for the #1295 standalone-judge-pass analyzer.

This script decides what "how often is only the judge behind a turn" *means*,
so it belongs to the same family as the scorer and the manifest builder: a tool
whose output is quoted as a measurement and which is confident whether or not
it is right.

Two properties carry the weight here.

**It must not answer a question the data cannot answer.** Streams recorded
before the ladder snapshot reached the wire (#865/#1043) carry no `ladder`, so
neither `stands_alone` nor `tracked_command` exists on them. Defaulting those
to `False` would report "0% standalone" for a corpus that never recorded the
field — the same number as "measured, and it never happened", and the wrong
one in the flattering direction.

**Its predicate must not drift from the pipeline's.** `stands_alone` here is a
restatement of `LadderInputs::judge_pass_stands_alone`, and the pair of
exclusions that make it narrow — a readable diff and a recorded file touch are
NOT corroboration — is exactly what a well-meaning edit would "fix".
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

_EVIDENCE = Path(__file__).resolve().parent.parent


def _load(relative: str, name: str):
    """Import a sibling script by path — they are files, not a package."""
    spec = importlib.util.spec_from_file_location(name, _EVIDENCE / relative)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


analyzer = _load("judge-evidence-demand-1295/analyze.py", "jed1295_analyze")


def _verdict(passed: bool, ladder: dict | None, summary: str = "") -> str:
    evidence: dict = {"summary": summary, "deterministic": False}
    if ladder is not None:
        evidence["ladder"] = ladder
    return json.dumps({"type": "judge_verdict", "passed": passed, "evidence": evidence})


def _host_run(root: Path, task: str, lines: list[str]) -> None:
    (root / task).mkdir(parents=True, exist_ok=True)
    (root / task / "stella-events.jsonl").write_text("\n".join(lines) + "\n")


# --------------------------------------------------------------------------
# The predicate
# --------------------------------------------------------------------------


def test_only_test_shaped_evidence_corroborates_a_pass() -> None:
    """A flip or a green touched test — and nothing else — clears the bar."""
    assert analyzer.stands_alone({"flip_achieved": False, "touched_tests_passed": None})
    assert not analyzer.stands_alone({"flip_achieved": True, "touched_tests_passed": None})
    assert not analyzer.stands_alone({"flip_achieved": False, "touched_tests_passed": True})
    # A RED test points the other way; reading it as support would invert the
    # whole ladder.
    assert analyzer.stands_alone({"flip_achieved": False, "touched_tests_passed": False})


def test_a_diff_and_a_file_touch_are_not_corroboration() -> None:
    """The narrowness is the point, and #1225 is why it has to be stated.

    A git baseline turns the diff and file-touch channels on. Both prove the
    tree CHANGED; a pass claims the change is RIGHT. If this ever starts
    reading them as support, the reported rate collapses for a reason that has
    nothing to do with the work being better verified.
    """
    assert analyzer.stands_alone(
        {
            "flip_achieved": False,
            "touched_tests_passed": None,
            "diff_available": True,
            "diff_lines": 400,
            "file_change_events": 30,
        }
    )


# --------------------------------------------------------------------------
# Unknown is not zero
# --------------------------------------------------------------------------


def test_a_pre_ladder_turn_is_unscorable_rather_than_zero(tmp_path: Path) -> None:
    """No `ladder` on the event means the fields do not exist, not that they were false."""
    _host_run(tmp_path, "old-task", [_verdict(True, None, "UNVERIFIABLE — flip oracle not armed")])
    rows = analyzer.extract_trial(tmp_path / "old-task", "hist", layout="host")

    assert len(rows) == 1
    assert rows[0]["stands_alone"] is None, "must not be guessed at"
    assert rows[0]["ladder_present"] is False
    assert rows[0]["oracle_unarmed_stated"] is True, "the prose fallback still reads"

    summary = analyzer.summarize(rows)
    assert summary["turns"] == 1
    assert summary["turns_scorable"] == 0
    assert summary["turns_no_ladder"] == 1
    # The load-bearing assertion: an unscorable turn contributes to NO rate.
    assert summary["standalone_judge_passes"] == 0
    assert summary["judge_passes"] == 0


def test_scorable_and_unscorable_turns_do_not_share_a_denominator(tmp_path: Path) -> None:
    """One recorded turn and one pre-ladder turn is 100%, never 50%."""
    _host_run(
        tmp_path,
        "mixed",
        [
            _verdict(True, None),
            _verdict(True, {"flip_achieved": False, "touched_tests_passed": None}),
        ],
    )
    summary = analyzer.summarize(analyzer.extract_trial(tmp_path / "mixed", "arm", layout="host"))

    assert summary["turns"] == 2
    assert summary["turns_scorable"] == 1
    assert summary["standalone_judge_passes"] == 1


# --------------------------------------------------------------------------
# The precondition the feature turns on
# --------------------------------------------------------------------------


def test_the_tracked_command_is_counted_separately_from_the_rate(tmp_path: Path) -> None:
    """Standalone-and-answerable is the number that says whether asking helps.

    A standalone pass with no tracked command is one no ask could ever fix —
    reporting it beside the bare rate is what keeps "the condition is common"
    from being read as "the ask would have paid off".
    """
    _host_run(
        tmp_path,
        "answerable",
        [
            _verdict(
                True,
                {
                    "flip_achieved": False,
                    "touched_tests_passed": None,
                    "tracked_command": "cargo test -p x",
                },
            ),
            _verdict(True, {"flip_achieved": False, "touched_tests_passed": None}),
        ],
    )
    summary = analyzer.summarize(
        analyzer.extract_trial(tmp_path / "answerable", "arm", layout="host")
    )

    assert summary["standalone_judge_passes"] == 2
    assert summary["turns_with_tracked_command"] == 1
    assert summary["standalone_with_tracked_command"] == 1


# --------------------------------------------------------------------------
# Layouts and robustness
# --------------------------------------------------------------------------


def test_a_host_run_has_no_reward_and_does_not_invent_one(tmp_path: Path) -> None:
    """There is no verifier outside the container, so `reward` stays absent.

    `None` must not become `0.0`: a run with no verifier and a run the verifier
    failed are different facts, and only one of them is a statement about the
    work.
    """
    _host_run(tmp_path, "t", [_verdict(True, {"flip_achieved": True})])
    rows = analyzer.extract_trial(tmp_path / "t", "arm", layout="host")
    assert rows[0]["reward"] is None
    assert analyzer.summarize(rows)["solved"] == 0


def test_a_harbor_trial_reads_its_verifier_reward(tmp_path: Path) -> None:
    trial = tmp_path / "task__abc123"
    (trial / "agent").mkdir(parents=True)
    (trial / "verifier").mkdir(parents=True)
    (trial / "agent" / "stella-events.jsonl").write_text(
        _verdict(True, {"flip_achieved": True}) + "\n"
    )
    (trial / "verifier" / "reward.txt").write_text("1.0\n")

    rows = analyzer.extract_trial(trial, "arm")
    assert rows[0]["reward"] == 1.0
    assert rows[0]["task"] == "task", "the attempt suffix is not part of the task name"
    assert analyzer.summarize(rows)["solved"] == 1


def test_a_truncated_final_line_does_not_lose_the_turns_before_it(tmp_path: Path) -> None:
    """A trial killed mid-write by the agent timeout is the normal case."""
    (tmp_path / "t").mkdir(parents=True)
    (tmp_path / "t" / "stella-events.jsonl").write_text(
        _verdict(True, {"flip_achieved": False, "touched_tests_passed": None})
        + "\n"
        + '{"type": "judge_verdict", "passed": tr'
    )
    rows = analyzer.extract_trial(tmp_path / "t", "arm", layout="host")
    assert len(rows) == 1


def test_a_trial_that_never_verified_contributes_no_rows(tmp_path: Path) -> None:
    """Silence is not a turn. 21 of 34 trials in the handoff corpus look like this."""
    _host_run(tmp_path, "quiet", ['{"type": "stage", "name": "execute"}'])
    assert analyzer.extract_trial(tmp_path / "quiet", "arm", layout="host") == []
