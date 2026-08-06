# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The contest vocabulary: what a seat is, and how one gets launched.

The bias here is toward the code paths where a bug produces a *plausible*
contest rather than an error — a credential aliased into a variable the agent
does not read, a role override filed under a key nothing consults, a task filter
that matches nothing. A benchmark tool that crashes gets fixed; one that
silently ran something other than the match you asked for does not.

Its siblings hold the rest on the same principle: `test_telemetry.py` reads
artifacts into numbers, `test_pricing.py` and `test_leaderboard.py` decide what
those numbers are allowed to mean, and `test_security.py` holds the local-only
boundary.

No Docker, no network, no model key: every fixture is synthetic.
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
from arenabench.config import match_from_toml, match_to_toml_dict, required_env
from arenabench.harbor_agent import arena_posture
from arenabench.model import (
    ROLES,
    Contestant,
    Engine,
    MatchSpec,
    RoleConfig,
    parse_dotenv,
    slugify,
)
from arenabench.registry import DEFAULT_REGISTRY, Task, sample_tasks
from arenabench.runner import ContestantRun, MatchRunner, _base_environment
from arenabench.telemetry import seat_manifest_path


def _fake_harbor(monkeypatch, *, version: str = "0.20.0", import_path_flag: bool = False):
    """Stand in for an installed Harbor.

    `_launch` resolves the binary and asks it for a version and a flag list
    before it builds any argv, so a test that never touches Harbor still needs
    those three answers. They are stubbed at the `arenabench.harbor` seam
    rather than at `shutil.which`, because the version and the CLI shape are
    the things the launch actually branches on — stubbing the lookup alone
    would leave the real binary (or its absence) deciding the test's outcome.
    """
    monkeypatch.setattr("arenabench.harbor.harbor_bin", lambda: "/usr/bin/harbor")
    monkeypatch.setattr("arenabench.harbor.harbor_version", lambda: version)
    monkeypatch.setattr(
        "arenabench.harbor.supports_agent_import_path", lambda: import_path_flag
    )


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
        engine = Engine(
            effort="xhigh", reasoning=True, roles={"verifier": RoleConfig(effort="low")}
        )
        assert engine.effective_role("worker").effort == "xhigh"
        assert engine.effective_role("verifier").effort == "low"
        assert engine.effective_role("verifier").reasoning is True

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
        engine = Engine(
            model="z-ai/glm-5.2", roles={"verifier": RoleConfig(model="openai/gpt-5.5")}
        )
        posture, _, _ = arena_posture("openrouter/z-ai/glm-5.2", engine)
        assert posture["pipeline_verifier_model"] == "openrouter/openai/gpt-5.5"
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
            "default_model", "pipeline_verifier_model", "pipeline_worker_model",
            "pipeline_triage_model", "allowed_models", "auto_mode", "effort_auto",
            "reasoning_auto", "headless_scope_bypass", "agents",
        }
        engine = Engine(
            model="m",
            roles={r: RoleConfig(model="x") for r in ("worker", "verifier", "triage")},
        )
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
        missed = spec.unhonoured(
            Engine(model="m", roles={"verifier": RoleConfig(effort="low")})
        )
        assert any("pipeline" in m for m in missed)

    def test_an_unset_knob_is_not_reported(self):
        assert resolve_agent("aider").unhonoured(Engine(model="m")) == []

    def test_a_missing_credential_is_named(self):
        seat = Contestant.from_json(
            {"name": "s", "agent": "stella", "engine": {"api": "anthropic"}}
        )
        assert missing_credentials(seat) == ["ANTHROPIC_API_KEY"]

    def test_any_one_of_several_alternatives_satisfies(self):
        seat = Contestant.from_json(
            {
                "name": "s",
                "agent": "gemini-cli",
                "engine": {"api": "google"},
                "env": "GOOGLE_API_KEY=x",
            }
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

    def test_required_env_names_the_subscription_alternative(self):
        """The CLI collects seat env against `required_env`. If that list
        carries only the provider key, an OAuth-only seat aborts at preflight
        and the token would never be forwarded — so the agent's own token
        variables must appear as candidates alongside the provider's."""
        spec = match_from_toml({
            "match": {"dataset": "terminal-bench-2.1"},
            "contestant": [{
                "agent": "claude-code",
                "engine": {"api": "anthropic", "model": "claude-fable-5"},
            }],
        })
        candidates = required_env(spec)[spec.contestants[0].id]
        assert "ANTHROPIC_API_KEY" in candidates
        assert "CLAUDE_CODE_OAUTH_TOKEN" in candidates

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


class TestOfflineTaskSource:
    """Reading tasks from an export instead of resolving them mid-run.

    Harbor resolves a registry ref against its backend once per task, at run
    time. One failed lookup raises out of the job and takes every remaining
    trial with it — the failure that ended two measured matches here, at task
    3 and task 7, killing both contestants together.
    """

    def _export(self, tmp_path: Path, tasks=("alpha", "beta")) -> Path:
        root = tmp_path / "terminal-bench-2.1" / "terminal-bench-2-1"
        for name in tasks:
            d = root / name
            d.mkdir(parents=True)
            (d / "task.toml").write_text(f'[task]\nname = "terminal-bench/{name}"\n')
        return root

    def test_an_export_is_found_and_is_the_directory_harbor_wants(
        self, tmp_path: Path, monkeypatch
    ):
        expected = self._export(tmp_path)
        monkeypatch.setenv("ARENABENCH_DATASETS", str(tmp_path))
        assert DEFAULT_REGISTRY.local_run_path("terminal-bench-2.1") == expected

    def test_no_export_means_no_path_rather_than_a_wrong_one(
        self, tmp_path: Path, monkeypatch
    ):
        monkeypatch.setenv("ARENABENCH_DATASETS", str(tmp_path / "empty"))
        assert DEFAULT_REGISTRY.local_run_path("terminal-bench-2.1") is None

    def test_harbors_own_cache_is_never_offered_as_a_run_path(
        self, tmp_path: Path, monkeypatch
    ):
        """The cache nests each task under its content hash. ArenaBench can
        enumerate that, but `--path` reads it as an empty dataset — which fails
        the run with 'no tasks matched' rather than anything diagnostic."""
        cached = tmp_path / "terminal-bench-2.1" / "terminal-bench-2-1" / "alpha" / "deadbeef"
        cached.mkdir(parents=True)
        (cached / "task.toml").write_text('[task]\nname = "terminal-bench/alpha"\n')
        monkeypatch.setenv("ARENABENCH_DATASETS", str(tmp_path))
        assert DEFAULT_REGISTRY.local_run_path("terminal-bench-2.1") is None

    def test_the_package_directory_comes_from_the_pinned_name(self):
        from arenabench.registry import TERMINAL_BENCH_21
        assert TERMINAL_BENCH_21.package_dir == "terminal-bench-2-1"

    def test_a_backfilled_record_never_invents_a_harbor_version(self, tmp_path: Path):
        """The whole point of the record is that a version you can trust looks
        different from one nobody measured. Harbor writes its version nowhere,
        so a reconstructed record must say so — a plausible guess here would be
        indistinguishable from a real reading, which is the confusion this
        exists to prevent."""
        from arenabench import provenance

        job = tmp_path / "jobs" / "m-seat"
        job.mkdir(parents=True)
        (job / "config.json").write_text(
            json.dumps({
                "job_name": "m-seat",
                # No `install_only` / `extra_instruction_paths`: the shape a
                # pre-0.20.0 Harbor wrote.
                "datasets": [{
                    "name": "terminal-bench/terminal-bench-2-1",
                    "ref": "sha256:7d7bdc1cbedad549fc1140404bd4dc45e5fd0"
                           "ea7c4186773687d177ad3a0699a",
                }],
            })
        )

        record = provenance.backfill_match(tmp_path)
        assert record is not None
        assert record.harbor_version is None, "a guessed version is worse than none"
        assert record.harbor_bound == "<0.20.0"
        assert record.measured is False
        assert record.source == provenance.SOURCE_BACKFILLED
        # The digest IS recoverable here, and must be recovered.
        assert record.dataset_digest.endswith("3a0699a")
        assert record.dataset_key == "terminal-bench-2.1"

    def test_a_measured_key_never_collides_with_an_unmeasured_one(self):
        """Two result sets are only comparable when the apparatus matches. An
        unknown Harbor must not produce the same grouping key as a known one,
        or the labelling silently permits exactly the mixing it forbids."""
        from arenabench.provenance import Provenance

        measured = Provenance(
            dataset_key="terminal-bench-2.1", dataset_digest="sha256:7d7bdc1c",
            harbor_version="0.6.1",
        )
        unmeasured = Provenance(
            dataset_key="terminal-bench-2.1", dataset_digest="sha256:7d7bdc1c",
            harbor_version=None, harbor_bound="<0.20.0",
        )
        newer = Provenance(
            dataset_key="terminal-bench-2.1", dataset_digest="sha256:7d7bdc1c",
            harbor_version="0.20.0",
        )
        keys = {measured.comparability_key, unmeasured.comparability_key,
                newer.comparability_key}
        assert len(keys) == 3, keys

    def test_frontier_bench_declares_the_harbor_it_needs_to_grade_correctly(self):
        """The floor is the point of the entry, not a nicety.

        Every Frontier-Bench task sets `environment_mode = "separate"`, which
        Harbor 0.6.1 drops silently — the run finishes and reports a score
        graded against the wrong container topology. An entry without this
        field would be an invitation to publish that number.
        """
        from arenabench.registry import FRONTIER_BENCH
        assert FRONTIER_BENCH.min_harbor == "0.20.0"
        assert DEFAULT_REGISTRY.get("frontier-bench") is not None

    def _launched_command(self, tmp_path: Path, monkeypatch, export: bool) -> list[str]:
        """The argv `_launch` actually hands Harbor, with the process stubbed."""
        if export:
            self._export(tmp_path, tasks=("alpha",))
            monkeypatch.setenv("ARENABENCH_DATASETS", str(tmp_path))
        else:
            monkeypatch.setenv("ARENABENCH_DATASETS", str(tmp_path / "nope"))
        _fake_harbor(monkeypatch)

        seen: dict = {}

        class _Fake:
            def __init__(self, command, **kwargs):
                seen["command"] = command
                self.returncode = None

            def poll(self):
                return None

        monkeypatch.setattr("arenabench.runner.subprocess.Popen", _Fake)
        spec = MatchSpec.from_json({
            "dataset": "terminal-bench-2.1", "tasks": ["alpha"],
            "contestants": [{"name": "s", "agent": "stella",
                             "engine": {"api": "openrouter", "model": "x/y"}}],
        })
        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / "ws")
        match = runner.create(spec)
        runner._launch(match, spec.contestants[0])
        return seen["command"]

    def test_an_export_is_filtered_by_bare_task_name(self, tmp_path: Path, monkeypatch):
        """Names are namespaced by the registry, not by the task. An export on
        disk is just directories and answers to the bare name — sending the
        qualified form matches nothing and ends the match at once."""
        command = self._launched_command(tmp_path, monkeypatch, export=True)
        assert "--path" in command and "--dataset" not in command
        assert command[command.index("--include-task-name") + 1] == "alpha"

    def test_a_registry_ref_is_filtered_by_qualified_task_name(
        self, tmp_path: Path, monkeypatch
    ):
        command = self._launched_command(tmp_path, monkeypatch, export=False)
        assert "--dataset" in command and "--path" not in command
        assert command[command.index("--include-task-name") + 1] == "terminal-bench/alpha"


