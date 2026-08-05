# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Turning a trial's artifacts into numbers, and numbers into totals.

The bias is toward the code paths where a bug produces a *plausible* number
rather than an error: solve-rate denominators, cache-token accounting, cost that
was taken on trust, and the incremental transcript cursor. A benchmark tool that
crashes gets fixed; one that silently reports 71% when the answer is 64% does
not.

No Docker, no network, no model key: every fixture is a synthetic trial tree.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from arenabench.telemetry import (
    MetricsReader,
    TranscriptReader,
    TrialMetrics,
    aggregate,
    seat_manifest_path,
)

# --------------------------------------------------------------------------
# fixtures
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
        "complete": True,
    }
    base.update(kwargs)
    return base


@pytest.fixture
def trial(tmp_path: Path) -> Path:
    trial_dir = tmp_path / "job" / "fix-git__1"
    write_events(
        trial_dir / "agent" / "stella-events.jsonl",
        [
            {"type": "stage", "name": "execute"},
            {"type": "reasoning", "delta": "let me look "},
            {"type": "reasoning", "delta": "at the reflog"},
            {
                "type": "tool_start",
                "call": {"id": "c1", "name": "bash", "arguments": {"cmd": "git reflog"}},
            },
            {"type": "tool_result", "call_id": "c1", "output": "abc123 HEAD@{0}"},
            usage(
                input_tokens=8000,
                output_tokens=900,
                cached_input_tokens=6000,
                cache_write_tokens=1500,
                cost_usd=0.042,
            ),
            {"type": "text", "delta": "Recovered the commit."},
            {"type": "complete", "model": "openrouter/z-ai/glm-5.2", "cost_usd": 0.042},
        ],
    )
    (trial_dir / "result.json").write_text(
        json.dumps(
            {
                "verifier_result": {"reward": 1.0},
                "started_at": "2026-08-02T10:00:00",
                "finished_at": "2026-08-02T10:04:30",
            }
        ),
        encoding="utf-8",
    )
    return trial_dir



# --------------------------------------------------------------------------
# telemetry
# --------------------------------------------------------------------------


