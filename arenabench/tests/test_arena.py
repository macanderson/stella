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

from arenabench.agents import AGENTS, missing_credentials, resolve_agent
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
from arenabench.registry import DEFAULT_REGISTRY
from arenabench.telemetry import MetricsReader, TranscriptReader, aggregate, leaders

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