class TestSeatLaunchRecord:
    """`_launch` leaves the one artifact that can say which route a job used.

    A directly-routed seat is launched with the bare model id, so nothing
    Harbor records for it can be told apart from a gateway seat by spelling —
    the record beside the job directory is what pricing reads instead (#1498).
    """

    def test_the_route_is_recorded_beside_the_job_directory(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ):
        _fake_harbor(monkeypatch)

        class _Fake:
            def __init__(self, command, **kwargs):
                self.returncode = None

            def poll(self):
                return None

        monkeypatch.setattr("arenabench.runner.subprocess.Popen", _Fake)
        spec = MatchSpec.from_json(
            {
                "dataset": "terminal-bench-2.1",
                "tasks": ["alpha"],
                "contestants": [
                    {
                        "name": "cc",
                        "agent": "claude-code",
                        "engine": {
                            "api": "zai",
                            "model": "glm-5.2",
                            "base_url": "https://api.z.ai/api/anthropic",
                        },
                    }
                ],
            }
        )
        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / "ws")
        match = runner.create(spec)
        run = runner._launch(match, spec.contestants[0])

        record = json.loads(seat_manifest_path(run.job_dir).read_text(encoding="utf-8"))
        # The two spellings genuinely diverge for this seat — that divergence
        # is why the record has to exist at all.
        assert record["launch_model"] == "glm-5.2"
        assert record["qualified_model"] == "zai/glm-5.2"
        assert record["api"] == "zai"


