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
import re
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
    resolve_candidates,
    resolve_max_revisions,
    resolve_model_timeout,
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


class TestAttemptCountArms:
    """Revisions and best-of-N as selectable, hashed arms (#1211 §6.7, §6.8).

    Before these existed the two knobs were unreachable rather than merely
    unset: outside `stella-pipeline`'s own tests, `max_revisions` was written
    once (to `2`) and `candidates` once (to `None`), so best-of-N was fully
    implemented and had never run in production.
    """

    _MODEL = "openrouter/anthropic/claude-sonnet-5"

    def test_unset_selectors_reproduce_the_frozen_posture(self) -> None:
        """The keys must be ABSENT when unselected, not present-at-default.

        The digest is taken over this dict, so writing `pipeline_max_revisions:
        2` — the value every historical run actually had — would still change
        every recorded hash, to describe a posture identical to the one they
        already described. Absence is the only encoding of "the engine's
        default" that keeps `bench/READINESS.md` honest.
        """
        default_posture, default_json, default_digest = _benchmark_engine_posture(
            self._MODEL
        )
        explicit = _benchmark_engine_posture(
            self._MODEL, max_revisions=None, candidates=None
        )
        assert default_json == explicit[1]
        assert default_digest == explicit[2]
        assert "pipeline_max_revisions" not in default_posture
        assert "pipeline_candidates" not in default_posture

    def test_the_registered_sonnet_digest_is_unchanged(self) -> None:
        """The one digest an external gate already checks by prefix.

        `bench/evidence/run/preflight_effort.sh` defaults `EXPECT_DIGEST` to
        this value and `bench/READINESS.md` §8.4.3 registers it. Pinning it
        here means a posture change is caught in unit tests rather than by a
        preflight on the rig, where the feedback costs a run.
        """
        _posture, _normalized, digest = _benchmark_engine_posture(
            "anthropic/claude-sonnet-5"
        )
        assert digest.startswith("3c428a22")

    def test_each_selector_moves_the_digest_and_only_its_own_key(self) -> None:
        """Two arms, two hashes — and neither disturbs the rest of the posture."""
        base, _base_json, base_digest = _benchmark_engine_posture(self._MODEL)
        revised, _revised_json, revised_digest = _benchmark_engine_posture(
            self._MODEL, max_revisions=4
        )
        best_of, _best_json, best_digest = _benchmark_engine_posture(
            self._MODEL, candidates=2
        )
        assert revised["pipeline_max_revisions"] == 4
        assert "pipeline_candidates" not in revised
        assert best_of["pipeline_candidates"] == 2
        assert "pipeline_max_revisions" not in best_of
        assert len({base_digest, revised_digest, best_digest}) == 3
        # Model routing, effort and the ceilings are untouched by either — an
        # attempt-count arm must not be a second, undeclared variable.
        for arm in (revised, best_of):
            for key in ("default_model", "allowed_models", "agents"):
                assert arm[key] == base[key]

    def test_selected_knobs_stay_inside_the_launcher_vocabulary(self) -> None:
        """The fail-closed seam refuses the RUN on an unknown root key.

        `config::trusted_engine_config_shape_is_strict` shares its vocabulary
        with `settings::ENGINE_ROOT_FIELDS`, so an unrecognised key here is not
        dropped — the trial dies at launch. This asserts against a literal copy
        of that list on purpose: a shared constant would drift together with
        the thing it is supposed to catch drifting.
        """
        posture, _normalized, _digest = _benchmark_engine_posture(
            self._MODEL,
            witness_author="openrouter/anthropic/claude-fable-5",
            worker_effort="xhigh",
            triage_model="openrouter/anthropic/claude-haiku-4.5",
            max_revisions=4,
            candidates=2,
            model_timeout_secs=1572,
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
            "pipeline_max_revisions",
            "pipeline_candidates",
            "model_timeout_secs",
            "agents",
        }
        assert set(posture) <= allowed_roots

    def test_resolvers_treat_unset_as_inherit_and_refuse_a_lost_value(self) -> None:
        """`None` inherits; empty is a refusal, not a second spelling of `None`.

        An empty selector means a value was lost between the launcher and here.
        Inheriting the default there would score the run under a configuration
        nobody chose — the same failure the effort selector refuses.
        """
        assert resolve_max_revisions(None) is None
        assert resolve_candidates(None) is None
        assert resolve_max_revisions("4") == 4
        assert resolve_candidates(" 2 ") == 2
        for resolver in (resolve_max_revisions, resolve_candidates):
            with pytest.raises(ValueError, match="must not be empty"):
                resolver("   ")
            with pytest.raises(ValueError, match="must be an integer"):
                resolver("two")

    def test_bounds_refuse_a_fat_fingered_digit_and_a_zero_candidate(self) -> None:
        """The ceilings catch the typo whose cost is a bill, not a wrong answer.

        `0` splits the two: no revisions is a real arm (one shot, no retry), so
        it is admissible. Zero candidates is not — the pipeline floors it to 1
        anyway, so accepting it would let two selector values describe the same
        run under two different digests.
        """
        assert resolve_max_revisions("0") == 0
        with pytest.raises(ValueError, match="candidates must be between 1"):
            resolve_candidates("0")
        with pytest.raises(ValueError, match="max revisions must be between"):
            resolve_max_revisions("40")
        with pytest.raises(ValueError, match="candidates must be between"):
            resolve_candidates("20")


