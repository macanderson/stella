# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The one-click head-to-heads, and the three failures they are built against.

Each class here pins a property that, when it broke, produced a *complete run
with plausible numbers* rather than an error — the failure mode this whole
tool exists to refuse. A preset whose arms quietly differ, a seat that
authenticates nowhere, an effort setting that never reached the wire: all
three score as an agent that cannot code.
"""

from __future__ import annotations

import json
import time
from pathlib import Path

import pytest

from arenabench import harbor
from arenabench.agents import dead_seat_reason, resolve_agent
from arenabench.claude_oauth import CLAUDE_OAUTH_ENV, apply_local_claude_login
from arenabench.model import Contestant, MatchSpec
from arenabench.presets import PANEL_TASKS, preset_listing
from arenabench.registry import DEFAULT_REGISTRY
from arenabench.runner import ContestantRun, MatchRunner


def _by_agent(preset: dict, agent: str) -> dict:
    (seat,) = [c for c in preset["match"]["contestants"] if c["agent"] == agent]
    return seat


class TestPresetParity:
    """The worker must be the constant; only the architecture may differ.

    A preset whose Stella arm runs a different model, effort or reasoning
    posture than its Claude Code arm is not a head-to-head — it is two
    experiments sharing a scoreboard, and the delta it reports belongs to
    whichever difference nobody noticed.
    """

    def test_every_preset_matches_worker_model_effort_and_reasoning(self):
        for preset in preset_listing():
            cc = _by_agent(preset, "claude-code")["engine"]
            stella = _by_agent(preset, "stella")["engine"]
            assert stella["effort"] == cc["effort"], preset["key"]
            assert stella["reasoning"] == cc["reasoning"], preset["key"]
            # Same model, two routes: first-party for Claude Code, OpenRouter
            # for Stella, whose catalog publishes it under a vendor segment.
            assert stella["model"] == f"anthropic/{cc['model']}", preset["key"]

    def test_the_three_advertised_models_are_all_covered(self):
        workers = {
            _by_agent(preset, "claude-code")["engine"]["model"]
            for preset in preset_listing()
        }
        assert workers == {"claude-sonnet-5", "claude-opus-5", "claude-fable-5"}

    def test_every_preset_runs_the_same_ten_task_panel(self):
        """Comparable to each other, not merely to themselves."""
        for preset in preset_listing():
            assert tuple(preset["match"]["tasks"]) == PANEL_TASKS, preset["key"]
            assert len(PANEL_TASKS) == 10


class TestVerifierIndependence:
    """The judge is always frontier-tier, and never the worker.

    Two properties, two different failures. A verifier that *is* the worker
    cannot catch the worker's blind spots — it shares them, so the pipeline's
    independence is nominal. A verifier below the frontier tier rubber-stamps:
    it lacks the capability to tell a good answer from a confident one, so it
    approves both, and the verdict measures the judge rather than the work.

    Note what is deliberately *not* asserted: that the judge outranks its
    worker. Opus 5 and Fable 5 are frontier peers, so the Fable worker is
    judged by Opus and vice versa — peer-level cross-model independence, which
    is the strongest posture available when the worker is already at the top
    of the tier.
    """

    #: Models strong enough to sit in judgement. A judge outside this set is
    #: cheaper, and cheapness in a verifier buys false approvals.
    FRONTIER_JUDGES = {"claude-opus-5", "claude-fable-5"}

    def test_no_preset_verifies_a_worker_with_itself(self):
        for preset in preset_listing():
            stella = _by_agent(preset, "stella")["engine"]
            verifier = stella["roles"]["verifier"]["model"]
            assert verifier != stella["model"], (
                f"{preset['key']}: a verifier that is the worker shares its "
                "blind spots and cannot be independent of them"
            )

    def test_every_verifier_is_drawn_from_the_frontier_judge_pool(self):
        for preset in preset_listing():
            stella = _by_agent(preset, "stella")["engine"]
            verifier = stella["roles"]["verifier"]["model"].rsplit("/", 1)[-1]
            assert verifier in self.FRONTIER_JUDGES, (
                f"{preset['key']}: {verifier} is below the frontier tier; a "
                "cheap judge approves confident answers as readily as correct "
                "ones"
            )

    def test_the_triage_role_is_the_only_cheap_one(self):
        """Cheapness is fine where nothing is being judged.

        Triage classifies the request and never touches the workspace, so a
        small model there costs accuracy nowhere that matters — which is
        precisely the argument that does *not* transfer to the verifier.
        """
        for preset in preset_listing():
            roles = _by_agent(preset, "stella")["engine"]["roles"]
            assert roles["triage"]["model"] == "anthropic/claude-haiku-4.5"
            assert roles["triage"]["effort"] == "low"

    def test_a_fable_worker_is_judged_by_opus_and_everyone_else_by_fable(self):
        judged = {
            _by_agent(p, "claude-code")["engine"]["model"]: _by_agent(p, "stella")[
                "engine"
            ]["roles"]["verifier"]["model"]
            for p in preset_listing()
        }
        assert judged == {
            "claude-sonnet-5": "anthropic/claude-fable-5",
            "claude-opus-5": "anthropic/claude-fable-5",
            "claude-fable-5": "anthropic/claude-opus-5",
        }


class TestPresetCredentialDeclarations:
    """Names only, never values — and the right names per seat."""

    def test_the_claude_code_seat_declares_only_the_subscription_token(self):
        for preset in preset_listing():
            seat = _by_agent(preset, "claude-code")
            assert seat["required_env"] == [CLAUDE_OAUTH_ENV], preset["key"]

    def test_the_stella_seat_declares_only_its_metered_key(self):
        for preset in preset_listing():
            assert _by_agent(preset, "stella")["required_env"] == [
                "OPENROUTER_API_KEY"
            ], preset["key"]

    def test_a_preset_never_carries_a_credential_value(self):
        """`redacted()` discloses env *keys*; a preset must have none at all."""
        for preset in preset_listing():
            for seat in preset["match"]["contestants"]:
                assert seat["env_keys"] == [], preset["key"]

    def test_a_preset_is_a_launchable_spec(self):
        """Round-trips through the same parser `POST /api/matches` uses."""
        for preset in preset_listing():
            spec = MatchSpec.from_json(preset["match"])
            assert len(spec.contestants) == 2
            assert spec.dataset == "terminal-bench-2.1"


class TestDeadSeatRefusal:
    """A credentialled seat its CLI cannot read from is refused, not run.

    Measured 2026-08-06: a Claude Code seat on ``api: openrouter`` with no
    ``base_url`` passed the credential check on a valid ``OPENROUTER_API_KEY``,
    then booted with ``apiKeySource: none`` and died at turn one of all
    eleven trials — every one scoring a 0.0 indistinguishable from a real loss.
    """

    def _seat(self, **raw: object) -> Contestant:
        return Contestant.from_json({"name": "cc", "agent": "claude-code", **raw})

    def test_an_openrouter_claude_code_seat_with_no_base_url_is_refused(self):
        seat = self._seat(
            engine={"api": "openrouter", "model": "anthropic/claude-sonnet-5"},
            env="OPENROUTER_API_KEY=sk-or-real",
        )
        reason = dead_seat_reason(seat)
        assert reason is not None
        assert "base_url" in reason

    def test_a_base_url_makes_the_same_seat_launchable(self):
        seat = self._seat(
            engine={
                "api": "zai",
                "model": "glm-5.2",
                "base_url": "https://api.z.ai/api/anthropic",
            },
            env="ZAI_API_KEY=zk",
        )
        assert dead_seat_reason(seat) is None

    def test_a_subscription_token_makes_the_seat_launchable(self):
        seat = self._seat(
            engine={"api": "anthropic", "model": "claude-sonnet-5"},
            env=f"{CLAUDE_OAUTH_ENV}=oat-live",
        )
        assert dead_seat_reason(seat) is None

    def test_an_agent_with_no_credential_variables_of_its_own_is_never_refused(self):
        """The check only speaks about agents whose variables it knows."""
        seat = Contestant.from_json(
            {"name": "a", "agent": "aider", "engine": {"api": "openai", "model": "m"}}
        )
        assert dead_seat_reason(seat) is None


class TestEffortReachesTheWire:
    """`effort` on a built-in seat must become the variable its CLI reads.

    Declaring the knob honoured while forwarding nothing is how a match TOML's
    ``effort = "medium"`` ran the Claude Code arm at its default: two runs that
    were secretly identical, which the honours system exists to prevent.
    """

    def _env(self, seat: Contestant, tmp_path: Path) -> tuple[dict, list[str]]:
        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path)
        run = ContestantRun(
            contestant=seat,
            job_name="job",
            job_dir=tmp_path / "job",
            log_path=tmp_path / "job.log",
        )
        return runner._agent_environment(seat, run), run.notes

    def _seat(self, env: str = "", **engine: object) -> Contestant:
        return Contestant.from_json(
            {"name": "cc", "agent": "claude-code", "engine": engine, "env": env}
        )

    def test_claude_code_declares_the_variable_it_reads_effort_from(self):
        spec = resolve_agent("claude-code")
        assert spec.effort_env == "CLAUDE_CODE_EFFORT_LEVEL"
        assert "effort" in spec.honours

    def test_the_seats_effort_is_forwarded_and_recorded(self, tmp_path: Path):
        seat = self._seat(api="anthropic", model="claude-fable-5", effort="medium")
        env, notes = self._env(seat, tmp_path)
        assert env["CLAUDE_CODE_EFFORT_LEVEL"] == "medium"
        assert any("medium" in note for note in notes), (
            "a forwarded setting must be visible on the seat, not silent"
        )

    def test_a_seat_cannot_smuggle_the_effort_variable_through_its_env(self):
        """Why the forwarding needs no "unless the seat set it" guard.

        A seat's ``env`` is a credentials-only channel: `screen_env` refuses
        every name that is not credential-shaped and *reports* it, so the
        engine's ``effort`` is the only place this setting can come from.
        That is what keeps a seat's declared posture and the wire in
        agreement — there is no second, quieter way to set it.
        """
        seat = self._seat(
            env="CLAUDE_CODE_EFFORT_LEVEL=max",
            api="anthropic",
            model="claude-fable-5",
            effort="medium",
        )
        assert "CLAUDE_CODE_EFFORT_LEVEL" not in seat.env
        assert "CLAUDE_CODE_EFFORT_LEVEL" in seat.ignored_env

    def test_every_preset_pins_the_same_effort_on_both_arms(self):
        """The parity the forwarding exists to make real."""
        for preset in preset_listing():
            efforts = {
                seat["engine"]["effort"] for seat in preset["match"]["contestants"]
            }
            assert len(efforts) == 1, preset["key"]


class TestPerDatasetHarbor:
    """One server, two Harbors — because one machine runs both lanes.

    Terminal-Bench's audited constant is 0.6.1 and Frontier-Bench refuses
    anything below 0.20.0. A single global override cannot serve both, and the
    failure of getting it wrong is silent: 0.6.1 drops Frontier-Bench's
    ``environment_mode`` and grades against the wrong container topology.
    """

    def test_the_override_name_is_derived_from_the_dataset_key(self):
        assert (
            harbor.dataset_harbor_env("frontier-bench")
            == "ARENABENCH_HARBOR_FRONTIER_BENCH"
        )
        assert (
            harbor.dataset_harbor_env("terminal-bench-2.1")
            == "ARENABENCH_HARBOR_TERMINAL_BENCH_2_1"
        )

    def test_a_dataset_override_wins_over_the_global_one(
        self, monkeypatch, tmp_path: Path
    ):
        lane = tmp_path / "lane-harbor"
        lane.write_text("#!/bin/sh\n")
        lane.chmod(0o755)
        globally = tmp_path / "global-harbor"
        globally.write_text("#!/bin/sh\n")
        globally.chmod(0o755)
        monkeypatch.setenv("ARENABENCH_HARBOR", str(globally))
        monkeypatch.setenv("ARENABENCH_HARBOR_FRONTIER_BENCH", str(lane))
        assert harbor.harbor_bin("frontier-bench") == str(lane)
        assert harbor.harbor_bin("terminal-bench-2.1") == str(globally)
        assert harbor.harbor_bin() == str(globally)

    def test_a_dataset_override_pointing_at_nothing_says_which_variable(
        self, monkeypatch, tmp_path: Path
    ):
        monkeypatch.setenv("ARENABENCH_HARBOR_FRONTIER_BENCH", str(tmp_path / "nope"))
        with pytest.raises(harbor.HarborUnavailableError, match="FRONTIER_BENCH"):
            harbor.harbor_bin("frontier-bench")

    def test_the_version_floor_is_checked_against_the_resolved_binary(
        self, monkeypatch
    ):
        """Checking a *different* Harbor's version proves nothing about this run."""
        monkeypatch.setattr(harbor, "harbor_version", lambda binary=None: "0.6.1")
        with pytest.raises(harbor.HarborTooOldError, match="0.20.0"):
            harbor.require_for_dataset("0.20.0", "Frontier Bench", binary="/lane/harbor")

    def test_frontier_bench_declares_the_floor_that_makes_it_gradeable(self):
        dataset = DEFAULT_REGISTRY.get("frontier-bench")
        assert dataset is not None
        assert dataset.min_harbor == "0.20.0"


