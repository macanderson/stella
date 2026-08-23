# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Getting a seat from a spec onto a running container.

`test_arena.py` covers the vocabulary a seat is described in; this file covers
what happens once a match actually launches one: a provider credential aliased
onto the exact environment variable an agent reads, a trial filmed by the
recorder supervisor without blocking it, a seeded draw of tasks, Claude Code's
subscription-token path, resolving tasks from an offline dataset export, and
the launch record written beside each job directory so pricing can tell a
routed seat apart from a gateway one after the fact.

No Docker, no network, no model key: every fixture is synthetic, and Harbor
itself is stubbed at the `arenabench.harbor` seam rather than exercised for
real.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from arenabench.agents import launch_model, missing_credentials, resolve_agent
from arenabench.config import match_from_toml, required_env
from arenabench.model import Contestant, Engine, MatchSpec
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

    def _stub_trial(self, tmp_path: Path, payload: bytes) -> Path:
        trial_dir = tmp_path / "job" / "t__1"
        (trial_dir / "arena").mkdir(parents=True)
        (trial_dir / "arena" / "recording.mp4").write_bytes(payload)
        return trial_dir

    def _stop(self, supervisor, trial_dir: Path, monkeypatch: pytest.MonkeyPatch):
        from arenabench.recorder import _Recording

        # `docker stop` is the only shell-out on this path and it is not what
        # is under test; the file on disk is.
        monkeypatch.setattr(
            "arenabench.recorder.subprocess.run", lambda *a, **k: None
        )
        supervisor._stop_container(_Recording("c", trial_dir, 0.0), trial_dir)

    def test_a_stub_too_small_to_play_is_not_reported_as_a_recording(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, caplog
    ):
        """An encoder killed before it flushed a fragment leaves 28 bytes of
        `ftyp` and nothing else. A bare `size > 0` check calls that a success
        and prints "recorded ... (0.0 MB)", so an operator has no reason to
        look — which is how a head-to-head finishes with a full gallery of
        unplayable stubs. Measured on a 32-vCPU rig, where x264 sized its
        thread pool from the host's cores and the container was OOM-killed.
        """
        import logging

        supervisor = self._supervisor(tmp_path)
        trial_dir = self._stub_trial(tmp_path, b"\x00" * 28)

        with caplog.at_level(logging.INFO, logger="arenabench.recorder"):
            self._stop(supervisor, trial_dir, monkeypatch)

        assert not [r for r in caplog.records if r.levelno == logging.INFO], (
            "a 28-byte stub was reported as a successful recording"
        )
        warned = [r.getMessage() for r in caplog.records if r.levelno >= logging.WARNING]
        assert warned, "a stub too small to play produced no warning at all"
        # The operator needs the cause, not just the symptom.
        assert "28 bytes" in warned[0]
        assert "memory limit" in warned[0]

    def test_a_real_recording_is_still_reported_as_one(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, caplog
    ):
        """The guard above must not swallow the ordinary case."""
        import logging

        from arenabench.recorder import MIN_PLAYABLE_BYTES

        supervisor = self._supervisor(tmp_path)
        trial_dir = self._stub_trial(tmp_path, b"\x00" * (MIN_PLAYABLE_BYTES + 1))

        with caplog.at_level(logging.INFO, logger="arenabench.recorder"):
            self._stop(supervisor, trial_dir, monkeypatch)

        assert [r for r in caplog.records if r.levelno == logging.INFO]
        assert not [r for r in caplog.records if r.levelno >= logging.WARNING]


class TestRecorderEncoder:
    """The encoder's footprint must be a property of the recorder, not of the
    host it lands on."""

    def _script(self) -> str:
        from pathlib import Path as _Path

        import arenabench

        return (
            _Path(arenabench.__file__).resolve().parent.parent / "recorder" / "record.sh"
        ).read_text()

    def test_the_encoder_thread_pool_is_pinned(self):
        """x264 sizes its pool from the CPUs it can *see* — the host's, since
        `--cpus` is a scheduling quota and not a visible core count. On a
        32-vCPU host it chose 28 threads, and the per-thread frame buffers blew
        through the recorder's 512 MB cgroup within a second of ffmpeg
        starting. Unpinned, this file only works on small machines, and a
        benchmark rig is not one.
        """
        script = self._script()
        assert "-threads" in script, "ffmpeg's thread pool is unpinned"
        assert "ARENA_THREADS" in script, "the pool size is not overridable"


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
    that sentence are required, and each has its own failure: no base URL
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
        # Explicitly unpinned: this test is about the argv shape, and a
        # pinned seat with no staged build now refuses the launch (#2098).
        spec = MatchSpec.from_json({
            "dataset": "terminal-bench-2.1", "tasks": ["alpha"], "sut_ref": "",
            "contestants": [{"name": "s", "agent": "stella",
                             "engine": {"api": "openrouter", "model": "x/y"}}],
        })
        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / "ws")
        match = runner.create(spec)
        runner._launch(match, spec.contestants[0], runner.resolve_harbor(match))
        return seen["command"]

    def test_an_export_is_filtered_by_bare_task_name(
        self, tmp_path: Path, monkeypatch, stella_adapter
    ):
        """Names are namespaced by the registry, not by the task. An export on
        disk is just directories and answers to the bare name — sending the
        qualified form matches nothing and ends the match at once."""
        command = self._launched_command(tmp_path, monkeypatch, export=True)
        assert "--path" in command and "--dataset" not in command
        assert command[command.index("--include-task-name") + 1] == "alpha"

    def test_a_registry_ref_is_filtered_by_qualified_task_name(
        self, tmp_path: Path, monkeypatch, stella_adapter
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
        run = runner._launch(match, spec.contestants[0], runner.resolve_harbor(match))

        record = json.loads(seat_manifest_path(run.job_dir).read_text(encoding="utf-8"))
        # The two spellings genuinely diverge for this seat — that divergence
        # is why the record has to exist at all.
        assert record["launch_model"] == "glm-5.2"
        assert record["qualified_model"] == "zai/glm-5.2"
        assert record["api"] == "zai"