class TestMatchTemplateRoles:
    """A role override in a committed match file must reach the engine.

    This is the silent shape this file exists for. A loader that files a role
    under a key nothing reads does not raise, does not fail, and does not stop
    the match: the seat runs with that role inheriting the baseline while the
    scoreboard reports it as configured, so the contest publishes a number for
    a pairing it never ran. `arenabench.toml` and Stella's engine spelled this
    role differently for one release and the translation between them outlived
    the difference, which is exactly how such a key survives review.
    """

    @staticmethod
    def _spec(role_name: str) -> MatchSpec:
        return match_from_toml(
            {
                "match": {"name": "t", "dataset": "terminal-bench-2.1"},
                "contestant": [
                    {
                        "id": "stella",
                        "agent": "stella",
                        "engine": {
                            "model": "z-ai/glm-5.2",
                            "effort": "medium",
                            "roles": {role_name: {"model": "openai/gpt-5.5"}},
                        },
                    }
                ],
            }
        )

    def test_a_role_override_lands_on_a_key_the_engine_reads(self):
        engine = self._spec("verifier").contestants[0].engine
        stray = set(engine.roles) - set(ROLES)
        assert not stray, f"{stray} is outside ROLES, so nothing reads it"
        assert engine.effective_role("verifier").model == "openai/gpt-5.5"

    def test_the_retired_spelling_still_loads(self):
        # A match file committed before the rename must keep running.
        engine = self._spec("judge").contestants[0].engine
        assert engine.effective_role("verifier").model == "openai/gpt-5.5"

    def test_nothing_writes_the_retired_spelling_back_out(self):
        raw = match_to_toml_dict(self._spec("judge"))
        roles = raw["contestant"][0]["engine"]["roles"]
        assert "verifier" in roles and "judge" not in roles