class TestMetrics:
    def test_reads_every_scoreboard_dimension_from_artifacts(self, trial: Path):
        metrics = MetricsReader().read(trial, "fix-git")
        assert metrics.status == "done"
        assert metrics.resolved is True
        assert metrics.tokens_in == 8000
        assert metrics.tokens_out == 900
        assert metrics.cache_read == 6000
        assert metrics.cache_write == 1500
        assert metrics.total_cost == pytest.approx(0.042)
        assert metrics.clock_time == pytest.approx(270.0)
        assert metrics.tools == 1

    def test_a_running_trial_is_not_reported_as_failed(self, tmp_path: Path):
        """`resolved` stays None until a verifier speaks. Coercing an
        unjudged trial to False is what turns 'still running' into a
        published loss."""
        trial_dir = tmp_path / "job" / "t__1"
        write_events(trial_dir / "agent" / "stella-events.jsonl", [usage()])
        metrics = MetricsReader().read(trial_dir, "t")
        assert metrics.status == "running"
        assert metrics.resolved is None

    def test_a_torn_final_line_does_not_lose_the_earlier_events(self, tmp_path: Path):
        """A live file is being appended to while it is read, so half a JSON
        object at the tail is expected rather than exceptional."""
        path = tmp_path / "job" / "t__1" / "agent" / "stella-events.jsonl"
        write_events(path, [usage(cost_usd=0.5)])
        with path.open("a", encoding="utf-8") as handle:
            handle.write('{"type":"step_usage","input_to')
        metrics = MetricsReader().read(path.parent.parent, "t")
        assert metrics.total_cost == pytest.approx(0.5)

    def test_an_agent_exception_marks_the_trial_failed(self, tmp_path: Path):
        trial_dir = tmp_path / "job" / "t__1"
        (trial_dir / "agent").mkdir(parents=True)
        (trial_dir / "result.json").write_text(
            json.dumps({"exception_info": {"exception_type": "NonZeroAgentExitCodeError"}}),
            encoding="utf-8",
        )
        metrics = MetricsReader().read(trial_dir, "t")
        assert metrics.resolved is False
        assert metrics.failure == "NonZeroAgentExitCodeError"

    def test_atif_only_trials_read_the_real_atif_field_names(self, tmp_path: Path):
        """The trajectory fallback is the ONLY source for a non-Stella agent.

        ATIF spells these ``total_prompt_tokens`` / ``total_completion_tokens``;
        the plausible-looking ``total_input_tokens`` / ``total_output_tokens``
        do not exist in the format. Reading the wrong key does not raise — it
        returns zero, so a Claude Code arm that really spent 40k tokens and $2
        would post 0 tokens and $0.00 and win every efficiency dimension on the
        scoreboard. Wrong-but-plausible is exactly the failure a benchmark tool
        must not have.
        """
        trial_dir = tmp_path / "job" / "t__1"
        (trial_dir / "agent").mkdir(parents=True)
        (trial_dir / "agent" / "trajectory.json").write_text(
            json.dumps(
                {
                    "steps": [{"tool_calls": [{"name": "bash"}]}],
                    "final_metrics": {
                        "total_prompt_tokens": 40_000,
                        "total_completion_tokens": 3_200,
                        "total_cached_tokens": 31_000,
                        "total_cost_usd": 2.15,
                        "extra": {"total_cache_write_tokens": 4_100},
                    },
                }
            ),
            encoding="utf-8",
        )
        metrics = MetricsReader().read(trial_dir, "t")
        assert metrics.tokens_in == 40_000
        assert metrics.tokens_out == 3_200
        assert metrics.cache_read == 31_000
        assert metrics.cache_write == 4_100
        assert metrics.total_cost == pytest.approx(2.15)

    def test_cost_is_recomputed_from_tokens_rather_than_taken_on_trust(
        self, trial: Path
    ):
        """The self-report is kept, and it is not what the scoreboard ranks.
        This trial says $0.042; its own token counts through the shared table
        say a tenth of that, and the second number is the one two seats can be
        compared on."""
        metrics = MetricsReader().read(trial, "fix-git")
        assert metrics.total_cost == pytest.approx(0.042)
        # 500 uncached in @0.76 + 6000 cache reads @0.14 + 1500 writes @0.76
        # (unbilled separately, so charged as input) + 900 out @2.42, per Mtok.
        assert metrics.priced_cost == pytest.approx(0.004538)

    def test_an_unpriced_model_leaves_the_comparable_cost_empty(self, tmp_path: Path):
        trial_dir = tmp_path / "job" / "t__1"
        write_events(
            trial_dir / "agent" / "stella-events.jsonl",
            [usage(model="acme/mystery-1", cost_usd=3.0)],
        )
        metrics = MetricsReader().read(trial_dir, "t")
        assert metrics.total_cost == pytest.approx(3.0)
        assert metrics.priced_cost is None, "missing beats a guessed number"

    def test_the_cache_is_invalidated_when_the_file_grows(self, trial: Path):
        reader = MetricsReader()
        first = reader.read(trial, "fix-git")
        write_events(trial / "agent" / "stella-events.jsonl", [usage(cost_usd=1.0)])
        second = reader.read(trial, "fix-git")
        assert second.total_cost > first.total_cost

    @staticmethod
    def _seat_on_zai(tmp_path: Path, job: str, recorded_model: str) -> Path:
        """A finished trial whose seat launched against z.ai first-party.

        `recorded_model` is what Harbor wrote down, which differs by routing:
        a directly-routed seat is launched (and recorded) as the bare id, an
        unrouted one as the qualified id. The launch record beside the job
        says what both spellings cannot — which route served the trial.
        """
        trial_dir = tmp_path / job / "t__1"
        (trial_dir / "agent").mkdir(parents=True)
        (trial_dir / "result.json").write_text(
            json.dumps(
                {
                    "config": {"agent": {"model_name": recorded_model}},
                    "verifier_result": {"reward": 1.0},
                }
            ),
            encoding="utf-8",
        )
        (trial_dir / "agent" / "trajectory.json").write_text(
            json.dumps(
                {
                    "final_metrics": {
                        "total_prompt_tokens": 1_000_000,
                        "total_completion_tokens": 0,
                    }
                }
            ),
            encoding="utf-8",
        )
        seat_manifest_path(trial_dir.parent).write_text(
            json.dumps(
                {
                    "schema": 1,
                    "contestant": job,
                    "api": "zai",
                    "model": "glm-5.2",
                    "qualified_model": "zai/glm-5.2",
                    "launch_model": recorded_model,
                }
            ),
            encoding="utf-8",
        )
        return trial_dir

    def test_the_seats_launch_record_prices_the_route_it_actually_used(
        self, tmp_path: Path
    ):
        """Two seats on the identical z.ai endpoint record different model
        spellings — a directly-routed seat is launched with the bare id, an
        unrouted one with the qualified id — so the recorded strings cannot
        carry route, and pricing them route-blind billed a first-party seat
        at the gateway's ~27%-higher rate (#1498). With the launch record,
        both seats price at the rate of the endpoint they actually called,
        and at the *same* rate as each other."""
        routed = MetricsReader().read(self._seat_on_zai(tmp_path, "cc", "glm-5.2"), "t")
        unrouted = MetricsReader().read(
            self._seat_on_zai(tmp_path, "stella", "zai/glm-5.2"), "t"
        )
        # 1M uncached input tokens at z.ai's own 0.60/Mtok, not OpenRouter's 0.76.
        assert routed.priced_cost == pytest.approx(0.60)
        assert unrouted.priced_cost == pytest.approx(0.60)

    def test_an_unpriced_route_falls_back_to_the_recorded_model(self, trial: Path):
        """A launch record whose route has no price of its own must not slam
        the door: the trial prices exactly as an archive without a record
        does — from the recorded model ids, at the gateway tier."""
        seat_manifest_path(trial.parent).write_text(
            json.dumps({"schema": 1, "api": "acme", "qualified_model": "acme/mystery-1"}),
            encoding="utf-8",
        )
        metrics = MetricsReader().read(trial, "fix-git")
        assert metrics.priced_cost == pytest.approx(0.004538)



