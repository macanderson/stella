# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The match watcher, witnessed against the run that motivated it.

Match ``dd52a57a6f49`` (2026-08-04, Claude Code vs the Stella pipeline, both
on Fable 5) was diagnosed by hand hours after it finished: a rate-limited arm
scored as three losses, a three-step "complete" that passed nothing, and a
verification failure as a trial's final word. The tracked archive
``fixtures/matches/dd52a57a6f49.zip`` — unpacked by ``conftest.py`` before
collection — replays that match, and the witness tests
here assert the watcher reports exactly those detections — and stays silent
on the seven trials where nothing was wrong, because a watcher that cries on
healthy trials trains its consumers to ignore it.

The synthetic tests pin the guards: every rule keys on outcome *plus* shape,
so a legitimately short pass, an arm torn down early, and an error the agent
recovered from must all fire nothing.
"""

from __future__ import annotations

import json
import os
import time
from pathlib import Path

import pytest

from arenabench.cli import main
from arenabench.monitor import (
    ALL_RULES,
    DEFAULT_RULES,
    PROTOCOL,
    PROTOCOL_VERSION,
    MatchWatcher,
    Thresholds,
)
from arenabench.telemetry import MetricsReader

FIXTURE_MATCH = Path(__file__).parent / "fixtures" / "matches" / "dd52a57a6f49"

CLAUDE_ARM = "claude-code-fable-5"
STELLA_ARM = "stella-fable-5-pipeline"


# --------------------------------------------------------------------------
# fixture helpers (synthetic trials, same idiom as test_telemetry)
# --------------------------------------------------------------------------


def write_events(path: Path, events: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        for event in events:
            handle.write(json.dumps(event) + "\n")


def usage(**kwargs) -> dict:
    base = {
        "type": "step_usage",
        "step": 1,
        "role": "worker",
        "model": "openrouter/z-ai/glm-5.2",
        "input_tokens": 1000,
        "output_tokens": 500,
        "cached_input_tokens": 0,
        "cache_write_tokens": 0,
        "cost_usd": 0.01,
        "duration_ms": 1200,
        "tool_calls": 1,
    }
    base.update(kwargs)
    return base


def result_json(trial_dir: Path, *, reward=None, exception: str = "") -> None:
    payload: dict = {}
    if reward is not None:
        payload["verifier_result"] = {"rewards": {"reward": reward}}
    if exception:
        payload["exception_info"] = {"exception_type": exception}
    trial_dir.mkdir(parents=True, exist_ok=True)
    (trial_dir / "result.json").write_text(json.dumps(payload), encoding="utf-8")


def match_tree(tmp_path: Path) -> tuple[Path, Path]:
    """An empty match directory with one agent's job dir."""
    match_dir = tmp_path / "m1"
    job_dir = match_dir / "jobs" / "m1-agent-a"
    job_dir.mkdir(parents=True)
    return match_dir, job_dir


# --------------------------------------------------------------------------
# the witness: match dd52a57a6f49, replayed
# --------------------------------------------------------------------------


class TestMatchDD52A57A6F49:
    """The definition of done for #1480, asserted line by line."""

    def test_reports_exactly_the_five_hand_diagnosed_detections(self):
        watcher = MatchWatcher(FIXTURE_MATCH)
        found = {(d.agent, d.task, d.rule): d.severity for d in watcher.scan()}
        assert found == {
            (CLAUDE_ARM, "fix-git", "zero-token"): "critical",
            (CLAUDE_ARM, "nginx-request-logging", "zero-token"): "critical",
            (CLAUDE_ARM, "openssl-selfsigned-cert", "zero-token"): "critical",
            (STELLA_ARM, "large-scale-text-editing", "late-verdict"): "warning",
            (STELLA_ARM, "sqlite-with-gcov", "premature-complete"): "warning",
        }, (
            "the watcher must find every hand-diagnosed failure of this match "
            "and nothing on its seven healthy trials"
        )

    def test_the_agent_timeout_with_real_spend_is_not_a_zero_token_void(self):
        """Claude Code's sqlite-with-gcov trial timed out after spending
        225k tokens. It had its budget and spent it — a loss, not a void,
        and the watcher must know the difference."""
        watcher = MatchWatcher(FIXTURE_MATCH)
        fired = {(d.agent, d.task) for d in watcher.scan() if d.rule == "zero-token"}
        assert (CLAUDE_ARM, "sqlite-with-gcov") not in fired

    def test_usage_incomplete_is_opt_in_and_fires_when_armed(self):
        armed = MatchWatcher(FIXTURE_MATCH, rules=ALL_RULES)
        notices = [d for d in armed.scan() if d.rule == "usage-incomplete"]
        assert len(notices) == 6
        assert all(d.severity == "notice" for d in notices)
        assert all(d.agent == STELLA_ARM for d in notices)

    def test_the_zero_token_trials_are_scored_as_voids_not_losses(self):
        """The scoring half of #1480: with a nonzero step count and zero
        observed spend, `resolved` must be None (unjudged) and the trial
        counted as infrastructure — the scoreboard read 'Claude Code lost 3
        tasks' when the truth was 'never made a single model call'."""
        trial = next(
            (FIXTURE_MATCH / "jobs" / f"dd52a57a6f49-{CLAUDE_ARM}").glob("fix-git__*")
        )
        metrics = MetricsReader().read(trial, "fix-git")
        assert metrics.steps > 0
        assert metrics.tokens_in == 0 and metrics.tokens_out == 0
        assert metrics.infrastructure is True
        assert metrics.resolved is None, "an infrastructure void, not a loss"

    def test_the_timeout_with_real_spend_still_scores_as_a_loss(self):
        trial = next(
            (FIXTURE_MATCH / "jobs" / f"dd52a57a6f49-{CLAUDE_ARM}").glob(
                "sqlite-with-gcov__*"
            )
        )
        metrics = MetricsReader().read(trial, "sqlite-with-gcov")
        assert metrics.tokens_in > 0
        assert metrics.infrastructure is False
        assert metrics.resolved is False


