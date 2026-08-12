"""`banned.py` as ArenaBench invokes it: a real subprocess, across the seam.

The two halves of the gate meet through a process boundary, not an import —
`arenabench/` is agent-agnostic and being prepared for ejection, and this
ledger cites Stella crate paths in every row (#3024). A boundary that neither
side's unit tests cross is a boundary where the contract rots: ArenaBench's
`tests/test_gate.py` drives its half with a *fake* checker, and everything
above in this directory drives this half by calling `evaluate` in-process.
Neither would notice if the real command stopped honouring the contract.

So this runs the actual command the way `arenabench.gate.run_gate` runs it —
`<python> banned.py <trial-dir> [--allow CODE]...`, reading exit status and
stdout — and asserts the four rules by observation:

* exit 0 is clear, exit 9 is banned;
* stdout is JSON carrying the behavior codes;
* a run it cannot read is neither (exit 2), because a checker that failed has
  said nothing about the trial and must not abort a paid run;
* `--allow` is honoured *by the gate*, since ArenaBench deliberately never
  re-derives a verdict by subtracting codes from a parsed list.

It imports nothing from ArenaBench, so the boundary is respected in the test
as well as in the code.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

from fixtures import ok, tool_pair, write_run

BANNED_EXIT = 9
GATE = Path(__file__).resolve().parent.parent / "banned.py"

_USAGE = {
    "ts": 1,
    "type": "step_usage",
    "step": 0,
    "role": "worker",
    "model": "anthropic/claude-sonnet-5",
    "complete": True,
}


def _run(root: Path, *allow: str) -> subprocess.CompletedProcess[str]:
    argv = [sys.executable, str(GATE), str(root)]
    for code in allow:
        argv += ["--allow", code]
    return subprocess.run(argv, capture_output=True, text=True, timeout=120)


def _trial(root: Path, pattern: str, answer: str, reward: float) -> Path:
    write_run(
        root,
        [
            {
                "task": "alpha__AAA",
                "reward": reward,
                "events": [
                    _USAGE,
                    *tool_pair("g1", "grep", {"pattern": pattern}, ok(answer)),
                ],
            }
        ],
        declared_worker="anthropic/claude-sonnet-5",
        declared_roles={},
    )
    return root


def test_a_banned_trial_exits_nine_with_its_codes_on_stdout(tmp_path):
    done = _run(_trial(tmp_path, "AKIA[0-9A-Z]{16}|ghp_", "(no matches)", 0))
    assert done.returncode == BANNED_EXIT, done.stderr
    payload = json.loads(done.stdout)
    assert payload["banned"] is True
    assert payload["codes"] == ["grep-ere-false-negative"]
    assert payload["tripped"][0]["warrant"]["citation"] == "#2989"


def test_a_healthy_trial_exits_zero(tmp_path):
    done = _run(_trial(tmp_path, "def static_file", "app.py:1: def static_file", 1))
    assert done.returncode == 0, done.stderr
    assert json.loads(done.stdout)["banned"] is False


def test_an_unreadable_run_is_neither_clear_nor_banned(tmp_path):
    """Exit 2. A gate that cannot read the trial has made no claim about it."""
    done = _run(tmp_path / "nothing-here")
    assert done.returncode == 2
    assert done.returncode != BANNED_EXIT


def test_allow_is_honoured_by_the_gate_itself(tmp_path):
    """ArenaBench passes the waiver in and trusts the exit code it gets back.

    It never subtracts allowed codes from a parsed report and downgrades a ban
    to clear — that path turns an unparseable report into a silent pass. So the
    waiver has to work on this side of the boundary, and this is what proves it.
    """
    root = _trial(tmp_path, "AKIA[0-9A-Z]{16}|ghp_", "(no matches)", 0)
    assert _run(root).returncode == BANNED_EXIT
    done = _run(root, "grep-ere-false-negative")
    assert done.returncode == 0, done.stderr
    payload = json.loads(done.stdout)
    assert payload["banned"] is False
    assert payload["waived"] == ["grep-ere-false-negative"]