class TestAggregate:
    def test_solve_rate_divides_by_judged_not_attempted(self):
        """Dividing by attempted makes every contestant start near 0% and
        climb — an artifact of progress, not of skill."""
        from arenabench.telemetry import TrialMetrics

        trials = [
            TrialMetrics("a", "a__1", status="done", resolved=True),
            TrialMetrics("b", "b__1", status="running", resolved=None),
            TrialMetrics("c", "c__1", status="running", resolved=None),
        ]
        totals = aggregate(trials)
        assert totals["judged"] == 1
        assert totals["solve_rate"] == 100.0

    def test_no_judged_trials_is_zero_not_a_crash(self):
        assert aggregate([])["solve_rate"] == 0.0

    def test_priced_cost_sums_only_the_trials_that_have_one(self):
        trials = [
            TrialMetrics("a", "a__1", resolved=True, total_cost=9.0, priced_cost=1.5),
            TrialMetrics("b", "b__1", resolved=True, total_cost=9.0, priced_cost=None),
        ]
        totals = aggregate(trials)
        assert totals["priced_cost"] == pytest.approx(1.5)
        assert totals["total_cost"] == pytest.approx(18.0)

    def test_a_wholly_unpriced_contestant_reports_no_cost_at_all(self):
        """Not 0.0 — that is a free run, which is a claim, and a false one."""
        trials = [TrialMetrics("a", "a__1", resolved=True, total_cost=9.0)]
        assert aggregate(trials)["priced_cost"] is None


class TestTranscript:
    def test_streaming_fragments_coalesce_under_one_seq(self, tmp_path: Path):
        path = tmp_path / "e.jsonl"
        write_events(path, [
            {"type": "reasoning", "delta": "hello "},
            {"type": "reasoning", "delta": "world"},
        ])
        entries = TranscriptReader().read(path)
        assert len({e["seq"] for e in entries}) == 1
        assert entries[-1]["body"] == "hello world"

    def test_a_non_delta_event_closes_the_open_run(self, tmp_path: Path):
        path = tmp_path / "e.jsonl"
        write_events(path, [
            {"type": "reasoning", "delta": "a"},
            {"type": "stage", "name": "execute"},
            {"type": "reasoning", "delta": "b"},
        ])
        entries = TranscriptReader().read(path)
        reasoning = [e for e in entries if e["kind"] == "reasoning"]
        assert len({e["seq"] for e in reasoning}) == 2, "a new run must start a new entry"

    def test_reads_are_incremental(self, tmp_path: Path):
        path = tmp_path / "e.jsonl"
        reader = TranscriptReader()
        write_events(path, [{"type": "stage", "name": "one"}])
        assert len(reader.read(path)) == 1
        assert reader.read(path) == [], "nothing new means nothing sent"
        write_events(path, [{"type": "stage", "name": "two"}])
        assert len(reader.read(path)) == 1

    def test_an_incomplete_trailing_line_is_held_for_the_next_read(self, tmp_path: Path):
        path = tmp_path / "e.jsonl"
        reader = TranscriptReader()
        path.write_text('{"type":"stage","name":"a"}\n{"type":"sta', encoding="utf-8")
        assert len(reader.read(path)) == 1
        path.write_text(
            '{"type":"stage","name":"a"}\n{"type":"stage","name":"b"}\n', encoding="utf-8"
        )
        assert len(reader.read(path)) == 1, "the completed line arrives exactly once"

    def test_truncation_restarts_rather_than_reading_stale_bytes(self, tmp_path: Path):
        path = tmp_path / "e.jsonl"
        reader = TranscriptReader()
        write_events(path, [{"type": "stage", "name": "a"}, {"type": "stage", "name": "b"}])
        reader.read(path)
        path.write_text('{"type":"stage","name":"fresh"}\n', encoding="utf-8")
        entries = reader.read(path)
        assert [e["title"] for e in entries] == ["fresh"]

    def test_two_trials_do_not_share_streaming_buffers(self, tmp_path: Path):
        reader = TranscriptReader()
        one, two = tmp_path / "1.jsonl", tmp_path / "2.jsonl"
        write_events(one, [{"type": "reasoning", "delta": "AAA"}])
        write_events(two, [{"type": "reasoning", "delta": "BBB"}])
        assert reader.read(one)[-1]["body"] == "AAA"
        assert reader.read(two)[-1]["body"] == "BBB"