class TestExportedDatasetIsVisibleAndRunnable:
    """The picker and the launcher must agree about where an export lives.

    They did not. ``local_run_path`` resolved ``<export root>/<key>/<package>``
    while ``task_roots`` looked only at an explicitly-registered directory and
    Harbor's own cache — so a dataset exported to the default root ran
    perfectly from ``--path`` and showed **zero tasks** in the picker. The
    benchmark looked unavailable while being fully materialised on disk, which
    is the shape of bug that gets a working lane declared broken.
    """

    def _export(self, root: Path, key: str, package: str, names: list[str]) -> None:
        for name in names:
            task_dir = root / key / package / name
            task_dir.mkdir(parents=True)
            (task_dir / "task.toml").write_text(
                f'[task]\nname = "{package}/{name}"\ndescription = "t"\n'
            )

    def test_a_default_root_export_is_both_enumerable_and_runnable(
        self, monkeypatch, tmp_path: Path
    ):
        monkeypatch.setenv("ARENABENCH_DATASETS", str(tmp_path))
        # A cache that holds nothing, so only the export can answer.
        monkeypatch.setenv("HARBOR_CACHE_DIR", str(tmp_path / "empty-cache"))
        self._export(tmp_path, "frontier-bench", "frontier-bench", ["alpha", "beta"])

        names = [task.name for task in DEFAULT_REGISTRY.tasks("frontier-bench")]
        assert names == ["alpha", "beta"], (
            "an export the launcher can run must also be one the picker can show"
        )
        assert DEFAULT_REGISTRY.local_run_path("frontier-bench") is not None

    def test_both_answers_come_from_one_definition(self, monkeypatch, tmp_path: Path):
        """The property that keeps them from drifting apart again."""
        monkeypatch.setenv("ARENABENCH_DATASETS", str(tmp_path))
        dataset = DEFAULT_REGISTRY.get("frontier-bench")
        assert dataset is not None
        candidates = DEFAULT_REGISTRY.export_candidates(dataset)
        assert tmp_path / "frontier-bench" in candidates
        roots = DEFAULT_REGISTRY.task_roots(dataset)
        for candidate in candidates:
            assert candidate / dataset.package_dir in roots