class TestModelTimeoutArm:
    """The third coupled ceiling, as a selectable hashed arm (#1211 §6.2).

    The other two have been selectable for generations — the output cap rides
    `agents.<role>.params.max_tokens` in the posture itself, the turn budget is
    a per-trial flag — but `model_timeout` was an `EngineConfig` constant. On
    this protocol that made a timeout change a **system-under-test** change: a
    re-freeze of the registered commit rather than a line in a posture, which
    is one of the three reasons the Fable-class ceiling memo gives for why the
    change could not be automated away.

    Nothing here decides the Fable arm's numbers. It makes them expressible.
    """

    _MODEL = "openrouter/anthropic/claude-sonnet-5"

    def test_unset_omits_the_key_and_reproduces_the_frozen_posture(self) -> None:
        """Absent means "the engine's default", which is what history had.

        The same rule the attempt-count knobs follow, and load-bearing for the
        same reason: writing `model_timeout_secs: 816` — the value every run so
        far actually had — would change every recorded digest to describe a
        posture identical to the one it already described.
        """
        default_posture, default_json, default_digest = _benchmark_engine_posture(
            self._MODEL
        )
        explicit = _benchmark_engine_posture(self._MODEL, model_timeout_secs=None)
        assert default_json == explicit[1]
        assert default_digest == explicit[2]
        assert "model_timeout_secs" not in default_posture
        # The digest an external preflight already checks by prefix, asserted
        # again from this arm's own test so a regression here cannot be read as
        # someone else's failure.
        assert _benchmark_engine_posture("anthropic/claude-sonnet-5")[2].startswith(
            "3c428a22"
        )

    def test_a_selected_timeout_lands_in_the_digest_and_moves_nothing_else(
        self,
    ) -> None:
        """Selecting the ceiling declares itself, and declares only itself."""
        base, _base_json, base_digest = _benchmark_engine_posture(self._MODEL)
        scaled, _scaled_json, scaled_digest = _benchmark_engine_posture(
            self._MODEL, model_timeout_secs=1572
        )
        assert scaled["model_timeout_secs"] == 1572
        assert scaled_digest != base_digest
        for key in ("default_model", "allowed_models", "agents"):
            assert scaled[key] == base[key]

    def test_zero_is_no_backstop_and_is_distinct_from_unset(self) -> None:
        """Three states, not two — and the digest can tell them apart.

        `None` asks for the engine's default; `0` asks for no ceiling at all
        (the engine's `Option::None`, an unbounded await). Collapsing them
        would make "I chose unbounded" indistinguishable from "I chose
        nothing", which is precisely the ambiguity these selectors exist to
        remove.
        """
        assert resolve_model_timeout("0") == 0
        unset, _unset_json, unset_digest = _benchmark_engine_posture(self._MODEL)
        unbounded, _json, unbounded_digest = _benchmark_engine_posture(
            self._MODEL, model_timeout_secs=0
        )
        assert "model_timeout_secs" not in unset
        assert unbounded["model_timeout_secs"] == 0
        assert unbounded_digest != unset_digest

    def test_the_resolver_refuses_a_lost_value_and_a_fat_fingered_digit(
        self,
    ) -> None:
        """Fails closed on every non-value, like every selector beside it.

        The ceiling is the one that matters on a benchmark: a timeout an order
        of magnitude too large does not fail loudly, it spends the trial's
        whole agent budget in silence and reports the result as a task the
        agent could not solve.
        """
        assert resolve_model_timeout(None) is None
        assert resolve_model_timeout(" 1572 ") == 1572
        with pytest.raises(ValueError, match="must not be empty"):
            resolve_model_timeout("   ")
        with pytest.raises(ValueError, match="must be an integer"):
            resolve_model_timeout("1572s")
        with pytest.raises(ValueError, match="model timeout must be between"):
            resolve_model_timeout("86400")

    def test_the_three_ceilings_can_be_selected_together(self) -> None:
        """The point of the knob: one posture that scales the whole budget.

        A Fable-class arm raises the output cap AND the silence ceiling that
        has to absorb it. Expressing only one is how `16384 -> 32000 -> 64000`
        each relocated the cliff instead of removing it — the recorded history
        of this very posture.
        """
        posture, _normalized, _digest = _benchmark_engine_posture(
            self._MODEL, model_timeout_secs=1572
        )
        assert posture["model_timeout_secs"] == 1572
        capped = [
            role
            for role, agent in posture["agents"].items()
            if "params" in agent and "max_tokens" in agent["params"]
        ]
        assert sorted(capped) == ["default", "judge", "worker"]


