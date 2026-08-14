"""The Stop-hook oracle's decision core: flip first, judge fallback, bounded.

The hook is a file, not a package member (it ships into a task container),
so it is imported by path.
"""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path

_SPEC = importlib.util.spec_from_file_location(
    "oracle_stop_hook",
    Path(__file__).parent.parent / "hooks" / "oracle_stop_hook.py",
)
oracle = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(oracle)  # type: ignore[union-attr]


def _no_judge(*_args):
    raise AssertionError("the judge must not be consulted on this path")


def test_a_passing_configured_command_allows(monkeypatch):
    monkeypatch.setenv("STELLA_ORACLE_TEST_COMMAND", "true")
    state = {"denies": 0, "probes": []}
    decision = oracle.decide({}, state, judge=_no_judge, prober=lambda cmd: (True, ""))
    assert decision == {"action": "allow"}


def test_a_failing_configured_command_denies_with_the_output_tail(monkeypatch):
    monkeypatch.setenv("STELLA_ORACLE_TEST_COMMAND", "pytest -q")
    state = {"denies": 0, "probes": []}
    decision = oracle.decide(
        {}, state, judge=_no_judge, prober=lambda cmd: (False, "2 failed, 1 passed")
    )
    assert decision["action"] == "deny"
    assert "2 failed, 1 passed" in decision["reason"]
    assert oracle.DIVERSIFY_NOTE not in decision["reason"], "first deny does not steer"


def test_the_second_consecutive_deny_steers_toward_a_fresh_approach(monkeypatch):
    monkeypatch.setenv("STELLA_ORACLE_TEST_COMMAND", "pytest -q")
    state = {"denies": 1, "probes": []}
    decision = oracle.decide(
        {}, state, judge=_no_judge, prober=lambda cmd: (False, "still red")
    )
    assert decision["action"] == "deny"
    assert oracle.DIVERSIFY_NOTE in decision["reason"]


def test_judge_probes_persist_and_their_flip_decides_across_rounds(monkeypatch, tmp_path):
    """Round 1: the judge derives a probe, it fails, the turn is held open.

    Round 2 (the revision): the SAME persisted probe passes — the fail→pass
    flip — with no second judge call. This is the deterministic-flip
    contract: the judge creates the oracle once; running it decides.
    """
    monkeypatch.delenv("STELLA_ORACLE_TEST_COMMAND", raising=False)
    monkeypatch.chdir(tmp_path)
    judged = {
        "probes": [{"cmd": "test -f solved.txt", "description": "output exists"}],
        "verdict": "fail",
        "unmet": [],
    }
    state = {"denies": 0, "probes": []}
    first = oracle.decide(
        {"finalText": "done"},
        state,
        judge=lambda *a: judged,
        prober=lambda cmd: (False, "no such file"),
    )
    assert first["action"] == "deny"
    assert state["probes"] == judged["probes"], "the derived oracle persists"

    second = oracle.decide(
        {"finalText": "done, really"},
        state,
        judge=_no_judge,  # no second judge call: the persisted probe is the oracle
        prober=lambda cmd: (True, ""),
    )
    assert second == {"action": "allow"}


def test_an_unreachable_judge_fails_open(monkeypatch, tmp_path):
    monkeypatch.delenv("STELLA_ORACLE_TEST_COMMAND", raising=False)
    monkeypatch.chdir(tmp_path)
    state = {"denies": 0, "probes": []}
    decision = oracle.decide({}, state, judge=lambda *a: None, prober=_no_judge)
    assert decision == {"action": "allow"}, "a broken oracle must not hold the turn"


def test_a_probe_less_judge_verdict_decides(monkeypatch, tmp_path):
    monkeypatch.delenv("STELLA_ORACLE_TEST_COMMAND", raising=False)
    monkeypatch.chdir(tmp_path)
    state = {"denies": 0, "probes": []}
    failing = {"probes": [], "verdict": "fail", "unmet": ["the server never starts"]}
    decision = oracle.decide({}, state, judge=lambda *a: failing, prober=_no_judge)
    assert decision["action"] == "deny"
    assert "the server never starts" in decision["reason"]

    passing = {"probes": [], "verdict": "pass", "unmet": []}
    state = {"denies": 0, "probes": []}
    decision = oracle.decide({}, state, judge=lambda *a: passing, prober=_no_judge)
    assert decision == {"action": "allow"}


def test_the_emitted_decision_is_the_engines_hook_vocabulary(monkeypatch):
    """The wire shape the engine folds: {"action": "deny", "reason": ...}."""
    monkeypatch.setenv("STELLA_ORACLE_TEST_COMMAND", "false")
    state = {"denies": 0, "probes": []}
    decision = oracle.decide({}, state, judge=_no_judge, prober=lambda cmd: (False, "x"))
    parsed = json.loads(json.dumps(decision))
    assert parsed["action"] == "deny"
    assert isinstance(parsed["reason"], str) and parsed["reason"]