class TestInfrastructureFailuresAreNotLosses:
    """A trial the agent never got to attempt is not a trial it lost."""

    def _trial(self, tmp_path: Path, exception_type: str, reward=None) -> TrialMetrics:
        trial = tmp_path / f"t-{exception_type}"
        trial.mkdir()
        payload: dict = {"exception_info": {"exception_type": exception_type}}
        if reward is not None:
            payload["verifier_result"] = {"reward": reward}
        (trial / "result.json").write_text(json.dumps(payload))
        return MetricsReader().read(trial, "task")

    def test_a_setup_timeout_is_left_unjudged(self, tmp_path: Path):
        """Claude Code installs its toolchain per container. On a slow link
        that setup blows its budget, which is a fact about its installer and
        not about how well it codes — scoring it 0 hands the other arm a win
        that does not survive anyone opening the logs."""
        m = self._trial(tmp_path, "AgentSetupTimeoutError")
        assert m.infrastructure is True
        assert m.resolved is None, "unjudged, not lost"

    def test_an_agent_that_ran_and_quit_still_scores_zero(self, tmp_path: Path):
        m = self._trial(tmp_path, "NonZeroAgentExitCodeError")
        assert m.infrastructure is False
        assert m.resolved is False

    def test_an_agent_that_used_all_its_time_still_scores_zero(self, tmp_path: Path):
        m = self._trial(tmp_path, "AgentTimeoutError")
        assert m.infrastructure is False and m.resolved is False

    def test_a_broken_verifier_returns_no_verdict_either_way(self, tmp_path: Path):
        m = self._trial(tmp_path, "VerifierTimeoutError")
        assert m.infrastructure is True and m.resolved is None

    def test_a_trial_killed_by_the_host_is_not_a_loss(self, tmp_path: Path):
        """Restarting the match server cancels every in-flight trial. Those
        agents were mid-task and on course — one of them had already made the
        verifier pass — so scoring the cancellation 0 marks a contestant down
        for something the operator did. Unlike `AgentTimeoutError`, the agent
        never got the budget it was promised."""
        m = self._trial(tmp_path, "CancelledError")
        assert m.infrastructure is True
        assert m.resolved is None, "unjudged, not lost"

    def test_a_cancelled_trial_does_not_dilute_the_solve_rate(self, tmp_path: Path):
        """The bug this pins: two arms each solved every task they were
        allowed to finish, and both boards read 75% because the host killed
        the fourth."""
        solved = [
            TrialMetrics(task=f"t{i}", trial=f"t{i}", resolved=True) for i in range(3)
        ]
        killed = self._trial(tmp_path, "CancelledError")
        assert aggregate([*solved, killed])["solve_rate"] == 100.0

    def test_infrastructure_trials_leave_the_solve_rate_alone(self, tmp_path: Path):
        """Eight of ten never started; the arm won both it actually ran. The
        rate must say 100% of 2 judged, and the count of the missing eight must
        be visible beside it — "won 8 of 10" and "won 2 of the 2 that started"
        are different claims."""
        started = [
            TrialMetrics(task=f"ok{i}", trial=f"ok{i}", resolved=True) for i in range(2)
        ]
        never = [
            TrialMetrics(task=f"x{i}", trial=f"x{i}", failure="AgentSetupTimeoutError",
                         infrastructure=True)
            for i in range(8)
        ]
        totals = aggregate(started + never)
        assert totals["judged"] == 2
        assert totals["solve_rate"] == 100.0
        assert totals["infrastructure"] == 8