class TestFableCeilingSet:
    """The approved Fable ceiling set (#1211 §6.2): 128,000 output, 1,572s.

    Both ceilings are properties of the model now, not one shared constant.
    One shared number was only ever right by coincidence — it is the model's
    own ceiling, and models differ. Fable 5 answers up to 128,000 output
    tokens, so capping its trials at Sonnet's 64,000 stopped it at half the
    height the comparator is allowed to fill, and the score reported that as
    a capability difference rather than as our own ceiling.
    """

    def test_fable_gets_both_approved_ceilings(self) -> None:
        posture, _normalized, _digest = _benchmark_engine_posture(
            "anthropic/claude-fable-5"
        )
        for role in ("default", "worker", "judge"):
            assert posture["agents"][role]["params"]["max_tokens"] == 128_000
        assert posture["model_timeout_secs"] == 1_572

    def test_the_timeout_is_never_left_behind_when_the_cap_moves(self) -> None:
        """The two ceilings ship together or the raise does nothing.

        Raising the cap alone relocates the cliff instead of removing it: the
        step stops on the timeout rather than the cap, which looks the same in
        the results and is just as much us stopping first. This is the
        recorded history of this posture — 16384 -> 32000 -> 64000 each moved
        one ceiling while the others held — so it gets a test, not a comment.
        """
        for model in ("anthropic/claude-fable-5", "openrouter/anthropic/claude-fable-5"):
            posture, _normalized, _digest = _benchmark_engine_posture(model)
            raised = posture["agents"]["worker"]["params"]["max_tokens"] > 64_000
            assert raised, f"{model} should carry the raised cap"
            assert posture.get("model_timeout_secs") is not None, (
                f"{model} raised its output cap without raising the silence "
                "ceiling that has to absorb it"
            )

    def test_sonnet_is_untouched_and_its_registered_digest_holds(self) -> None:
        """The approval covered Fable. Sonnet's arm must be bit-identical.

        Its comparator stops at 64,000 — Claude Code landed steps on exactly
        that number twice and still emitted the tool call — so parity says
        Sonnet stays. `3c428a22…` is registered in `bench/READINESS.md` and
        defaulted by `preflight_effort.sh`, so moving it would silently
        invalidate an already-published number.
        """
        posture, _normalized, digest = _benchmark_engine_posture(
            "anthropic/claude-sonnet-5"
        )
        assert posture["agents"]["worker"]["params"]["max_tokens"] == 64_000
        assert "model_timeout_secs" not in posture
        assert digest.startswith("3c428a22")

    def test_the_booking_route_is_not_a_model_property(self) -> None:
        """Direct and gateway reach the same model, so the ceilings match.

        They still hash differently, because `default_model` differs and the
        digest describes the whole posture — but a model must not get a
        different ceiling for having been reached through OpenRouter.
        """
        direct, _dj, direct_digest = _benchmark_engine_posture(
            "anthropic/claude-fable-5"
        )
        gateway, _gj, gateway_digest = _benchmark_engine_posture(
            "openrouter/anthropic/claude-fable-5"
        )
        assert direct["agents"] == gateway["agents"]
        assert direct["model_timeout_secs"] == gateway["model_timeout_secs"]
        assert direct_digest != gateway_digest

    def test_an_explicit_selector_still_overrides_the_models_default(self) -> None:
        """The table is the default, not a lock — an arm can still say otherwise."""
        posture, _normalized, _digest = _benchmark_engine_posture(
            "anthropic/claude-fable-5", model_timeout_secs=900
        )
        assert posture["model_timeout_secs"] == 900

    def test_the_registered_fable_digests(self) -> None:
        """Pinned so a ceiling cannot move without this test naming it.

        These are the values `bench/READINESS.md` registers for the Fable arm.
        A posture change that does not update them is a run whose digest
        describes a configuration nobody registered.
        """
        assert _benchmark_engine_posture("anthropic/claude-fable-5")[2] == (
            "5d42e2364755534c5632189ca988b892d108f54c30901d988fc88037407b2bfe"
        )
        assert _benchmark_engine_posture("openrouter/anthropic/claude-fable-5")[2] == (
            "18a1ba22e2ffef7fb8634504a0d6aff39c3117a52e48dd0f22159693641fd572"
        )


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


