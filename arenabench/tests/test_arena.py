# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The contest vocabulary: what a seat is, and how one gets launched.

The bias here is toward the code paths where a bug produces a *plausible*
contest rather than an error — a credential aliased into a variable the agent
does not read, a role override filed under a key nothing consults, a task filter
that matches nothing. A benchmark tool that crashes gets fixed; one that
silently ran something other than the match you asked for does not.

Its siblings hold the rest on the same principle: `test_arena_launch.py`
covers routing a seat's credentials onto the process that runs it, recording
its trial, and sourcing its tasks; `test_telemetry.py` reads artifacts into
numbers, `test_pricing.py` and `test_leaderboard.py` decide what those numbers
are allowed to mean, and `test_security.py` holds the local-only boundary.

No Docker, no network, no model key: every fixture is synthetic.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from arenabench.agents import AGENTS, missing_credentials, resolve_agent
from arenabench.config import match_from_toml, match_to_toml_dict
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
from arenabench.registry import DEFAULT_REGISTRY
from arenabench.runner import ContestantRun, MatchRunner


def _fake_harbor(monkeypatch, *, version: str = "0.20.0", import_path_flag: bool = False):
    """Stand in for an installed Harbor.

    `_launch` resolves the binary and asks it for a version and a flag list
    before it builds any argv, so a test that never touches Harbor still needs
    those three answers. They are stubbed at the `arenabench.harbor` seam
    rather than at `shutil.which`, because the version and the CLI shape are
    the things the launch actually branches on — stubbing the lookup alone
    would leave the real binary (or its absence) deciding the test's outcome.
    """
    monkeypatch.setattr(
        "arenabench.harbor.harbor_bin", lambda dataset_key=None: "/usr/bin/harbor"
    )
    monkeypatch.setattr(
        "arenabench.harbor.harbor_version", lambda binary=None: version
    )
    monkeypatch.setattr(
        "arenabench.harbor.supports_agent_import_path",
        lambda binary=None: import_path_flag,
    )
    # A stubbed Harbor also answers the seat preflight (#2325): `_launch` asks
    # whether *this* binary's interpreter can import the adapter, and a stub
    # binary has no interpreter to ask. Stubbed here rather than per test so a
    # future launch test cannot forget it — the refusal itself is witnessed by
    # `TestTheRigMustBeAbleToRunTheSeatItAccepts`, which does not use this
    # helper.
    monkeypatch.setattr(
        "arenabench.adapter.stella_seat_problem", lambda binary=None, **_kw: None
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
            roles={"worker": RoleConfig(model="claude-sonnet-5")},
        )
        assert Engine.from_json(engine.to_json()) == engine

    def test_reasoning_spelled_off_as_a_string_actually_turns_it_off(self):
        """`bool("false")` is True, and that answer ran the opposite arm (#2334).

        `reasoning` is a negative selector with a True default: the only
        reason to write it at all is to turn reasoning off, so the one value
        anyone ever types was precisely the one read backwards. This repo's
        own match files prove operators write the string forms — see the
        `reasoning = "off"` role overrides in
        `arenabench/matches/glm52-claude-code-vs-stella.toml` — they simply
        happened to sit under `[contestant.engine.roles.*]`, which `RoleConfig`
        parses correctly, rather than two spaces out under the engine, which
        did not.
        """
        for spelled_off in ("false", "False", "no", "off", "0", ""):
            engine = Engine.from_json({"model": "m", "reasoning": spelled_off})
            assert engine.reasoning is False, spelled_off

        for spelled_on in (True, 1, "true", "TRUE", " yes ", "on"):
            engine = Engine.from_json({"model": "m", "reasoning": spelled_on})
            assert engine.reasoning is True, spelled_on

        # Absent keeps the documented default, and an undeclarable value keeps
        # it too: for this field the shipping configuration is reasoning ON, so
        # falling back to False would be the closed answer to the wrong question.
        assert Engine.from_json({"model": "m"}).reasoning is True
        assert Engine.from_json({"model": "m", "reasoning": "maybe"}).reasoning is True

    def test_a_toml_template_refuses_a_reasoning_it_cannot_read(self):
        """The human-authored side refuses rather than defaulting (#2334)."""
        from arenabench.config import MatchTemplateError, match_from_toml

        with pytest.raises(MatchTemplateError) as caught:
            match_from_toml(
                {
                    "dataset": "terminal-bench-2.1",
                    "contestant": [
                        {
                            "name": "s",
                            "agent": "stella",
                            "engine": {"model": "m", "reasoning": "sometimes"},
                        },
                    ],
                }
            )
        assert "reasoning" in str(caught.value)


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

    def test_agent_timeout_multiplier_survives_json_and_toml(self):
        """The agent-execution budget knob must survive every launch path.

        Terminal-Bench pins 900s of agent execution per task; Stella's
        witness stage runs after execution, so a spec that asks for more
        time and silently loses the field starves the flip measurement
        (#2109, #2089). Witness: field absent → each hop drops it.
        """
        from arenabench.config import dump_match, match_from_toml

        spec = MatchSpec.from_json(
            {
                "dataset": "terminal-bench-2.1",
                "agent_timeout_multiplier": 2.0,
                "contestants": [
                    {"name": "s", "agent": "stella", "engine": {"model": "m"}},
                ],
            }
        )
        assert spec.agent_timeout_multiplier == 2.0
        assert MatchSpec.from_json(spec.to_json()).agent_timeout_multiplier == 2.0

        import tomllib

        parsed = match_from_toml(tomllib.loads(dump_match(spec)))
        assert parsed.agent_timeout_multiplier == 2.0

        # Below-1.0 values clamp like the setup multiplier: a typo degrades
        # to the dataset default rather than shrinking every task's budget.
        clamped = MatchSpec.from_json(
            {
                "dataset": "terminal-bench-2.1",
                "agent_timeout_multiplier": 0.25,
                "contestants": [
                    {"name": "s", "agent": "stella", "engine": {"model": "m"}},
                ],
            }
        )
        assert clamped.agent_timeout_multiplier == 1.0

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
            "pipeline_triage_model", "pipeline_research_model", "pipeline_plan_model",
            "allowed_models", "auto_mode", "effort_auto",
            "reasoning_auto", "headless_scope_bypass", "agents",
        }
        engine = Engine(
            model="m",
            roles={
                r: RoleConfig(model="x")
                for r in ("worker", "verifier", "triage", "research", "plan")
            },
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


@pytest.mark.parametrize("import_path_flag", [True, False])
def test_every_registered_agent_declares_how_it_launches(monkeypatch, import_path_flag):
    """Every agent in the registry can name the flags that launch it.

    Harbor is stubbed because `launch_flags` asks the installed binary which
    of `--agent-import-path`/`--agent` it takes, and this is an assertion
    about the *registry*, not about what happens to be on PATH. Both answers
    run: the fold between those two flags is the one thing that varies here,
    and its docstring claims to work in both directions.
    """
    from arenabench.agents import launch_flags

    _fake_harbor(monkeypatch, import_path_flag=import_path_flag)
    for slug in AGENTS:
        seat = Contestant.from_json({"name": slug, "agent": slug, "engine": {"model": "m"}})
        assert launch_flags(seat), slug


def test_the_registry_declares_every_agent_harbor_actually_installs():
    """Harbor 0.6.1's `AgentFactory._AGENTS` is the ground truth for what
    `--agent <slug>` can launch; `nemo-agent`, `openhands-sdk`, and `pi` were
    installed there but absent from this registry, so an operator asking for
    any of them by slug got `resolve_agent`'s "unknown agent" error despite
    the CLI being real. `terminus`/`terminus-1` are deliberately excluded:
    they exist as `AgentName` enum members but no class is registered for
    them in Harbor's factory, so they are not actually launchable either.
    """
    for slug in ("nemo-agent", "openhands-sdk", "pi"):
        spec = resolve_agent(slug)
        assert spec.harbor_agent == slug


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


class TestFailedMatchStatus:
    """A match whose every seat died before a single trial ran must land
    `failed`, never `finished`.

    Witnessed against match 1eb525153bc5: Docker was down, both Harbor
    processes exited in two seconds with one log line each, and the arena
    stamped the run `finished` — a green pill over 0/0 trials. `failed` had
    been in the status enum from the start; nothing ever set it.
    """

    def _match(self, tmp_path: Path):
        from arenabench.runner import Match

        spec = MatchSpec.from_json(
            {
                "id": "deadbeef0000",
                "name": "kvk",
                "dataset": "terminal-bench-2.1",
                "contestants": [
                    {
                        "id": "st",
                        "name": "Stella",
                        "agent": "stella",
                        "engine": {"api": "anthropic", "model": "claude-sonnet-5"},
                    },
                    {
                        "id": "cc",
                        "name": "Claude Code",
                        "agent": "claude-code",
                        "engine": {"api": "anthropic", "model": "claude-sonnet-5"},
                    },
                ],
            }
        )
        dataset = DEFAULT_REGISTRY.get("terminal-bench-2.1")
        match = Match(spec, dataset, tmp_path / "ws")
        match.status = "running"
        return match

    def _run_for(self, match, contestant, **kwargs) -> ContestantRun:
        job_name = f"{match.spec.id}-{contestant.slug}"
        return ContestantRun(
            contestant=contestant,
            job_name=job_name,
            job_dir=match.jobs_root / job_name,
            log_path=match.workspace / f"{contestant.slug}.log",
            **kwargs,
        )

    def test_seats_that_never_launched_fail_the_match(self, tmp_path: Path):
        match = self._match(tmp_path)
        for contestant in match.spec.contestants:
            match.runs[contestant.id] = self._run_for(
                match, contestant, error="harbor: command not found"
            )
        MatchRunner(DEFAULT_REGISTRY, tmp_path)._await_completion(match)
        assert match.status == "failed"
        assert "no trial ever ran" in match.note
        assert "harbor: command not found" in match.note

    def test_instant_nonzero_exits_fail_the_match_with_the_logged_reason(
        self, tmp_path: Path
    ):
        import subprocess

        match = self._match(tmp_path)
        for contestant in match.spec.contestants:
            run = self._run_for(match, contestant)
            run.log_path.write_text(
                "Docker daemon is not running. Please start Docker and try again.\n",
                encoding="utf-8",
            )
            run.process = subprocess.Popen(["/bin/sh", "-c", "exit 1"])
            run.process.wait()
            match.runs[contestant.id] = run
        MatchRunner(DEFAULT_REGISTRY, tmp_path)._await_completion(match)
        assert match.status == "failed"
        # The operator learns why from the match itself, not the seat logs.
        assert "Docker daemon is not running" in match.note

    def test_a_match_that_produced_trials_still_finishes(self, tmp_path: Path):
        """Harbor can exit nonzero after real trials ran; that contest is
        still worth reading, and the seat badges carry the crash."""
        import subprocess

        match = self._match(tmp_path)
        for contestant in match.spec.contestants:
            run = self._run_for(match, contestant)
            (run.job_dir / "fix-git__1").mkdir(parents=True)
            run.process = subprocess.Popen(["/bin/sh", "-c", "exit 1"])
            run.process.wait()
            match.runs[contestant.id] = run
        MatchRunner(DEFAULT_REGISTRY, tmp_path)._await_completion(match)
        assert match.status == "finished"
        assert match.note == ""


class TestRestoredMatches:
    """A server restart must not erase the history sitting on disk (#1885).

    Witness: on the old code every one of these fails — the runner had no
    restore path at all, so a rebooted `ArenaServer` listed nothing and
    `arena.match(...)` raised for a match whose artifacts were fully intact.
    """

    FIXTURE = Path(__file__).parent / "fixtures" / "matches" / "dd52a57a6f49"

    def _workspace_with_fixture(self, tmp_path: Path) -> Path:
        import shutil

        workspace = tmp_path / "ws"
        (workspace / "matches").mkdir(parents=True)
        shutil.copytree(self.FIXTURE, workspace / "matches" / "dd52a57a6f49")
        return workspace

    def test_a_finished_match_on_disk_is_listed_after_boot(self, tmp_path: Path):
        from arenabench.server import ArenaServer

        arena = ArenaServer(self._workspace_with_fixture(tmp_path))
        listed = {m["id"]: m for m in arena.list_matches()["matches"]}
        assert "dd52a57a6f49" in listed
        assert listed["dd52a57a6f49"]["status"] == "finished"

    def test_a_restored_match_serves_its_snapshot_and_seats_read_done(
        self, tmp_path: Path
    ):
        from arenabench.server import ArenaServer

        arena = ArenaServer(self._workspace_with_fixture(tmp_path))
        snap = arena.match("dd52a57a6f49")
        assert snap["status"] == "finished"
        assert len(snap["contestants"]) == 2
        assert all(c["state"] == "done" for c in snap["contestants"])
        assert snap["rows"], "the task grid must come back from the job dirs"
        # History replays; it never re-runs. No process, no supervisor.
        match = arena.runner.matches["dd52a57a6f49"]
        assert all(run.process is None for run in match.runs.values())

    def test_create_persists_a_secret_free_spec_for_the_next_boot(
        self, tmp_path: Path
    ):
        spec = MatchSpec.from_json(
            {
                "name": "kvk",
                "dataset": "terminal-bench-2.1",
                "tasks": ["fix-git"],
                "contestants": [
                    {
                        "name": "Stella",
                        "agent": "stella",
                        "engine": {"api": "openrouter", "model": "z-ai/glm-5.2"},
                        "env": "OPENROUTER_API_KEY=sk-secret-value",
                    },
                    {
                        "name": "Claude Code",
                        "agent": "claude-code",
                        "engine": {"api": "anthropic", "model": "claude-sonnet-5"},
                    },
                ],
            }
        )
        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path)
        match = runner.create(spec)
        raw = (match.workspace / "spec.json").read_text(encoding="utf-8")
        assert "sk-secret-value" not in raw, "credential values must never touch disk"
        restored = MatchSpec.from_json(json.loads(raw))
        assert restored.id == spec.id
        assert [c.name for c in restored.contestants] == ["Stella", "Claude Code"]
        assert restored.tasks == ("fix-git",)

    def test_junk_directories_do_not_break_the_boot(self, tmp_path: Path):
        from arenabench.server import ArenaServer

        workspace = self._workspace_with_fixture(tmp_path)
        (workspace / "matches" / "no-jobs-here").mkdir()
        (workspace / "matches" / "empty-jobs" / "jobs").mkdir(parents=True)
        arena = ArenaServer(workspace)
        listed = [m["id"] for m in arena.list_matches()["matches"]]
        assert listed == ["dd52a57a6f49"]


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


