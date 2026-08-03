# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Tests for the parts a wrong answer would quietly corrupt a contest.

The bias here is toward the code paths where a bug produces a *plausible*
number rather than an error: solve-rate denominators, leader selection with
partial data, cache-token accounting, and the incremental transcript cursor.
A benchmark tool that crashes gets fixed; one that silently reports 71% when
the answer is 64% does not.

No Docker, no network, no model key: every fixture is a synthetic trial tree.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from arenabench.agents import (
    AGENTS,
    launch_model,
    missing_credentials,
    resolve_agent,
)
from arenabench.harbor_agent import arena_posture
from arenabench.model import (
    DIMENSIONS,
    DIMENSIONS_BY_KEY,
    Contestant,
    Engine,
    MatchSpec,
    RoleConfig,
    parse_dotenv,
    slugify,
)
from arenabench.registry import DEFAULT_REGISTRY, Task, sample_tasks
from arenabench.runner import ContestantRun, MatchRunner, _base_environment
from arenabench.telemetry import (
    MetricsReader,
    TranscriptReader,
    TrialMetrics,
    aggregate,
    leaders,
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
            {"type": "tool_start", "call": {"id": "c1", "name": "bash", "arguments": {"cmd": "git reflog"}}},
            {"type": "tool_result", "call_id": "c1", "output": "abc123 HEAD@{0}"},
            usage(input_tokens=8000, output_tokens=900, cached_input_tokens=6000, cache_write_tokens=1500, cost_usd=0.042),
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
# model
# --------------------------------------------------------------------------


class TestDotenv:
    def test_export_prefix_and_quotes_are_stripped(self):
        env = parse_dotenv('export A=1\nB="two"\nC=\'three\'')
        assert env == {"A": "1", "B": "two", "C": "three"}

    def test_a_multiline_double_quoted_value_survives(self):
        """A PEM key pasted into the box must not be truncated at line one.

        This is the realistic failure: an operator pastes a whole `.env`
        containing an SSH key, and a naive line-based parser keeps only
        `-----BEGIN RSA PRIVATE KEY-----`, producing a credential that is
        present, wrong, and fails far away from the paste box.
        """
        env = parse_dotenv('KEY="-----BEGIN-----\nabc\ndef\n-----END-----"\nAFTER=1')
        assert env["KEY"].splitlines() == ["-----BEGIN-----", "abc", "def", "-----END-----"]
        assert env["AFTER"] == "1", "parsing must resume after the closing quote"

    def test_values_are_never_interpolated(self):
        """`FOO=$BAR` stays literal: pasting must not pull in arena env."""
        assert parse_dotenv("FOO=$BAR")["FOO"] == "$BAR"

    def test_comments_blank_lines_and_junk_are_skipped(self):
        env = parse_dotenv("# c\n\nNOEQ\n9BAD=x\nOK=1")
        assert env == {"OK": "1"}


class TestEngine:
    def test_qualified_model_is_idempotent(self):
        bare = Engine(api="openrouter", model="z-ai/glm-5.2")
        already = Engine(api="openrouter", model="openrouter/z-ai/glm-5.2")
        assert bare.qualified_model == already.qualified_model == "openrouter/z-ai/glm-5.2"

    def test_a_role_inherits_the_baseline_unless_it_overrides(self):
        engine = Engine(effort="xhigh", reasoning=True, roles={"judge": RoleConfig(effort="low")})
        assert engine.effective_role("worker").effort == "xhigh"
        assert engine.effective_role("judge").effort == "low"
        assert engine.effective_role("judge").reasoning is True

    def test_round_trips_through_json(self):
        engine = Engine(
            api="anthropic", model="claude-opus-5", effort="max", reasoning=False,
            max_tokens=64000, roles={"worker": RoleConfig(model="claude-sonnet-5")},
        )
        assert Engine.from_json(engine.to_json()) == engine


class TestMatchSpec:
    def test_duplicate_seat_names_are_disambiguated(self):
        """Two seats named the same is a *normal* thing to do when comparing
        two engines of one agent — but the slug becomes a Harbor job directory
        and Harbor refuses to reuse one."""
        spec = MatchSpec.from_json(
            {
                "dataset": "terminal-bench-2.1",
                "contestants": [
                    {"name": "stella", "agent": "stella", "engine": {"model": "a"}},
                    {"name": "stella", "agent": "stella", "engine": {"model": "b"}},
                ],
            }
        )
        slugs = [c.slug for c in spec.contestants]
        assert len(set(slugs)) == 2, slugs

    def test_validate_reports_every_problem_at_once(self):
        spec = MatchSpec.from_json({"contestants": [{"name": "x", "agent": "stella"}]})
        problems = spec.validate()
        assert any("dataset" in p for p in problems)
        assert any("model" in p for p in problems)

    def test_env_never_round_trips_to_the_client(self):
        contestant = Contestant.from_json(
            {"name": "s", "agent": "stella", "env": "OPENROUTER_API_KEY=sk-secret"}
        )
        payload = json.dumps(contestant.redacted())
        assert "sk-secret" not in payload
        assert "OPENROUTER_API_KEY" in payload, "key names are disclosed, values are not"


def test_slugify_produces_a_safe_job_name():
    assert slugify("  Stella (GLM 5.2) — xhigh!  ") == "stella-glm-5-2-xhigh"


# --------------------------------------------------------------------------
# dimensions and leaders
# --------------------------------------------------------------------------


class TestDimensions:
    def test_lower_is_better_for_cost_and_clock(self):
        assert DIMENSIONS_BY_KEY["total_cost"].better(1.0, 2.0)
        assert DIMENSIONS_BY_KEY["clock_time"].better(10, 20)

    def test_cache_read_is_higher_is_better(self):
        """Cache reads are prompt tokens you did not pay full price for."""
        assert DIMENSIONS_BY_KEY["cache_read"].better(9000, 10)

    def test_cache_write_crowns_nobody(self):
        """Writes are a real cost paid to enable future reads, so neither
        direction is self-evidently better and the scoreboard must not claim
        one is."""
        dim = DIMENSIONS_BY_KEY["cache_write"]
        assert dim.direction == "neutral"
        assert not dim.better(1, 2) and not dim.better(2, 1)


class TestLeaders:
    def test_a_contestant_with_no_judged_trial_cannot_lead(self):
        """The bug this pins: a seat that has not spent anything has cost 0,
        clock 0 and tokens 0 — so a naive "lowest wins" hands it four crowns
        for the first minutes of every match."""
        totals = {
            "spent": {"judged": 4, "passed": 3, "solve_rate": 75.0, "total_cost": 2.0, "clock_time": 300, "tokens_in": 10, "tokens_out": 10, "cache_read": 5},
            "idle": {"judged": 0, "passed": 0, "solve_rate": 0.0, "total_cost": 0.0, "clock_time": 0, "tokens_in": 0, "tokens_out": 0, "cache_read": 0},
        }
        won = leaders(totals, DIMENSIONS)
        assert all(winners == ["spent"] for winners in won.values()), won

    def test_ties_return_every_tied_contestant(self):
        totals = {
            "a": {"judged": 1, "solve_rate": 50.0, "total_cost": 1.0, "clock_time": 5, "tokens_in": 1, "tokens_out": 1, "cache_read": 1},
            "b": {"judged": 1, "solve_rate": 50.0, "total_cost": 1.0, "clock_time": 5, "tokens_in": 1, "tokens_out": 1, "cache_read": 1},
        }
        assert sorted(leaders(totals, DIMENSIONS)["solve_rate"]) == ["a", "b"]

    def test_no_judged_trials_anywhere_crowns_nobody(self):
        assert leaders({"a": {"judged": 0}}, DIMENSIONS) == {}


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

    def test_the_cache_is_invalidated_when_the_file_grows(self, trial: Path):
        reader = MetricsReader()
        first = reader.read(trial, "fix-git")
        write_events(trial / "agent" / "stella-events.jsonl", [usage(cost_usd=1.0)])
        second = reader.read(trial, "fix-git")
        assert second.total_cost > first.total_cost


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


# --------------------------------------------------------------------------
# posture and agents
# --------------------------------------------------------------------------


class TestPosture:
    def test_every_automatic_selector_is_off(self):
        posture, _, _ = arena_posture("openrouter/z-ai/glm-5.2", Engine(model="z-ai/glm-5.2"))
        assert posture["auto_mode"] == "off"
        assert posture["effort_auto"] == "off"
        assert posture["reasoning_auto"] == "off"

    def test_the_effort_the_operator_picked_reaches_the_worker(self):
        engine = Engine(model="z-ai/glm-5.2", effort="max", reasoning=True)
        posture, _, _ = arena_posture("openrouter/z-ai/glm-5.2", engine)
        assert posture["agents"]["worker"] == {"effort": "max", "reasoning": "on"}

    def test_triage_does_not_inherit_an_expensive_baseline(self):
        engine = Engine(model="z-ai/glm-5.2", effort="xhigh")
        posture, _, _ = arena_posture("openrouter/z-ai/glm-5.2", engine)
        assert posture["agents"]["triage"] == {"effort": "low", "reasoning": "off"}

    def test_but_an_explicit_triage_override_still_wins(self):
        engine = Engine(model="m", effort="xhigh", roles={"triage": RoleConfig(effort="high")})
        posture, _, _ = arena_posture("openrouter/m", engine)
        assert posture["agents"]["triage"]["effort"] == "high"

    def test_a_role_model_is_qualified_with_the_workers_provider(self):
        """A pin typed as a bare slug is silently dropped by the engine, so
        the provider is inferred from the model that *is* routed."""
        engine = Engine(model="z-ai/glm-5.2", roles={"judge": RoleConfig(model="openai/gpt-5.5")})
        posture, _, _ = arena_posture("openrouter/z-ai/glm-5.2", engine)
        assert posture["pipeline_judge_model"] == "openrouter/openai/gpt-5.5"
        assert "openrouter/openai/gpt-5.5" in posture["allowed_models"]

    def test_the_digest_changes_when_the_configuration_does(self):
        _, _, a = arena_posture("openrouter/m", Engine(model="m", effort="high"))
        _, _, b = arena_posture("openrouter/m", Engine(model="m", effort="max"))
        assert a != b, "a changed posture must be a changed identity"

    def test_the_digest_is_stable_for_the_same_configuration(self):
        _, _, a = arena_posture("openrouter/m", Engine(model="m", effort="high"))
        _, _, b = arena_posture("openrouter/m", Engine(model="m", effort="high"))
        assert a == b

    def test_an_unrouted_model_is_refused(self):
        with pytest.raises(ValueError):
            arena_posture("glm-5.2", Engine(model="glm-5.2"))

    def test_only_root_keys_the_engine_accepts_are_emitted(self):
        """Stella's trusted-launcher seam fails closed on an unknown root key,
        so an extra field here would refuse the run rather than be ignored."""
        allowed = {
            "default_model", "pipeline_judge_model", "pipeline_worker_model",
            "pipeline_triage_model", "allowed_models", "auto_mode", "effort_auto",
            "reasoning_auto", "headless_scope_bypass", "agents",
        }
        engine = Engine(model="m", roles={r: RoleConfig(model="x") for r in ("worker", "judge", "triage")})
        posture, _, _ = arena_posture("openrouter/m", engine)
        assert set(posture) <= allowed


class TestAgentRegistry:
    def test_stella_is_arenabench_supplied_and_the_rest_are_harbor_builtins(self):
        assert resolve_agent("stella").import_path is not None
        assert resolve_agent("claude-code").harbor_agent == "claude-code"

    def test_an_unknown_agent_fails_loudly(self):
        """Defaulting would run a contest other than the one that was asked
        for, and report a clean number for it."""
        with pytest.raises(KeyError):
            resolve_agent("not-an-agent")

    def test_a_knob_the_agent_ignores_is_reported(self):
        spec = resolve_agent("aider")
        missed = spec.unhonoured(Engine(model="m", roles={"judge": RoleConfig(effort="low")}))
        assert any("pipeline" in m for m in missed)

    def test_an_unset_knob_is_not_reported(self):
        assert resolve_agent("aider").unhonoured(Engine(model="m")) == []

    def test_a_missing_credential_is_named(self):
        seat = Contestant.from_json({"name": "s", "agent": "stella", "engine": {"api": "anthropic"}})
        assert missing_credentials(seat) == ["ANTHROPIC_API_KEY"]

    def test_any_one_of_several_alternatives_satisfies(self):
        seat = Contestant.from_json(
            {"name": "s", "agent": "gemini-cli", "engine": {"api": "google"}, "env": "GOOGLE_API_KEY=x"}
        )
        assert missing_credentials(seat) == []


# --------------------------------------------------------------------------
# registry
# --------------------------------------------------------------------------


class TestRegistry:
    def test_terminal_bench_is_registered_and_pinned_by_digest(self):
        dataset = DEFAULT_REGISTRY.get("terminal-bench-2.1")
        assert dataset is not None
        assert dataset.digest.startswith("sha256:")

    def test_an_unfetched_dataset_yields_no_tasks_rather_than_raising(self, tmp_path):
        from arenabench.registry import Dataset, Registry

        registry = Registry().add(
            Dataset(key="ghost", title="Ghost", harbor_id="x@sha256:y", namespace="nope")
        )
        assert registry.tasks("ghost") == []

    def test_an_export_directory_shadows_the_shared_cache(self, tmp_path):
        from arenabench.registry import Dataset, Registry

        (tmp_path / "solo").mkdir()
        (tmp_path / "solo" / "task.toml").write_text(
            '[task]\nname = "ns/solo"\ndescription = "d"\n'
            '[metadata]\ndifficulty = "easy"\n[environment]\nmemory_mb = 2048\ncpus = 1\n',
            encoding="utf-8",
        )
        registry = Registry().add(
            Dataset(key="k", title="K", harbor_id="x@sha256:y", namespace="ns")
        )
        registry.export_dirs["k"] = tmp_path
        tasks = registry.tasks("k")
        assert [t.name for t in tasks] == ["solo"]
        assert tasks[0].difficulty == "easy"


def test_every_registered_agent_declares_how_it_launches():
    from arenabench.agents import launch_flags

    for slug in AGENTS:
        seat = Contestant.from_json({"name": slug, "agent": slug, "engine": {"model": "m"}})
        assert launch_flags(seat), slug


class TestRecorder:
    """The recorder is optional, so its failures must stay cheap and quiet.

    Nothing here starts a container. Both behaviours under test are decisions
    the supervisor makes *before* it shells out, which is exactly why they are
    reachable without Docker.
    """

    def _supervisor(self, tmp_path: Path):
        from arenabench.recorder import RecorderSupervisor

        return RecorderSupervisor(jobs_root=tmp_path, jobs=["job"])

    def test_the_task_platform_pin_is_not_inherited_by_the_recorder(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ):
        """A benchmark host exports ``DOCKER_DEFAULT_PLATFORM=linux/amd64``
        because Terminal-Bench task images publish amd64 only. The recorder is
        built locally for the host's own architecture and never runs task code,
        so inheriting the pin sends Docker looking for an amd64 variant of an
        arm64 image — which it cannot find locally and then tries to *pull*
        from a registry that has no such repository. The operator sees "image
        not found" and goes hunting for a missing image that is sitting right
        there.
        """
        monkeypatch.setenv("DOCKER_DEFAULT_PLATFORM", "linux/amd64")
        monkeypatch.setenv("ARENA_UNRELATED", "keep-me")

        env = self._supervisor(tmp_path)._docker_env()

        assert "DOCKER_DEFAULT_PLATFORM" not in env
        # Scrubbing one variable must not amount to running with a bare
        # environment: Docker still needs DOCKER_HOST, PATH and friends.
        assert env["ARENA_UNRELATED"] == "keep-me"

    def test_the_ambient_environment_is_not_mutated(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ):
        """The pin is dropped from a *copy*. Popping it from ``os.environ``
        itself would silently unpin every task container started afterwards,
        turning a recorder fix into a benchmark-integrity bug."""
        import os

        monkeypatch.setenv("DOCKER_DEFAULT_PLATFORM", "linux/amd64")
        self._supervisor(tmp_path)._docker_env()
        assert os.environ["DOCKER_DEFAULT_PLATFORM"] == "linux/amd64"

    def test_a_trial_that_cannot_be_filmed_is_abandoned_not_retried_forever(
        self, tmp_path: Path
    ):
        """The watcher polls every two seconds for the life of the trial, so an
        unrecoverable start failure is not one error — it is one error per poll
        for as long as the trial runs, each a registry round-trip, scrolling
        the log an operator is watching the match in."""
        supervisor = self._supervisor(tmp_path)
        trial_dir = tmp_path / "job" / "t__1"

        for _ in range(supervisor.max_start_attempts - 1):
            supervisor._record_failure(trial_dir, "no such image")
            assert trial_dir not in supervisor._finished

        supervisor._record_failure(trial_dir, "no such image")
        assert trial_dir in supervisor._finished

        # `_finished` is the short-circuit `_consider` checks first, so being
        # in it is what actually stops the retries.
        (trial_dir / "agent").mkdir(parents=True)
        supervisor._start_container = lambda *a, **k: pytest.fail(  # type: ignore[method-assign]
            "gave up on this trial but tried to start it again"
        )
        supervisor._consider(trial_dir, "job")

    def test_a_recovered_trial_starts_its_failure_count_over(self, tmp_path: Path):
        """The cap counts *consecutive* failures. A trial that starts on the
        second attempt and later needs restarting should not be one strike from
        being abandoned for the rest of the match."""
        supervisor = self._supervisor(tmp_path)
        trial_dir = tmp_path / "job" / "t__1"

        supervisor._record_failure(trial_dir, "transient")
        assert supervisor._failures[trial_dir] == 1

        supervisor._failures.pop(trial_dir, None)  # what a successful start does
        supervisor._record_failure(trial_dir, "transient")
        assert supervisor._failures[trial_dir] == 1
        assert trial_dir not in supervisor._finished


# --------------------------------------------------------------------------
# routing an agent off its home provider
# --------------------------------------------------------------------------


def _seat(env: str = "", **engine: object) -> Contestant:
    return Contestant.from_json(
        {"name": "cc", "agent": "claude-code", "engine": engine, "env": env}
    )


class TestDirectProviderRouting:
    """Seating Claude Code on a non-Anthropic, Anthropic-shaped endpoint.

    Harbor's Claude Code agent reads ``ANTHROPIC_BASE_URL`` and, when one is
    set, forwards the model name to that endpoint unchanged. Both halves of
    that sentence are load-bearing, and each has its own failure: no base URL
    and the seat silently runs on Anthropic; a prefixed model name and every
    trial dies with model-not-found, which on a scoreboard is indistinguishable
    from an agent that cannot code.
    """

    def test_claude_code_declares_that_it_honours_a_base_url(self):
        spec = resolve_agent("claude-code")
        assert "base_url" in spec.honours
        assert spec.unhonoured(Engine(model="glm-5.2", base_url="https://x/y")) == []

    def test_an_agent_with_nowhere_to_put_a_base_url_still_says_so(self):
        assert "base_url" in resolve_agent("aider").unhonoured(
            Engine(model="m", base_url="https://x/y")
        )

    def test_a_routed_seat_is_launched_with_the_providers_own_model_id(self):
        seat = _seat(api="zai", model="glm-5.2", base_url="https://api.z.ai/api/anthropic")
        assert launch_model(seat) == "glm-5.2"

    def test_an_unrouted_seat_keeps_harbors_provider_prefix(self):
        seat = _seat(api="anthropic", model="claude-opus-4-5")
        assert launch_model(seat) == "anthropic/claude-opus-4-5"

    def test_baring_a_model_strips_only_the_api_prefix(self):
        """An OpenRouter id keeps its vendor segment: ``z-ai`` is part of the
        name OpenRouter publishes, not a route ArenaBench added."""
        assert Engine(api="openrouter", model="z-ai/glm-5.2").bare_model == "z-ai/glm-5.2"
        assert Engine(api="openrouter", model="openrouter/z-ai/glm-5.2").bare_model == (
            "z-ai/glm-5.2"
        )
        assert Engine(api="zai", model="zai/glm-5.2").bare_model == "glm-5.2"

    def _routing(self, seat: Contestant, tmp_path: Path) -> tuple[dict, list[str]]:
        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path)
        run = ContestantRun(
            contestant=seat,
            job_name="job",
            job_dir=tmp_path / "job",
            log_path=tmp_path / "job.log",
        )
        return runner._routing_environment(seat, run), run.notes

    def test_a_provider_key_is_aliased_into_the_variable_the_agent_reads(
        self, tmp_path: Path
    ):
        seat = _seat(
            env="ZAI_API_KEY=zk-secret",
            api="zai",
            model="glm-5.2",
            base_url="https://api.z.ai/api/anthropic",
        )
        env, notes = self._routing(seat, tmp_path)
        assert env["ANTHROPIC_BASE_URL"] == "https://api.z.ai/api/anthropic"
        assert env["ANTHROPIC_AUTH_TOKEN"] == "zk-secret"
        assert any("ZAI_API_KEY" in note for note in notes), (
            "aliasing a credential must be visible on the seat, not silent"
        )

    def test_an_explicitly_pasted_token_is_never_overwritten(self, tmp_path: Path):
        seat = _seat(
            env="ZAI_API_KEY=zk-provider\nANTHROPIC_AUTH_TOKEN=zk-explicit",
            api="zai",
            model="glm-5.2",
            base_url="https://api.z.ai/api/anthropic",
        )
        env, _ = self._routing(seat, tmp_path)
        assert "ANTHROPIC_AUTH_TOKEN" not in env, (
            "an operator's own choice outranks ArenaBench's inference"
        )

    def test_a_seat_with_no_base_url_is_not_rerouted(self, tmp_path: Path):
        seat = _seat(env="ANTHROPIC_API_KEY=sk", api="anthropic", model="claude-opus-4-5")
        env, notes = self._routing(seat, tmp_path)
        assert env == {} and notes == []

    def test_either_credential_name_satisfies_a_routed_seat(self):
        base = "https://api.z.ai/api/anthropic"
        provider = _seat(env="ZAI_API_KEY=k", api="zai", model="glm-5.2", base_url=base)
        agent_side = _seat(
            env="ANTHROPIC_AUTH_TOKEN=k", api="zai", model="glm-5.2", base_url=base
        )
        assert missing_credentials(provider) == []
        assert missing_credentials(agent_side) == []
        assert missing_credentials(
            _seat(api="zai", model="glm-5.2", base_url=base)
        ) == [
            "ZAI_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ]


class TestRandomTaskSampling:
    """A seeded draw, because "we ran ten random tasks" is otherwise unfalsifiable."""

    def _tasks(self, count: int) -> list[Task]:
        return [
            Task(
                name=f"task-{index:02d}",
                qualified=f"ns/task-{index:02d}",
                memory_mb=8192 if index % 5 == 0 else 2048,
            )
            for index in range(count)
        ]

    def test_the_same_seed_draws_the_same_tasks(self):
        pool = self._tasks(50)
        assert sample_tasks(pool, 10, 7) == sample_tasks(pool, 10, 7)

    def test_a_different_seed_draws_a_different_slice(self):
        pool = self._tasks(50)
        assert sample_tasks(pool, 10, 7) != sample_tasks(pool, 10, 8)

    def test_the_draw_comes_back_in_dataset_order(self):
        """Which ten were chosen is the finding; the order they left the
        generator in carries no information and would only make two selections
        harder to compare."""
        drawn = sample_tasks(self._tasks(50), 10, 7)
        assert [task.name for task in drawn] == sorted(task.name for task in drawn)

    def test_asking_for_more_than_exists_yields_everything(self):
        pool = self._tasks(6)
        assert sample_tasks(pool, 99, 1) == pool

    def test_excluding_heavy_narrows_the_population_drawn_from(self):
        pool = self._tasks(50)
        drawn = sample_tasks(pool, 10, 7, exclude_heavy=True)
        assert len(drawn) == 10
        assert not any(task.heavy for task in drawn)

    def test_a_memory_ceiling_narrows_the_pool(self):
        """`heavy` is calibrated for a bigger machine than the one in front of
        you. A host giving Docker 8 GB and racing two contestants can afford
        4 GB each, which excludes tasks `heavy` happily keeps."""
        pool = self._tasks(50)
        drawn = sample_tasks(pool, 10, 7, max_memory_mb=4096)
        assert drawn and all(task.memory_mb <= 4096 for task in drawn)
        assert any(task.memory_mb == 8192 for task in pool), "fixture must have some"

    def test_a_ceiling_that_excludes_everything_draws_nothing(self):
        assert sample_tasks(self._tasks(50), 10, 7, max_memory_mb=1) == []


class TestSubscriptionCredential:
    """Claude Code seated on a Claude subscription rather than metered credits.

    Harbor forwards `CLAUDE_CODE_OAUTH_TOKEN` into the container and the CLI
    picks whichever auth method is actually present, so a seat carrying only
    that token runs on the plan. Two things must hold for it to be safe.
    """

    def test_a_subscription_token_credentials_the_seat(self):
        seat = Contestant.from_json({
            "name": "cc", "agent": "claude-code",
            "engine": {"api": "anthropic", "model": "claude-fable-5"},
            "env": "CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat-x",
        })
        assert missing_credentials(seat) == []

    def test_a_subscription_token_is_never_written_to(self, tmp_path: Path):
        """It is not a provider bearer token. Aliasing a provider key into it
        would produce a seat that cannot authenticate at all — so it counts as
        credentials present without ever becoming an alias target."""
        seat = Contestant.from_json({
            "name": "cc", "agent": "claude-code",
            "engine": {"api": "zai", "model": "glm-5.2",
                       "base_url": "https://api.z.ai/api/anthropic"},
            "env": "ZAI_API_KEY=zk",
        })
        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path)
        run = ContestantRun(contestant=seat, job_name="j",
                            job_dir=tmp_path / "j", log_path=tmp_path / "j.log")
        env = runner._routing_environment(seat, run)
        assert env["ANTHROPIC_AUTH_TOKEN"] == "zk"
        assert "CLAUDE_CODE_OAUTH_TOKEN" not in env

    def test_an_ambient_subscription_token_never_reaches_a_seat(self, monkeypatch):
        """The scrub list is prefix-based and `CLAUDE_CODE_` is not one of the
        prefixes, so this needs its own entry. Without it, a token exported in
        the operator's shell silently credentials *every* Claude Code seat —
        two arms you believed were on different credentials, both on the plan.
        """
        monkeypatch.setenv("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat-ambient")
        assert "CLAUDE_CODE_OAUTH_TOKEN" not in _base_environment()


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