_CATALOG_RS = (
    Path(__file__).resolve().parents[3] / "stella-model" / "src" / "catalog.rs"
)


def _parse_output_ceilings(source: str) -> dict[str, int]:
    """Collect each shipping model's completion ceiling out of catalog.rs text.

    Parsed rather than duplicated, because duplicating it is the defect this
    exists to catch. Entries with no `with_max_output_tokens` are omitted:
    the engine falls back to its global default for those, and the posture
    has no per-model ceiling to match.

    Split out from the file read so the parser's own blind spots can be
    tested against synthetic source — see `TestCatalogCeilingParser`.
    """
    # Stop at the test module. Chunking on `CatalogEntry::new(` bounds every
    # chunk by the *next* entry, but the seed table's last row has no next
    # seeded entry — its chunk otherwise runs on into `#[cfg(test)]` and
    # adopts the first ceiling it finds among the fixtures there. Skipping
    # `test-only` slugs cannot prevent that: it filters an entry's own
    # identity, not the extent of the chunk before it. The module boundary
    # is what actually closes the region.
    source = source.split("#[cfg(test)]")[0]
    ceilings: dict[str, int] = {}
    for chunk in source.split("CatalogEntry::new(")[1:]:
        head = re.match(r'\s*"([^"]+)"\s*,\s*"([^"]+)"', chunk)
        if head is None:
            continue
        slug, provider = head.groups()
        if slug.startswith("test-only"):
            continue  # fixtures for the catalog's own tests, never benchmarked
        # Within the seed table `split` already consumed the delimiter, so a
        # chunk ends exactly where the next entry begins and the first
        # ceiling in it is this entry's own.
        ceiling = re.search(r"with_max_output_tokens\(Some\(([\d_]+)\)\)", chunk)
        if ceiling is None:
            continue
        # Always provider-qualified, never "the slug already looks qualified".
        # The gateway rows are seeded under slugs like
        # `anthropic/claude-fable-5`, which is what a caller writes as
        # `--model openrouter/anthropic/claude-fable-5`. Treating that slug as
        # already-qualified collapses it onto the direct-Anthropic row's key,
        # and since the gateway rows come later in the file they overwrite it
        # — the ratchet then silently checks one model's posture against
        # another model's ceiling. Caught by deliberately drifting a single
        # row and finding this test still green.
        ceilings[f"{provider}/{slug}"] = int(ceiling.group(1).replace("_", ""))
    return ceilings


def _seeded_output_ceilings() -> dict[str, int]:
    """`_parse_output_ceilings` over the real catalog this repo ships."""
    return _parse_output_ceilings(_CATALOG_RS.read_text(encoding="utf-8"))