class TestWatchCommand:
    """`arenabench watch` as CI consumes it: lines, exit codes, the stream."""

    def test_one_shot_prints_the_detections_and_exits_3(self, capsys):
        code = main(["watch", str(FIXTURE_MATCH)])
        lines = [
            line for line in capsys.readouterr().out.splitlines() if line.strip()
        ]
        assert code == 3, "a zero-token arm invalidates the match"
        assert len(lines) == 5
        assert sum("zero-token" in line for line in lines) == 3
        assert sum("premature-complete" in line for line in lines) == 1
        assert sum("late-verdict" in line for line in lines) == 1

    def test_jsonl_emits_protocol_events_framed_by_watching_and_end(self, capsys):
        code = main(["watch", str(FIXTURE_MATCH), "--format", "jsonl"])
        events = [
            json.loads(line)
            for line in capsys.readouterr().out.splitlines()
            if line.strip()
        ]
        assert code == 3
        assert all(event["protocol"] == PROTOCOL for event in events)
        assert all(event["v"] == PROTOCOL_VERSION for event in events)
        assert all(event["run"] == "dd52a57a6f49" for event in events)
        assert [e["kind"] for e in events[:1]] == ["watching"]
        assert events[-1]["kind"] == "end"
        assert events[-1]["invalidating"] is True
        assert events[-1]["detections"] == {"notice": 0, "warning": 2, "critical": 3}
        detections = [e for e in events if e["kind"] == "detection"]
        assert len(detections) == 5
        # Everything the evidence line cites rides structured too — a
        # consumer never parses prose.
        zero = next(d for d in detections if d["rule"] == "zero-token")
        assert zero["data"]["steps"] == 2
        assert zero["data"]["failure"] == "NonZeroAgentExitCodeError"

    def test_a_clean_match_exits_0(self, tmp_path, capsys):
        match_dir, job_dir = match_tree(tmp_path)
        trial = job_dir / "task__1"
        write_events(
            trial / "agent" / "stella-events.jsonl",
            [usage(), {"type": "complete"}],
        )
        result_json(trial, reward=1.0)
        assert main(["watch", str(match_dir)]) == 0
        assert capsys.readouterr().out.strip() == ""

    def test_an_unknown_rule_is_a_usage_error(self, capsys):
        assert main(["watch", str(FIXTURE_MATCH), "--rules", "zero-day"]) == 2
        assert "unknown rule" in capsys.readouterr().err

    def test_a_missing_match_is_a_usage_error_not_a_clean_pass(
        self, tmp_path, capsys
    ):
        assert main(["watch", "no-such-match", "--workspace", str(tmp_path)]) == 2


# --------------------------------------------------------------------------
# the guards: outcome plus shape, never shape alone
# --------------------------------------------------------------------------