class TestSnapshotDetections:
    """The monitor's verdicts ride the snapshot the web UI renders (#1569).

    Match dd52a57a6f49's five hand-diagnosed detections are pinned by
    `test_monitor.py`; here the same fixture is served through the arena and
    the snapshot payload must carry exactly those five, attributed to
    contestant ids rather than job-dir slugs. An operator watching the web UI
    must see the same dead arm the terminal watcher reports — a detection
    that only ever reached a terminal is the exact silence #1480 was filed
    about, one surface over.
    """

    FIXTURE = Path(__file__).parent / "fixtures" / "matches" / "dd52a57a6f49"

    def _fixture_arena(self, tmp_path: Path):
        from arenabench.runner import Match
        from arenabench.server import ArenaServer

        # Names chosen so the slugs match the fixture's job directories
        # (`dd52a57a6f49-claude-code-fable-5`, `dd52a57a6f49-stella-…`).
        spec = MatchSpec.from_json(
            {
                "id": "dd52a57a6f49",
                "name": "replayed",
                "dataset": "terminal-bench-2.1",
                "contestants": [
                    {
                        "id": "cc",
                        "name": "claude code fable 5",
                        "agent": "claude-code",
                        "engine": {"api": "anthropic", "model": "claude-fable-5"},
                    },
                    {
                        "id": "st",
                        "name": "stella fable 5 pipeline",
                        "agent": "stella",
                        "engine": {"api": "openrouter", "model": "z-ai/glm-5.2"},
                    },
                ],
            }
        )
        arena = ArenaServer(tmp_path / "ws")
        match = Match(spec, DEFAULT_REGISTRY.get("terminal-bench-2.1"), self.FIXTURE)
        match.status = "finished"
        arena.runner.matches[spec.id] = match
        return arena, spec.id

    def test_the_snapshot_carries_the_five_pinned_detections(self, tmp_path: Path):
        arena, match_id = self._fixture_arena(tmp_path)
        payload = arena.match(match_id)
        found = {
            (d["contestant"], d["task"], d["rule"]): d["severity"]
            for d in payload["detections"]
        }
        assert found == {
            ("cc", "fix-git", "zero-token"): "critical",
            ("cc", "nginx-request-logging", "zero-token"): "critical",
            ("cc", "openssl-selfsigned-cert", "zero-token"): "critical",
            ("st", "large-scale-text-editing", "late-verdict"): "warning",
            ("st", "sqlite-with-gcov", "premature-complete"): "warning",
        }
        # The structured evidence reaches the client too — the UI never has
        # to parse prose.
        zero = next(d for d in payload["detections"] if d["rule"] == "zero-token")
        assert zero["data"]["steps"] == 2
        assert "never made a model call" in zero["evidence"]

    def test_detections_accumulate_across_snapshots_rather_than_draining(
        self, tmp_path: Path
    ):
        """The watcher reports each fact once; the snapshot must not. A
        second SSE tick — or a second client — gets the same five, or the UI
        would flash a banner for one poll interval and go quiet."""
        arena, match_id = self._fixture_arena(tmp_path)
        first = arena.match(match_id)["detections"]
        second = arena.match(match_id)["detections"]
        assert len(first) == 5
        assert second == first