class TestCatalogCeilingParser:
    """The parser's own blind spots, pinned against synthetic source.

    The parity check below is only as trustworthy as what this reads out of
    `catalog.rs`, and both ways it has been wrong so far were silent: a
    number was still produced, just the wrong model's. Neither would have
    reddened anything. So the two known misreads get fixtures rather than a
    comment, because a comment does not fail.
    """

    _SHIPPING_ROW = """
                CatalogEntry::new(
                    "claude-sonnet-5",
                    "anthropic",
                    "claude",
                    200_000,
                )
                .with_max_output_tokens(Some(64_000)),
    """

    # The seed table's *last* row, and — as in `catalog.rs` today — one that
    # declares no ceiling of its own. That is what leaves its chunk open: with
    # no ceiling to find first, the next one anywhere below becomes "its".
    _UNCAPPED_LAST_ROW = """
                CatalogEntry::new(
                    "anthropic/claude-haiku-4.5",
                    "openrouter",
                    "claude",
                    200_000,
                ),
    """

    _TEST_MODULE_BARE_BUILDER = """
#[cfg(test)]
mod tests {
    #[test]
    fn a_row_carries_its_own_ceiling() {
        let entry = base.with_max_output_tokens(Some(8_000));
        assert_eq!(entry.max_output_tokens, Some(8_000));
    }
}
"""

    def test_a_fixtures_ceiling_is_not_attributed_to_the_last_shipping_row(
        self,
    ) -> None:
        """The seed table's last row must not adopt a number from the tests.

        Nothing closes its chunk, and it has no ceiling of its own to be found
        first. The bare builder call below is the shape `catalog.rs` actually
        contains — a fixture asserting a ceiling round-trips, reached with no
        `CatalogEntry::new(` in between — so an uncapped shipping row came back
        capped at a number written to exercise the setter. Silent either way:
        green if the posture happens to sit at 8000, otherwise red about a
        model that never declared a ceiling at all.
        """
        source = (
            self._SHIPPING_ROW
            + self._UNCAPPED_LAST_ROW
            + self._TEST_MODULE_BARE_BUILDER
        )
        assert _parse_output_ceilings(source) == {"anthropic/claude-sonnet-5": 64000}

    def test_a_fixture_entry_does_not_become_a_benchmarked_model(self) -> None:
        # The other way test source leaks: a fixture with its own
        # `CatalogEntry::new(` becomes a chunk, and a phantom row the parity
        # check then demands the posture cap at. Its slug is whatever the
        # fixture author picked, so the `test-only` prefix is not a guarantee.
        source = (
            self._SHIPPING_ROW
            + """
#[cfg(test)]
mod tests {
    #[test]
    fn a_row_carries_its_own_ceiling() {
        let entry = CatalogEntry::new(
            "some-fixture",
            "anthropic",
            "claude",
            200_000,
        )
        .with_max_output_tokens(Some(8_000));
    }
}
"""
        )
        assert _parse_output_ceilings(source) == {"anthropic/claude-sonnet-5": 64000}

    def test_a_gateway_slug_keeps_its_own_key(self) -> None:
        # `anthropic/claude-sonnet-5` seeded under `openrouter` is a distinct
        # row from the direct-Anthropic one. Reading the already-slashed slug
        # as fully qualified collapses the two, and the later row wins — so
        # one model's posture gets checked against the other's ceiling.
        source = (
            self._SHIPPING_ROW
            + """
                CatalogEntry::new(
                    "anthropic/claude-sonnet-5",
                    "openrouter",
                    "claude",
                    200_000,
                )
                .with_max_output_tokens(Some(48_000)),
    """
        )
        assert _parse_output_ceilings(source) == {
            "anthropic/claude-sonnet-5": 64000,
            "openrouter/anthropic/claude-sonnet-5": 48000,
        }


class TestOutputCeilingParity:
    """#1211 §6.2: the posture's cap must be the model's own ceiling.

    `params.max_tokens` exists to stop Stella capping itself below what the
    comparator gets — "never be the side that stops first". It is a literal
    in `posture.py`, and the model's actual ceiling is a literal in
    `stella-model/src/catalog.rs`. Nothing connected the two, so a catalog
    that learns a higher ceiling leaves the benchmark quietly capped at the
    old one: the exact handicap the constant was introduced to remove, now
    invisible because both numbers still look deliberate.
    """

    def test_catalog_is_readable_and_seeds_ceilings(self) -> None:
        # If this fails, the parser below is silently checking nothing —
        # which is how a ratchet becomes decoration.
        assert _CATALOG_RS.is_file(), f"{_CATALOG_RS} is missing"
        assert _seeded_output_ceilings(), "no seeded ceiling was parsed"

    def test_posture_caps_every_seeded_model_at_its_own_ceiling(self) -> None:
        for model, ceiling in _seeded_output_ceilings().items():
            posture, _, _ = _benchmark_engine_posture(model)
            for role, agent in posture["agents"].items():
                params = agent.get("params")
                if params is None:
                    # `triage` keeps the engine default deliberately: it emits
                    # a three-line classification, so the cap never binds.
                    assert role == "triage", f"{role} unexpectedly has no cap"
                    continue
                assert params["max_tokens"] == ceiling, (
                    f"{model} role {role}: the posture caps at "
                    f"{params['max_tokens']} but the catalog seeds the model's "
                    f"ceiling at {ceiling}. Whichever moved, the benchmark is "
                    "now capping itself away from the comparator — raise the "
                    "posture, or correct the catalog."
                )