class TestLocalClaudeLogin:
    """The Claude Code seat's token is read live, and only where declared.

    Claude Code rotates its OAuth credential whenever any local session
    refreshes, and rotation *revokes* the previous access token — measured
    2026-08-06, a freshly-pulled token died at turn one minutes later. Any
    stored copy is therefore stale by construction, so the only survivable
    seat reads the live store at launch.
    """

    def _spec(self, *, required: list[str], env: dict | None = None) -> MatchSpec:
        return MatchSpec.from_json(
            {
                "name": "m",
                "dataset": "terminal-bench-2.1",
                "tasks": ["fix-git"],
                "contestants": [
                    {
                        "name": "cc",
                        "agent": "claude-code",
                        "engine": {"api": "anthropic", "model": "claude-fable-5"},
                        "env": {"required": required, **(env or {})},
                    }
                ],
            }
        )

    def _login(self, monkeypatch, token: str | None, *, expires_in_s: float = 3600):
        def fake() -> str | None:
            return token

        monkeypatch.setattr("arenabench.claude_oauth.local_claude_token", fake)

    def test_a_declared_seat_is_filled_from_the_local_login(self, monkeypatch):
        self._login(monkeypatch, "oat-live")
        spec, notes = apply_local_claude_login(
            self._spec(required=[CLAUDE_OAUTH_ENV])
        )
        assert spec.contestants[0].env[CLAUDE_OAUTH_ENV] == "oat-live"
        assert notes and "never persisted" in notes[0]

    def test_a_seat_that_declared_nothing_is_never_switched_onto_the_plan(
        self, monkeypatch
    ):
        """A seat pointed elsewhere must not silently join the operator's plan."""
        self._login(monkeypatch, "oat-live")
        spec, notes = apply_local_claude_login(self._spec(required=[]))
        assert CLAUDE_OAUTH_ENV not in spec.contestants[0].env
        assert notes == []

    def test_an_operators_own_token_is_never_overwritten(self, monkeypatch):
        self._login(monkeypatch, "oat-from-keychain")
        spec, _ = apply_local_claude_login(
            self._spec(
                required=[CLAUDE_OAUTH_ENV], env={CLAUDE_OAUTH_ENV: "oat-explicit"}
            )
        )
        assert spec.contestants[0].env[CLAUDE_OAUTH_ENV] == "oat-explicit"

    def test_no_local_login_leaves_the_spec_untouched(self, monkeypatch):
        self._login(monkeypatch, None)
        original = self._spec(required=[CLAUDE_OAUTH_ENV])
        spec, notes = apply_local_claude_login(original)
        assert spec is original and notes == []

    def test_an_expired_token_is_not_handed_to_a_seat(self, monkeypatch, tmp_path: Path):
        """Handing over an expired token reproduces the dead arm, one step later."""
        from arenabench import claude_oauth

        home = tmp_path / "claude"
        home.mkdir()
        (home / ".credentials.json").write_text(
            json.dumps(
                {
                    "claudeAiOauth": {
                        "accessToken": "oat-stale",
                        "expiresAt": int((time.time() - 60) * 1000),
                    }
                }
            )
        )
        monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(home))
        monkeypatch.setattr(claude_oauth.sys, "platform", "linux")
        assert claude_oauth.local_claude_token() is None

    def test_a_live_token_is_read_from_the_credentials_file(
        self, monkeypatch, tmp_path: Path
    ):
        from arenabench import claude_oauth

        home = tmp_path / "claude"
        home.mkdir()
        (home / ".credentials.json").write_text(
            json.dumps(
                {
                    "claudeAiOauth": {
                        "accessToken": "oat-live",
                        "expiresAt": int((time.time() + 3600) * 1000),
                    }
                }
            )
        )
        monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(home))
        monkeypatch.setattr(claude_oauth.sys, "platform", "linux")
        assert claude_oauth.local_claude_token() == "oat-live"

    def test_a_missing_store_is_not_an_error(self, monkeypatch, tmp_path: Path):
        from arenabench import claude_oauth

        monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path / "absent"))
        monkeypatch.setattr(claude_oauth.sys, "platform", "linux")
        assert claude_oauth.local_claude_token() is None
