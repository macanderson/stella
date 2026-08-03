"""The two frozen benchmark arms, and how a trial reports which one it ran.

Companion to `stella_harbor.posture`, split from `test_adapter.py` for the same
reason the module was split from the package root: the file-size gate treats a
separable concern as its own module rather than as more length on an existing
one.

The subject is #1007 — the Terminal-Bench posture pinned one model for every
role, Stella's authored-witness tier requires an author independent of the
worker, and so every scored run measured Stella with that tier structurally off
while saying so only in a log line. Three layers are covered here: what the
posture declares, what the event stream observes, and what reaches trial
metadata.
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from types import SimpleNamespace

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from harbor.models.agent.context import AgentContext  # noqa: E402

from stella_harbor import (  # noqa: E402 - after importorskip by design
    _ASSURANCE_TIERS_VERSION,
    _ENGINE_CONFIG_ENV,
    _WITNESS_AUTHOR_ENV,
    StellaAgent,
    _benchmark_assurance_tiers,
    _benchmark_engine_posture,
    _stream_to_envelope,
)


def _bare_agent() -> StellaAgent:
    """A StellaAgent instance bypassing the Harbor base ``__init__``.

    Defined here rather than imported from `test_adapter`: it is a one-line
    factory, and a cross-test-module import would couple two files for less
    than it costs to restate.
    """
    return StellaAgent.__new__(StellaAgent)


class TestBenchmarkPosture:
    """What each arm pins, and what it refuses to pin."""

    def test_control_arm_still_disables_the_authored_witness(self) -> None:
        """The control arm's cost, asserted rather than discovered on a trial.

        Stella refuses to let a worker author the test that verifies it, so the
        witness needs an author resolving to a different model. With no
        ``witness_author`` every role inherits one ``default_model``, which is
        exactly the condition under which no such author exists — so every
        number this arm produces is measured with the witness off (#973).

        Still guarded, and still for the same reason: the arm must never change
        as a side effect of an unrelated edit. What #1007 changed is that the
        other arm now *exists* and is reachable by asking for it explicitly —
        see the treatment-arm test below.
        """
        model = "openrouter/z-ai/glm-5.1"
        posture, _normalized, _digest = _benchmark_engine_posture(model)

        roles = posture["agents"]
        assert "judge" in roles, "the judge role must be stated, not implied"
        resolved = {
            role: config.get("model", posture["default_model"])
            for role, config in roles.items()
        }
        assert resolved["judge"] == resolved["worker"] == model
        assert "pipeline_judge_model" not in posture
        assert posture["allowed_models"] == [model], (
            "one allowed model is what forbids a distinct witness author; "
            "widening it silently would be a measurement change, not a fix"
        )

    def test_witness_arm_pins_an_independent_author_and_moves_the_digest(
        self,
    ) -> None:
        """The treatment arm: a second frozen model, disclosed by the hash.

        The reproducibility argument that motivated one inherited model is
        untouched — nothing is auto-selected, both models are pinned, and the
        posture is still exactly one hash. What changes is that the hash now
        distinguishes *which* frozen configuration ran, so a witness-on arm and
        a witness-off arm cannot be mistaken for each other (#1007).
        """
        worker = "openrouter/z-ai/glm-5.1"
        author = "openrouter/deepseek/deepseek-v4-pro"
        control, _control_json, control_digest = _benchmark_engine_posture(worker)
        arm, arm_json, arm_digest = _benchmark_engine_posture(
            worker, witness_author=author
        )

        assert arm["default_model"] == worker
        assert arm["pipeline_judge_model"] == author, (
            "the judge role is what the authored-witness tier resolves its "
            "independent author from"
        )
        assert arm["allowed_models"] == [worker, author]
        # Auto-selection stays off in both arms: the second model is pinned,
        # not chosen at runtime from a widened vocabulary.
        assert arm["auto_mode"] == control["auto_mode"] == "off"
        assert all(
            "model" not in role and "provider" not in role
            for role in arm["agents"].values()
        ), "routing stays in the flat root keys, never per-agent overrides"
        assert json.loads(arm_json) == arm
        assert arm_digest != control_digest, (
            "the arm must be visible in the registered SUT hash, never only in the logs"
        )

    def test_witness_arm_root_keys_stay_inside_the_launcher_vocabulary(
        self,
    ) -> None:
        """The strict launcher seam fails *closed* on an unknown root key.

        `config::trusted_engine_config_shape_is_strict` refuses any posture with
        a root key outside `settings::ENGINE_ROOT_FIELDS`, so a posture that
        grew a descriptive field would not be a mislabelled run — it would be a
        refused one. Mirrored here so the Python side fails in unit tests rather
        than on the first container.
        """
        engine_root_fields = {
            "default_model",
            "pipeline_judge_model",
            "pipeline_worker_model",
            "pipeline_triage_model",
            "allowed_models",
            "auto_mode",
            "effort_auto",
            "reasoning_auto",
            "headless_scope_bypass",
            "agents",
        }
        for witness_author in (None, "openrouter/deepseek/deepseek-v4-pro"):
            posture, _json_text, _digest = _benchmark_engine_posture(
                "openrouter/z-ai/glm-5.1", witness_author=witness_author
            )
            unknown = set(posture) - engine_root_fields
            assert not unknown, (
                f"the trusted launcher seam would refuse this posture: {unknown}"
            )

    def test_witness_author_must_be_independent_and_same_provider(self) -> None:
        """Both refusals exist so an arm cannot degrade into the other silently."""
        worker = "openrouter/z-ai/glm-5.1"

        with pytest.raises(ValueError, match="must differ from the worker model"):
            _benchmark_engine_posture(worker, witness_author=worker)

        # One credential reaches the container, resolved from the worker's
        # provider. A second provider would authenticate against nothing.
        with pytest.raises(ValueError, match="must share the worker's provider"):
            _benchmark_engine_posture(worker, witness_author="anthropic/claude-fable-5")

        with pytest.raises(ValueError, match="provider/model spec"):
            _benchmark_engine_posture(worker, witness_author="not-a-spec")

    def test_assurance_tiers_declare_the_arm_and_are_hashed(self) -> None:
        """A scored run declares a disabled tier instead of only logging it."""
        worker = "openrouter/z-ai/glm-5.1"
        author = "openrouter/deepseek/deepseek-v4-pro"

        off, off_json, off_digest = _benchmark_assurance_tiers(worker)
        assert off["version"] == _ASSURANCE_TIERS_VERSION
        assert off["arm"] == "witness-off"
        assert off["witness_author_model"] is None
        assert off["tiers"]["authored_witness"] == "off"
        assert off["tiers"]["model_judge"] == "on-same-model-as-worker"
        assert off["tiers"]["deterministic_verify"] == "on"
        assert (
            "no author independent of the worker"
            in (off["authored_witness_off_reason"])
        )
        assert json.loads(off_json) == off

        on, _on_json, on_digest = _benchmark_assurance_tiers(
            worker, witness_author=author
        )
        assert on["arm"] == "witness-on"
        assert on["witness_author_model"] == author
        assert on["tiers"]["authored_witness"] == "on"
        assert on["tiers"]["model_judge"] == "on-independent-of-worker"
        assert on["authored_witness_off_reason"] is None
        assert on_digest != off_digest


class TestWorkerEffortAndTriageArms:
    """The two selectors a tuning run varies, and what they refuse."""

    _MODEL = "openrouter/anthropic/claude-sonnet-5"

    def test_unset_selectors_reproduce_the_frozen_posture(self) -> None:
        """Carrying this code must not, by itself, change any recorded digest.

        The selectors exist so a run can *ask* for a different arm. A tree that
        merely contains them has asked for nothing, so the posture it produces
        has to be byte-identical to the one produced before they existed —
        otherwise every hash in `bench/READINESS.md` silently stops describing
        the run it was recorded against.
        """
        explicit = _benchmark_engine_posture(
            self._MODEL, worker_effort="xhigh", triage_model=None
        )
        default = _benchmark_engine_posture(self._MODEL)
        assert default[1] == explicit[1]
        assert default[2] == explicit[2]
        assert default[0]["agents"]["worker"]["effort"] == "xhigh"
        assert "pipeline_triage_model" not in default[0]

    def test_worker_effort_moves_only_the_worker_and_the_digest(self) -> None:
        """`high` and `xhigh` are two arms, and the hash has to say which."""
        high, _high_json, high_digest = _benchmark_engine_posture(
            self._MODEL, worker_effort="high"
        )
        xhigh, _xhigh_json, xhigh_digest = _benchmark_engine_posture(
            self._MODEL, worker_effort="xhigh"
        )
        assert high["agents"]["worker"]["effort"] == "high"
        assert xhigh["agents"]["worker"]["effort"] == "xhigh"
        assert high_digest != xhigh_digest
        # Only the worker moved. `default` governs roles with no entry of
        # their own, and letting it track the worker would retune them as an
        # undeclared second variable inside a one-variable comparison.
        assert high["agents"]["default"]["effort"] == "xhigh"
        assert high["agents"]["judge"]["effort"] == "xhigh"
        assert high["agents"]["triage"] == xhigh["agents"]["triage"]

    def test_unrecognised_worker_effort_is_refused_not_defaulted(self) -> None:
        """A typo must not be silently promoted to the frozen default.

        Falling back would attribute the run to a tier nobody selected, and the
        digest would agree with the typo rather than with reality — the same
        failure mode as a treatment arm degrading into the control arm.
        """
        with pytest.raises(ValueError, match="worker effort"):
            _benchmark_engine_posture(self._MODEL, worker_effort="ultra")

    def test_triage_pin_lands_in_the_flat_key_and_widens_the_vocabulary(
        self,
    ) -> None:
        """A pinned triage author must also be an *allowed* model.

        `allowed_models` is a whitelist. A triage pin outside it is refused at
        resolve time, triage falls back to the worker, and the run bills the
        expensive model for the cheap role while the digest claims otherwise.
        """
        triage = "openrouter/anthropic/claude-haiku-4.5"
        posture, _normalized, digest = _benchmark_engine_posture(
            self._MODEL, triage_model=triage
        )
        assert posture["pipeline_triage_model"] == triage
        assert triage in posture["allowed_models"]
        assert self._MODEL in posture["allowed_models"]
        # Triage stays low/off regardless of who authors it: the role emits a
        # short classification, and raising it would change what Stella is
        # rather than what it was allowed to spend.
        assert posture["agents"]["triage"] == {"effort": "low", "reasoning": "off"}
        assert digest != _benchmark_engine_posture(self._MODEL)[2]

    def test_triage_pin_must_share_the_workers_provider(self) -> None:
        """One credential reaches the container, so a second provider is unusable."""
        with pytest.raises(ValueError, match="triage model must share"):
            _benchmark_engine_posture(
                self._MODEL, triage_model="anthropic/claude-haiku-4.5"
            )

    def test_all_three_roles_coexist_inside_the_launcher_vocabulary(self) -> None:
        """The full tuning posture must survive the fail-closed launcher seam.

        `config::trusted_engine_config_shape_is_strict` rejects any root key
        outside `ENGINE_ROOT_FIELDS`, so a posture that names a judge *and* a
        triage author is the shape most likely to trip it — and it refuses the
        run outright rather than dropping the unknown key.
        """
        posture, _normalized, _digest = _benchmark_engine_posture(
            self._MODEL,
            witness_author="openrouter/anthropic/claude-fable-5",
            worker_effort="high",
            triage_model="openrouter/anthropic/claude-haiku-4.5",
        )
        allowed_roots = {
            "default_model",
            "pipeline_judge_model",
            "pipeline_worker_model",
            "pipeline_triage_model",
            "allowed_models",
            "auto_mode",
            "effort_auto",
            "reasoning_auto",
            "headless_scope_bypass",
            "agents",
        }
        assert set(posture) <= allowed_roots
        assert set(posture["agents"]) <= {"default", "worker", "judge", "triage"}
        # Every pinned role is in the whitelist, or it cannot be resolved.
        for role_key in ("pipeline_judge_model", "pipeline_triage_model"):
            assert posture[role_key] in posture["allowed_models"]


class TestWitnessStreamObservation:
    """Folding `AgentEvent::Proof` into a field an analysis can read."""

    def test_stream_parser_reports_an_unavailable_witness_as_a_field(self) -> None:
        """`witness_authored: false` must be readable without a trajectory grep.

        This is the exact stream the #909 measured run produced on every trial:
        triage asked for a witness, the warrant required one, and the ladder
        could not author it because no model was independent of the worker.
        """
        reason = (
            "no author independent of the worker (judge and worker both "
            "resolved to `openrouter/z-ai/glm-5.1`)"
        )
        events = [
            {
                "type": "proof",
                "step": {"kind": "assurance", "witness": True, "judge": True},
            },
            {
                "type": "proof",
                "step": {"kind": "warrant", "required": True, "diff_lines": 42},
            },
            {
                "type": "proof",
                "step": {"kind": "witness_unavailable", "reason": reason},
            },
            {
                "type": "error",
                "message": f"continuing without an authored witness: {reason}",
                "retryable": True,
            },
            {"type": "complete", "status": "completed", "cost_usd": 0.5},
        ]
        envelope = _stream_to_envelope(
            "\n".join(json.dumps(event) for event in events),
            process_returned=True,
        )
        assert envelope is not None
        stream = envelope["_stella_stream"]
        assert stream["witness_authored"] is False
        assert stream["witness_authored_state"] == "unavailable"
        assert stream["witness_unavailable_count"] == 1
        assert stream["witness_unavailable_reasons"] == [reason]
        assert stream["witness_warranted_count"] == 1
        assert stream["assurance_witness_planned"] is True
        assert stream["proof_step_counts"]["witness_unavailable"] == 1

    def test_stream_parser_reports_an_authored_witness(self) -> None:
        events = [
            {
                "type": "proof",
                "step": {"kind": "warrant", "required": True, "diff_lines": 12},
            },
            {
                "type": "proof",
                "step": {
                    "kind": "witness_authored",
                    "path": "tests/test_fix.py",
                    "command": "pytest tests/test_fix.py",
                    "fingerprint": "a" * 64,
                },
            },
            {"type": "complete", "status": "completed", "cost_usd": 0.5},
        ]
        envelope = _stream_to_envelope(
            "\n".join(json.dumps(event) for event in events),
            process_returned=True,
        )
        assert envelope is not None
        assert envelope["_stella_stream"]["witness_authored"] is True
        assert envelope["_stella_stream"]["witness_authored_state"] == "authored"
        assert envelope["_stella_stream"]["witness_authored_count"] == 1

    def test_stream_parser_separates_no_witness_data_from_a_denied_witness(
        self,
    ) -> None:
        """Tri-state on purpose: silence is not the same claim as "unavailable"."""
        events = [{"type": "complete", "status": "completed", "cost_usd": 0.5}]
        envelope = _stream_to_envelope(json.dumps(events[0]), process_returned=True)
        assert envelope is not None
        assert envelope["_stella_stream"]["witness_authored"] is None
        assert envelope["_stella_stream"]["witness_authored_state"] == "not_reported"
        assert envelope["_stella_stream"]["proof_step_counts"] == {}


class TestWitnessArmEndToEnd:
    """Env knob -> posture -> container -> trial metadata."""

    def test_witness_arm_reaches_the_container_and_the_trial_metadata(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The treatment arm, end to end: env knob → posture → container → metadata.

        The interesting hop is the middle one. `_secure_exec_with_credential_fd`
        deliberately *recomputes* the posture from argv at the process boundary
        rather than trusting the caller, so before #1007 an arm expressed only
        in `run()` would have been rebuilt as the control posture and shipped —
        the container running witness-off while the trial recorded witness-on.
        """
        worker = "openrouter/z-ai/glm-5.1"
        author = "openrouter/deepseek/deepseek-v4-pro"
        monkeypatch.setenv("STELLA_BUDGET", "0.17")
        monkeypatch.setenv("OPENROUTER_API_KEY", "openrouter-test-secret")
        monkeypatch.setenv(_WITNESS_AUTHOR_ENV, author)

        events = [
            {
                "type": "proof",
                "step": {"kind": "warrant", "required": True, "diff_lines": 30},
            },
            {
                "type": "proof",
                "step": {
                    "kind": "witness_authored",
                    "path": "tests/test_fix.py",
                    "command": "pytest tests/test_fix.py",
                    "fingerprint": "f" * 64,
                },
            },
            {"type": "complete", "status": "completed", "cost_usd": 0.42},
        ]
        (tmp_path / "stella-events.jsonl").write_text(
            "\n".join(json.dumps(event) for event in events)
        )

        agent = _bare_agent()
        agent.logs_dir = tmp_path
        agent.model_name = worker
        agent._extra_env = {}
        agent._version = "stella 0.6.21"

        _posture, posture_json, posture_digest = _benchmark_engine_posture(
            worker, witness_author=author
        )
        seen: dict[str, str] = {}

        class _Environment:
            async def _stella_secure_exec_with_stdin(
                self, *, command: list[str], env: dict[str, str], stdin: bytes
            ):
                seen["posture"] = env[_ENGINE_CONFIG_ENV]
                # The author must never travel as its own container variable:
                # the posture is the single channel, so there is exactly one
                # thing to hash and exactly one thing that can disagree.
                assert _WITNESS_AUTHOR_ENV not in env
                return SimpleNamespace(stdout=None, stderr=None, return_code=0)

        context = AgentContext()
        asyncio.run(
            StellaAgent.run.__wrapped__(agent, "Fix the task.", _Environment(), context)
        )

        assert seen["posture"] == posture_json
        assert json.loads(seen["posture"])["pipeline_judge_model"] == author
        assert agent._engine_posture_sha256 == posture_digest

        assert context.metadata["stella_assurance_arm"] == "witness-on"
        assert context.metadata["stella_witness_author_model"] == author
        assert context.metadata["stella_assurance_tiers_version"] == (
            _ASSURANCE_TIERS_VERSION
        )
        assert (
            context.metadata["stella_assurance_tiers"]["tiers"]["authored_witness"]
            == "on"
        )
        assert len(context.metadata["stella_assurance_tiers_sha256"]) == 64
        # Declared *and* observed, which are different claims: the posture
        # enabled the tier, and this trial's proof stream shows it ran.
        assert context.metadata["stella_witness_authored_state"] == "authored"
        assert context.metadata["stella_stream"]["witness_authored"] is True

    def test_control_arm_metadata_declares_the_witness_off(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The disabled tier is a field, not a log line — the point of #1007."""
        monkeypatch.setenv("STELLA_BUDGET", "0.17")
        monkeypatch.setenv("OPENROUTER_API_KEY", "openrouter-test-secret")
        monkeypatch.delenv(_WITNESS_AUTHOR_ENV, raising=False)
        reason = (
            "no author independent of the worker (judge and worker both "
            "resolved to `openrouter/z-ai/glm-5.1`)"
        )
        events = [
            {
                "type": "proof",
                "step": {"kind": "witness_unavailable", "reason": reason},
            },
            {"type": "complete", "status": "completed", "cost_usd": 0.42},
        ]
        (tmp_path / "stella-events.jsonl").write_text(
            "\n".join(json.dumps(event) for event in events)
        )

        agent = _bare_agent()
        agent.logs_dir = tmp_path
        agent.model_name = "openrouter/z-ai/glm-5.1"
        agent._extra_env = {}
        agent._version = "stella 0.6.21"

        class _Environment:
            async def _stella_secure_exec_with_stdin(
                self, *, command: list[str], env: dict[str, str], stdin: bytes
            ):
                assert "pipeline_judge_model" not in env[_ENGINE_CONFIG_ENV]
                return SimpleNamespace(stdout=None, stderr=None, return_code=0)

        context = AgentContext()
        asyncio.run(
            StellaAgent.run.__wrapped__(agent, "Fix the task.", _Environment(), context)
        )

        assert context.metadata["stella_assurance_arm"] == "witness-off"
        assert "stella_witness_author_model" not in context.metadata
        assert context.metadata["stella_witness_authored_state"] == "unavailable"
        assert context.metadata["stella_assurance_tiers"]["authored_witness_off_reason"]
        assert context.metadata["stella_stream"]["witness_unavailable_reasons"] == [
            reason
        ]