class TestSutPinTemplates:
    """The SUT pin as a file: a committed match must run the Stella it names.

    Before #2082 neither TOML path carried `sut_ref` at all — a committed
    template always ran `main` no matter what the match it was downloaded
    from had pinned, and the GUI's "download this match" silently dropped
    the pin. Every test here is about the pin surviving the file.
    """

    def _two_pin_spec(self) -> MatchSpec:
        return match_from_toml(
            {
                "match": {
                    "name": "twins",
                    "dataset": "terminal-bench-2.1",
                    "tasks": ["fix-git"],
                    "sut_ref": "branch-a",
                },
                "contestant": [
                    {
                        "id": "champ",
                        "name": "champion",
                        "agent": "stella",
                        "engine": {"api": "openrouter", "model": "z-ai/glm-5.2"},
                    },
                    {
                        "id": "chall",
                        "name": "challenger",
                        "agent": "stella",
                        "sut_ref": "branch-b",
                        "engine": {"api": "openrouter", "model": "z-ai/glm-5.2"},
                    },
                ],
            }
        )

    def test_a_match_level_pin_reaches_the_spec(self):
        spec = self._two_pin_spec()
        assert spec.sut_ref == "branch-a"

    def test_a_seat_override_wins_and_the_rest_inherit(self):
        spec = self._two_pin_spec()
        assert spec.contestants[0].sut_ref is None
        assert spec.contestants[1].sut_ref == "branch-b"
        assert spec.sut_ref_for(spec.contestants[0]) == "branch-a"
        assert spec.sut_ref_for(spec.contestants[1]) == "branch-b"

    def test_a_template_without_the_key_still_means_main(self):
        """Absent must keep meaning `main`, not the opt-out: every template
        committed before the key existed asked for the default branch."""
        spec = match_from_toml(
            {
                "match": {"dataset": "terminal-bench-2.1"},
                "contestant": [
                    {"agent": "stella", "engine": {"model": "z-ai/glm-5.2"}}
                ],
            }
        )
        assert spec.sut_ref == "main"

    def test_the_pin_round_trips_byte_stably(self):
        """dump -> load -> dump is the identity, pins included."""
        import tomllib

        from arenabench.config import dump_match

        text = dump_match(self._two_pin_spec())
        again = match_from_toml(tomllib.loads(text))
        assert again.sut_ref == "branch-a"
        assert again.contestants[1].sut_ref == "branch-b"
        assert dump_match(again) == text

    def test_a_downloaded_template_no_longer_drops_the_pin(self):
        """The GUI's "download this match" renders through dump_match; the
        pin must be in the bytes, unconditionally."""
        from arenabench.config import dump_match

        pinned = MatchSpec.from_json(
            {
                "dataset": "terminal-bench-2.1",
                "sut_ref": "0123456789abcdef0123456789abcdef01234567",
                "contestants": [
                    {"name": "s", "agent": "stella", "engine": {"model": "m"}}
                ],
            }
        )
        assert 'sut_ref = "0123456789abcdef0123456789abcdef01234567"' in dump_match(
            pinned
        )

    def test_a_pin_on_a_non_stella_seat_is_refused_at_parse(self):
        from arenabench.config import MatchTemplateError

        with pytest.raises(MatchTemplateError) as caught:
            match_from_toml(
                {
                    "match": {"dataset": "terminal-bench-2.1"},
                    "contestant": [
                        {
                            "agent": "claude-code",
                            "sut_ref": "main",
                            "engine": {"api": "anthropic", "model": "m"},
                        }
                    ],
                }
            )
        assert any("only to a stella seat" in p for p in caught.value.problems)

    def test_a_pin_on_a_non_stella_seat_is_refused_at_validate(self):
        spec = MatchSpec.from_json(
            {
                "dataset": "terminal-bench-2.1",
                "contestants": [
                    {
                        "name": "cc",
                        "agent": "claude-code",
                        "sut_ref": "main",
                        "engine": {"api": "anthropic", "model": "m"},
                    }
                ],
            }
        )
        assert any("only to a stella seat" in p for p in spec.validate())

    def test_the_seat_pin_survives_the_json_spec_record(self):
        """`spec.json` round-trips through redacted()/from_json on restore;
        losing the pin there would strand history without its SUT."""
        spec = self._two_pin_spec()
        again = MatchSpec.from_json(spec.to_json())
        assert again.contestants[1].sut_ref == "branch-b"
        assert again.sut_ref == "branch-a"
