"""The wait parser and the `blind-wait` detector, against real command text.

Every command string below was sent by a trial in arenabench match
`13f7f2bb533d`. They are kept verbatim because the discrimination this code
has to make is between two shapes that look alike and cost 13x differently:
a blind `sleep N`, which always costs N, and a poll loop, which costs one pass
when the check passes early. That match measured the loop at 20.9s and the
blind wait beside it at 280.4s.
"""

from __future__ import annotations

from detectors import run_all
from fixtures import ok, write_run
from run_trace import load_run
from waits import ADVISORY_BOUND_SECS, REFUSAL_BOUND_SECS, blocking_sleep_seconds

# The two calls that dominated the panel's tool time.
BACKGROUNDED_INSTALL = (
    "timeout 500 pip install --quiet torch --index-url "
    "https://download.pytorch.org/whl/cpu 2>&1 | tail -30 &\n"
    "BGPID=$!\nsleep 490\nwait $BGPID 2>/dev/null\necho done"
)
BLIND_POLL = "sleep 280; tail -30 /tmp/apt_install.log; echo ---; ps aux|grep apt"

# The same agent, in the same trial, getting it right.
POLL_LOOP = (
    'for i in $(seq 1 25); do sleep 20; if ! ps aux | grep -q "[a]pt-get install"; '
    "then echo DONE; break; fi; echo waiting $i; done; which g++ gcc"
)
NOHUP_START = (
    "nohup apt-get install -y g++ > /tmp/apt_install.log 2>&1 & sleep 2; echo started"
)
# The shape the advisory's own remedy text recommends.
CHECK_FIRST_LOOP = "for i in $(seq 30); do curl -sf http://localhost:8080 && break; sleep 1; done"


def test_a_backgrounded_install_is_not_part_of_the_wait():
    """The install runs beside the wait; only the `sleep` is the wait."""
    assert blocking_sleep_seconds(BACKGROUNDED_INSTALL) == 490
    assert blocking_sleep_seconds(BLIND_POLL) == 280


def test_a_poll_loop_is_read_as_a_pass_or_not_at_all():
    """Neither reading of a poll loop reaches the bound.

    A segment counts only when it is exactly `sleep N`. `do sleep 20;` keeps
    the loop keyword in the segment, so it reads as no sleep; the same loop
    written `…; sleep 1; done` reads as one second, which is one pass rather
    than the worst case. Both are far under the bound, so the cheap pattern is
    never declined. The seconds this predicate cannot see are checked against
    the Rust it mirrors in `crates/stella-core/src/shell_text.rs`.
    """
    assert blocking_sleep_seconds(POLL_LOOP) is None
    assert blocking_sleep_seconds(CHECK_FIRST_LOOP) == 1
    assert blocking_sleep_seconds(NOHUP_START) == 2


def test_the_bounds_separate_the_two_shapes():
    assert ADVISORY_BOUND_SECS < REFUSAL_BOUND_SECS
    for command in (BACKGROUNDED_INSTALL, BLIND_POLL):
        assert blocking_sleep_seconds(command) >= REFUSAL_BOUND_SECS, command
    for command in (POLL_LOOP, CHECK_FIRST_LOOP, NOHUP_START):
        assert (blocking_sleep_seconds(command) or 0) < REFUSAL_BOUND_SECS, command


def test_a_command_that_never_sleeps_reports_nothing():
    assert blocking_sleep_seconds("grep sleep /app/main.c") is None
    assert blocking_sleep_seconds("cargo build --release") is None
    # A duration this parser cannot read is not a wait it may invent.
    assert blocking_sleep_seconds("sleep $DELAY") is None


def _bash_call(call_id: str, command: str, *, started_ms: int, ended_ms: int):
    return [
        {
            "ts": started_ms,
            "type": "tool_start",
            "call": {"call_id": call_id, "name": "bash", "input": {"command": command}},
        },
        {"ts": ended_ms, "type": "tool_result", "call_id": call_id, "output": ok("done")},
    ]


def test_the_detector_fires_on_the_blind_wait_and_leaves_the_poll_loop_alone(tmp_path):
    write_run(
        tmp_path,
        [
            {
                "task": "rstan-to-pystan__AAA",
                "reward": 0,
                "events": [
                    *_bash_call("blind", BLIND_POLL, started_ms=0, ended_ms=280_400),
                    *_bash_call("loop", POLL_LOOP, started_ms=300_000, ended_ms=320_900),
                    {"ts": 1, "type": "step_usage", "role": "worker", "model": "m"},
                ],
            }
        ],
    )
    findings = [f for f in run_all(load_run(tmp_path, "testrun")) if f.detector == "blind-wait"]
    assert len(findings) == 1
    finding = findings[0]
    assert finding.count == 1, "the poll loop is not a blind wait"
    only = finding.occurrences[0]
    assert "280s of `sleep` asked for" in only.location
    assert "280.4s spent" in only.location
    assert "sleep 280" in only.excerpt


def test_the_detector_is_silent_when_nothing_waits(tmp_path):
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 1,
                "events": [
                    *_bash_call("build", "cargo build", started_ms=0, ended_ms=4_000),
                    {"ts": 1, "type": "step_usage", "role": "worker", "model": "m"},
                ],
            }
        ],
    )
    assert not [
        f for f in run_all(load_run(tmp_path, "testrun")) if f.detector == "blind-wait"
    ]