class TestRuleGuards:
    def test_a_legitimately_short_pass_fires_nothing(self, tmp_path):
        """A 3-step pass is a real result. The premature-complete floors
        describe when a *failure's* shape needs explaining — applied to a
        pass they would punish efficiency."""
        match_dir, job_dir = match_tree(tmp_path)
        trial = job_dir / "quick__1"
        write_events(
            trial / "agent" / "stella-events.jsonl",
            [
                usage(step=0, output_tokens=40),
                usage(step=1, output_tokens=50),
                usage(step=2, output_tokens=30),
                {"type": "complete"},
            ],
        )
        result_json(trial, reward=1.0)
        assert MatchWatcher(match_dir).scan() == []

    def test_a_short_failure_without_the_agents_own_claim_fires_nothing(
        self, tmp_path
    ):
        """Torn down early also looks short. Only the agent's own `complete`
        makes shortness a *confident* zero rather than an interrupted one."""
        match_dir, job_dir = match_tree(tmp_path)
        trial = job_dir / "cut__1"
        write_events(
            trial / "agent" / "stella-events.jsonl",
            [usage(step=0, output_tokens=40)],
        )
        result_json(trial, reward=0.0, exception="AgentTimeoutError")
        fired = [d.rule for d in MatchWatcher(match_dir).scan()]
        assert "premature-complete" not in fired

    def test_a_declared_complete_short_failure_fires(self, tmp_path):
        match_dir, job_dir = match_tree(tmp_path)
        trial = job_dir / "confident__1"
        write_events(
            trial / "agent" / "stella-events.jsonl",
            [usage(step=0, output_tokens=40), {"type": "complete"}],
        )
        result_json(trial, reward=0.0)
        fired = [d for d in MatchWatcher(match_dir).scan()]
        assert [d.rule for d in fired] == ["premature-complete"]
        assert fired[0].severity == "warning"

    def test_an_unjudged_completion_waits_for_the_verdict(self, tmp_path):
        """Declared complete, verifier not yet spoken: firing now would tag
        every fast pass in the window before its verdict lands."""
        match_dir, job_dir = match_tree(tmp_path)
        trial = job_dir / "pending__1"
        write_events(
            trial / "agent" / "stella-events.jsonl",
            [usage(step=0, output_tokens=40), {"type": "complete"}],
        )
        assert MatchWatcher(match_dir).scan() == []

    def test_an_error_the_agent_recovered_from_is_not_a_late_verdict(
        self, tmp_path
    ):
        """A mid-run verification failure the agent then works on is the
        system working — only an error nothing followed is 'too late'."""
        match_dir, job_dir = match_tree(tmp_path)
        trial = job_dir / "recovered__1"
        write_events(
            trial / "agent" / "stella-events.jsonl",
            [
                usage(step=0),
                {"type": "error", "message": "verification failed: probe says no"},
                usage(step=1),
                {"type": "complete"},
            ],
        )
        result_json(trial, reward=0.0)
        fired = [d.rule for d in MatchWatcher(match_dir).scan()]
        assert "late-verdict" not in fired

    def test_a_terminal_verification_failure_fires(self, tmp_path):
        match_dir, job_dir = match_tree(tmp_path)
        trial = job_dir / "late__1"
        write_events(
            trial / "agent" / "stella-events.jsonl",
            [
                # Enough steps and tokens that the shape is unremarkable —
                # only the terminal error is wrong with this trial.
                *[usage(step=i, output_tokens=1000) for i in range(6)],
                {"type": "error", "message": "verification failed: probe says no"},
                {"type": "complete"},
            ],
        )
        result_json(trial, reward=0.0)
        fired = [d for d in MatchWatcher(match_dir).scan()]
        assert [d.rule for d in fired] == ["late-verdict"]
        assert "verification failed" in fired[0].evidence

    def test_zero_token_waits_for_the_trial_to_conclude(self, tmp_path):
        """A trajectory-only arm writes its file at the end; steps with no
        tokens and no result yet is a trial in flight, not a void."""
        match_dir, job_dir = match_tree(tmp_path)
        trial = job_dir / "inflight__1"
        (trial / "agent").mkdir(parents=True)
        (trial / "agent" / "trajectory.json").write_text(
            json.dumps({"steps": [{}, {}]}), encoding="utf-8"
        )
        assert MatchWatcher(match_dir).scan() == []

    def test_a_stalled_running_trial_fires_and_a_lively_one_does_not(
        self, tmp_path
    ):
        match_dir, job_dir = match_tree(tmp_path)
        lively = job_dir / "lively__1"
        write_events(lively / "agent" / "stella-events.jsonl", [usage()])
        stalled = job_dir / "stalled__1"
        events = stalled / "agent" / "stella-events.jsonl"
        write_events(events, [usage()])
        old = time.time() - 20 * 60
        os.utime(events, (old, old))

        fired = MatchWatcher(match_dir, thresholds=Thresholds(stall_minutes=10)).scan()
        assert [(d.task, d.rule) for d in fired] == [("stalled", "stall")]

    def test_a_trajectory_only_arm_that_goes_silent_fires_stall(self, tmp_path):
        """The hole #1571 closes: an arm that streams no events — every
        non-Stella arm — still tees its stdout incrementally. A stale tee,
        no event stream, no verdict: the trial is hung, and before the fix
        it could burn its whole time allowance without a detection."""
        match_dir, job_dir = match_tree(tmp_path)
        tee = job_dir / "hung__1" / "agent" / "claude-code.txt"
        tee.parent.mkdir(parents=True)
        tee.write_text("booted\n", encoding="utf-8")
        old = time.time() - 20 * 60
        os.utime(tee, (old, old))

        fired = MatchWatcher(match_dir, thresholds=Thresholds(stall_minutes=10)).scan()
        assert [(d.task, d.rule) for d in fired] == [("hung", "stall")]

    def test_a_trajectory_only_arm_still_writing_does_not_stall(self, tmp_path):
        match_dir, job_dir = match_tree(tmp_path)
        tee = job_dir / "busy__1" / "agent" / "claude-code.txt"
        tee.parent.mkdir(parents=True)
        tee.write_text("working\n", encoding="utf-8")

        watcher = MatchWatcher(match_dir, thresholds=Thresholds(stall_minutes=10))
        assert watcher.scan() == []

    def test_liveness_is_the_allowlist_not_the_tree(self, tmp_path):
        """A fresh write by the arena's own side must not read as agent
        liveness — the freshest mtime of the whole tree would count the
        recorder and the verifier as the agent's pulse, and a hung arm
        being *watched* would never stall."""
        match_dir, job_dir = match_tree(tmp_path)
        trial = job_dir / "hung__1"
        tee = trial / "agent" / "claude-code.txt"
        tee.parent.mkdir(parents=True)
        tee.write_text("booted\n", encoding="utf-8")
        old = time.time() - 20 * 60
        os.utime(tee, (old, old))
        (trial / "arena").mkdir()
        (trial / "arena" / "recording.mp4").write_bytes(b"")

        fired = MatchWatcher(match_dir, thresholds=Thresholds(stall_minutes=10)).scan()
        assert [(d.task, d.rule) for d in fired] == [("hung", "stall")]

    def test_a_fresh_session_transcript_is_liveness_despite_a_stale_tee(
        self, tmp_path
    ):
        """Appending to a transcript under agent/sessions/ touches no
        directory mtime — only the file's own. The probe must look inside,
        or an arm whose stdout went quiet while its session log still grows
        would fire a false stall."""
        match_dir, job_dir = match_tree(tmp_path)
        agent = job_dir / "quiet__1" / "agent"
        sessions = agent / "sessions" / "project"
        sessions.mkdir(parents=True)
        (agent / "claude-code.txt").write_text("quiet\n", encoding="utf-8")
        (sessions / "session.jsonl").write_text("{}\n", encoding="utf-8")
        old = time.time() - 20 * 60
        for stale in (agent / "claude-code.txt", sessions, sessions.parent):
            os.utime(stale, (old, old))

        watcher = MatchWatcher(match_dir, thresholds=Thresholds(stall_minutes=10))
        assert watcher.scan() == []


class TestSubscription:
    """What makes --follow a subscription rather than a report."""

    def test_a_detection_is_emitted_exactly_once_across_scans(self, tmp_path):
        match_dir, job_dir = match_tree(tmp_path)
        trial = job_dir / "confident__1"
        write_events(
            trial / "agent" / "stella-events.jsonl",
            [usage(step=0, output_tokens=40), {"type": "complete"}],
        )
        result_json(trial, reward=0.0)
        watcher = MatchWatcher(match_dir)
        assert len(watcher.scan()) == 1
        assert watcher.scan() == [], "a fact does not become more true"

    def test_a_trial_appearing_later_is_still_detected(self, tmp_path):
        match_dir, job_dir = match_tree(tmp_path)
        watcher = MatchWatcher(match_dir)
        assert watcher.scan() == []
        trial = job_dir / "confident__1"
        write_events(
            trial / "agent" / "stella-events.jsonl",
            [usage(step=0, output_tokens=40), {"type": "complete"}],
        )
        result_json(trial, reward=0.0)
        assert [d.rule for d in watcher.scan()] == ["premature-complete"]

    def test_counts_and_invalidating_accumulate(self, tmp_path):
        match_dir, job_dir = match_tree(tmp_path)
        trial = job_dir / "void__1"
        (trial / "agent").mkdir(parents=True)
        (trial / "agent" / "trajectory.json").write_text(
            json.dumps({"steps": [{}, {}]}), encoding="utf-8"
        )
        result_json(trial, exception="NonZeroAgentExitCodeError")
        watcher = MatchWatcher(match_dir)
        watcher.scan()
        assert watcher.invalidating is True
        assert watcher.counts["critical"] == 1

    def test_the_agent_name_is_the_job_dir_minus_the_match_prefix(self, tmp_path):
        match_dir, _ = match_tree(tmp_path)
        assert list(MatchWatcher(match_dir).agents()) == ["agent-a"]

    def test_default_rules_are_the_four_that_explain_or_invalidate(self):
        assert DEFAULT_RULES == (
            "zero-token",
            "premature-complete",
            "late-verdict",
            "stall",
        )
        with pytest.raises(ValueError, match="unknown rule"):
            MatchWatcher(FIXTURE_MATCH, rules=("zero-day",))
