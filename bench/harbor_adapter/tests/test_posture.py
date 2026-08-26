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

import ast
import asyncio
import json
import re
import sys
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
    assurance_tiers_from_posture,
    refuse_unauthorable_witness_arm,
)

# Imported from the module rather than the package: internals of the posture's
# ceiling and selector policy, not part of what the adapter re-exports. The
# selector resolvers joined them when the adapter stopped calling them one by
# one — `read_posture_selectors` is the package-level entry point now, and a
# re-export kept alive only for a test is a public surface nobody asked for.
from stella_harbor.posture import (  # noqa: E402 - after importorskip by design
    _BENCHMARKED_SLUGS,
    _MINIMAL_PROMPT_ENV,
    read_posture_selectors,
    resolve_minimal_prompt,
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
        ``verifier`` every role inherits one ``default_model``, which is
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
        assert "verifier" in roles, "the verifier role must be stated, not implied"
        resolved = {
            role: config.get("model", posture["default_model"])
            for role, config in roles.items()
        }
        assert resolved["verifier"] == resolved["worker"] == model
        assert "pipeline_verifier_model" not in posture
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
            worker, verifier=author
        )

        assert arm["default_model"] == worker
        assert arm["pipeline_verifier_model"] == author, (
            "the verifier role is what the authored-witness tier resolves its "
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
        refused one. Checked against the vocabulary PARSED from `unknown.rs`
        (#2033) so the Python side fails in unit tests rather than on the
        first container — including when the Rust side removes a key this
        posture still emits, which a hand-copy silently missed.
        """
        for verifier in (None, "openrouter/deepseek/deepseek-v4-pro"):
            posture, _json_text, _digest = _benchmark_engine_posture(
                "openrouter/z-ai/glm-5.1", verifier=verifier
            )
            unknown = set(posture) - _engine_root_fields()
            assert not unknown, (
                f"the trusted launcher seam would refuse this posture: {unknown}"
            )

    def test_verifier_must_be_independent_and_same_provider(self) -> None:
        """Both refusals exist so an arm cannot degrade into the other silently."""
        worker = "openrouter/z-ai/glm-5.1"

        with pytest.raises(ValueError, match="must differ from the worker model"):
            _benchmark_engine_posture(worker, verifier=worker)

        # One credential reaches the container, resolved from the worker's
        # provider. A second provider would authenticate against nothing.
        with pytest.raises(ValueError, match="must share the worker's provider"):
            _benchmark_engine_posture(worker, verifier="anthropic/claude-fable-5")

        with pytest.raises(ValueError, match="provider/model spec"):
            _benchmark_engine_posture(worker, verifier="not-a-spec")

    def test_assurance_tiers_declare_the_arm_and_are_hashed(self) -> None:
        """A scored run declares a disabled tier instead of only logging it."""
        worker = "openrouter/z-ai/glm-5.1"
        author = "openrouter/deepseek/deepseek-v4-pro"

        off, off_json, off_digest = _benchmark_assurance_tiers(worker)
        assert off["version"] == _ASSURANCE_TIERS_VERSION
        assert off["arm"] == "witness-off"
        assert off["verifier_model"] is None
        assert off["tiers"]["authored_witness"] == "off"
        assert off["tiers"]["model_verdict"] == "on-same-model-as-worker"
        assert off["tiers"]["deterministic_verify"] == "on"
        assert (
            "no author independent of the worker"
            in (off["authored_witness_off_reason"])
        )
        assert json.loads(off_json) == off

        on, _on_json, on_digest = _benchmark_assurance_tiers(
            worker, verifier=author
        )
        assert on["arm"] == "witness-on"
        assert on["verifier_model"] == author
        assert on["tiers"]["authored_witness"] == "on"
        assert on["tiers"]["model_verdict"] == "on-independent-of-worker"
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
        assert high["agents"]["verifier"]["effort"] == "xhigh"
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
        outside `ENGINE_ROOT_FIELDS`, so a posture that names a verifier *and* a
        triage author is the shape most likely to trip it — and it refuses the
        run outright rather than dropping the unknown key.
        """
        posture, _normalized, _digest = _benchmark_engine_posture(
            self._MODEL,
            verifier="openrouter/anthropic/claude-fable-5",
            worker_effort="high",
            triage_model="openrouter/anthropic/claude-haiku-4.5",
        )
        # The vocabulary is PARSED from the launcher's own `unknown.rs`
        # (#2033), never hand-copied: the copy this replaced listed 10 of the
        # authority's 20 keys, failing a legitimate `model_timeout_secs`
        # posture while claiming the launcher would refuse it.
        assert set(posture) <= _engine_root_fields()
        assert set(posture["agents"]) <= _engine_agent_names()
        # Every pinned role is in the whitelist, or it cannot be resolved.
        for role_key in ("pipeline_verifier_model", "pipeline_triage_model"):
            assert posture[role_key] in posture["allowed_models"]


class TestRetiredAttemptCountArms:
    """The three knobs the staged pipeline took with it (#3871).

    `max_revisions` and `candidates` (#1211 §6.7, §6.8) and
    `verifier_evidence_demand` (#1295) were selectable, hashed arms until
    `crates/stella-pipeline` — the only thing that read any of them — was
    deleted (#3865). This class is what is left: proof that the posture no
    longer emits their keys, that asking for one fails loudly at the call site,
    and that the digest the removal must NOT have moved did not move.
    """

    _MODEL = "openrouter/anthropic/claude-sonnet-5"

    _RETIRED_KEYS = (
        "pipeline_max_revisions",
        "pipeline_candidates",
        "pipeline_verifier_evidence_demand",
    )
    _RETIRED_KWARGS = ("max_revisions", "candidates", "verifier_evidence_demand")

    def test_the_retired_keys_are_neither_emitted_nor_accepted(self) -> None:
        """**Witness (#3871).** Either half alone is passable, so both are here.

        The emission half fails on the pre-removal code, which writes each key
        whenever its selector is set. The kwarg half is what makes the removal
        *loud*: a caller that still selects an arm gets a `TypeError` naming the
        argument, here, rather than a run that launches and is refused by
        `config::trusted_engine_config_shape_is_strict` with an engine-config
        error that never names the knob — or, worse on the pre-#3865 engine, one
        that starts and silently measures the default.
        """
        posture, _normalized, _digest = _benchmark_engine_posture(self._MODEL)
        for key in self._RETIRED_KEYS:
            assert key not in posture, f"{key} is retired and must not be emitted"

        for kwarg in self._RETIRED_KWARGS:
            with pytest.raises(TypeError, match=kwarg):
                _benchmark_engine_posture(self._MODEL, **{kwarg: 2})

    def test_the_registered_sonnet_digest_is_unchanged(self) -> None:
        """The one digest an external gate already checks by prefix.

        `bench/evidence/run/preflight_effort.sh` defaults `EXPECT_DIGEST` to
        this value and `bench/READINESS.md` §8.4.5 registers it. Pinning it
        here means a posture change is caught in unit tests rather than by a
        preflight on the rig, where the feedback costs a run.

        It moved once, from `c8536200`, when the output cap left the posture
        (#2411). Retiring the three attempt-count knobs (#3871) must NOT move
        it a second time, and that is this test's job here: all three followed
        the omit-when-unset rule, so a posture that never selected one is
        byte-identical before and after their removal. A red assertion here
        would mean the removal silently re-registered every historical number
        under a posture that no longer describes them.
        """
        _posture, _normalized, digest = _benchmark_engine_posture(
            "anthropic/claude-sonnet-5"
        )
        assert digest.startswith("6c7fc70c")

    def test_selected_knobs_stay_inside_the_launcher_vocabulary(self) -> None:
        """The fail-closed seam refuses the RUN on an unknown root key.

        `config::trusted_engine_config_shape_is_strict` shares its vocabulary
        with `settings::ENGINE_ROOT_FIELDS`, so an unrecognised key here is not
        dropped — the trial dies at launch. Checked against the vocabulary
        parsed from `unknown.rs` (#2033). This used to argue for a literal
        copy ("a shared constant would drift together with the thing it
        catches drifting"), but the argument runs backwards for the direction
        that costs money: a key REMOVED from the Rust side left the literal
        green while every run refused at launch. The parsed set moves with
        the authority, which is exactly what makes a removal fail here.
        """
        posture, _normalized, _digest = _benchmark_engine_posture(
            self._MODEL,
            verifier="openrouter/anthropic/claude-fable-5",
            worker_effort="xhigh",
            triage_model="openrouter/anthropic/claude-haiku-4.5",
            model_timeout_secs=1572,
        )
        assert set(posture) <= _engine_root_fields()


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

        The same rule the attempt-count knobs follow, and required for the
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
            "6c7fc70c"
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

        A timeout an order of magnitude too large is the dangerous end on a
        benchmark: it does not fail loudly, it spends the trial's
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

    def test_selecting_the_silence_ceiling_adds_no_output_cap(self) -> None:
        """The knob scales the one ceiling left, and cannot smuggle back another.

        It used to be checked as a set: a Fable-class arm raised the output cap
        AND the silence ceiling that absorbs it, because expressing only one is
        how `16384 -> 32000 -> 64000` each relocated the cliff instead of
        removing it. Removing the cap ended that sequence (#2411), so what this
        now pins is the other direction — selecting a timeout must not
        reintroduce a `max_tokens` on any role.
        """
        posture, _normalized, _digest = _benchmark_engine_posture(
            self._MODEL, model_timeout_secs=1572
        )
        assert posture["model_timeout_secs"] == 1572
        capped = [
            role
            for role, agent in posture["agents"].items()
            if "max_tokens" in (agent.get("params") or {})
        ]
        assert capped == []


class TestFableCeilingSet:
    """What is left of the Fable ceiling set (#1211 §6.2) after #2411.

    It was two ceilings, 128,000 output and 1,572s of silence. The output
    half is gone with every other cap: 128,000 was Fable's own maximum, which
    is exactly what the engine takes from the catalog when nothing is sent, so
    the posture was restating the authority and owning a copy that could drift.

    The silence ceiling stays, and matters more rather than less. It is the
    one per-generation ceiling still set here, and it now has to absorb a
    generation nothing else bounds.
    """

    def test_fable_keeps_its_silence_ceiling_and_asks_for_no_cap(self) -> None:
        posture, _normalized, _digest = _benchmark_engine_posture(
            "anthropic/claude-fable-5"
        )
        for role in ("default", "worker", "verifier"):
            assert "max_tokens" not in (posture["agents"][role].get("params") or {})
        assert posture["model_timeout_secs"] == 1_572

    def test_an_uncapped_generation_still_has_a_ceiling_to_absorb_it(self) -> None:
        """The half of the old pairing that survives, and why it survives.

        The rule was that the cap and the timeout ship together, because
        raising one alone relocates the cliff rather than removing it — the
        recorded history of this posture, where 16384 -> 32000 -> 64000 each
        moved one ceiling while the others held.

        Removing the cap outright is the end of that sequence, not another
        step in it: the generation is now bounded by the model's own maximum.
        But Fable is the model whose maximum is large enough to have needed a
        bespoke silence ceiling in the first place, so that row must not be
        swept away with the cap it was derived from.
        """
        for model in ("anthropic/claude-fable-5", "openrouter/anthropic/claude-fable-5"):
            posture, _normalized, _digest = _benchmark_engine_posture(model)
            assert posture.get("model_timeout_secs") is not None, (
                f"{model} lost the silence ceiling that has to absorb a "
                "generation now bounded only by the model's own maximum"
            )

    def test_sonnet_asks_for_no_cap_and_its_new_digest_is_registered(self) -> None:
        """Sonnet's arm moved too, and that is the point of the change.

        It used to pin 64,000 — where Claude Code's steps were measured
        stopping — against a model that can write 128,000. That was the
        best-argued cap in the file and still a cap: a number the comparator
        was never given, imposed on the seat that was being measured against
        it. `6c7fc70c…` is registered in `bench/READINESS.md` 8.4.5 and
        defaulted by `preflight_effort.sh`.
        """
        posture, _normalized, digest = _benchmark_engine_posture(
            "anthropic/claude-sonnet-5"
        )
        assert "max_tokens" not in (posture["agents"]["worker"].get("params") or {})
        assert "model_timeout_secs" not in posture
        assert digest.startswith("6c7fc70c")

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
            "2099a1a48c0080974429964e13e0eee39e1f8af378c45e309318f34e03390e9b"
        )
        assert _benchmark_engine_posture("openrouter/anthropic/claude-fable-5")[2] == (
            "b5755ccf6a8cebe050606ffb8611fcc291d8b501efabcf222bf4159022bdaccc"
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
            "no author independent of the worker (verifier and worker both "
            "resolved to `openrouter/z-ai/glm-5.1`)"
        )
        events = [
            {
                "type": "proof",
                "step": {"kind": "assurance", "witness": True, "verifier": True},
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


class TestSelfVerdictStreamObservation:
    """What Stella claimed about its own work, beside what it was graded.

    The A/B in #1284 turns on comparing the two. Stella's claim is a
    `verdict` event and the grade is the verifier's reward, which the
    agent never sees — so a trial is the only place they meet, and the event
    stream is the only place the claim exists.
    """

    @staticmethod
    def _stream(*events: dict) -> dict:
        envelope = _stream_to_envelope(
            "\n".join(json.dumps(event) for event in events), process_returned=True
        )
        assert envelope is not None
        return envelope["_stella_stream"]

    def test_a_model_opinion_and_a_flip_oracle_are_told_apart(self) -> None:
        """`deterministic` is the difference between two instruments.

        #1284 measures the *verifier's* agreement with the official grader.
        Folding an oracle verdict in under the same name reports the ladder's
        aggregate as the verifier's reliability.
        """
        opinion = self._stream(
            {
                "type": "verdict",
                "passed": True,
                "evidence": {"summary": "looks right", "deterministic": False},
            },
            {"type": "complete", "status": "completed", "cost_usd": 0.5},
        )
        assert opinion["self_verdict_passed"] is True
        assert opinion["self_verdict_state"] == "passed"
        assert opinion["self_verdict_deterministic"] is False

        oracle = self._stream(
            {
                "type": "verdict",
                "passed": True,
                "evidence": {
                    "summary": "flip oracle: fail→pass on `pytest -q`",
                    "deterministic": True,
                },
            },
            {"type": "complete", "status": "completed", "cost_usd": 0.5},
        )
        assert oracle["self_verdict_deterministic"] is True

    def test_the_last_verdict_is_the_one_the_trial_ended_on(self) -> None:
        """A revised candidate is judged again; the reward grades the last one."""
        stream = self._stream(
            {
                "type": "verdict",
                "passed": False,
                "evidence": {"summary": "test still red", "deterministic": True},
            },
            {
                "type": "verdict",
                "passed": True,
                "evidence": {"summary": "flip observed", "deterministic": True},
            },
            {"type": "complete", "status": "completed", "cost_usd": 0.5},
        )
        assert stream["self_verdict_passed"] is True
        assert stream["self_verdict_count"] == 2

    def test_a_trial_that_closed_no_verdict_claimed_nothing(self) -> None:
        """Silence is not a failed verdict.

        An interrupted trial made no claim about its work, and scoring that as
        a truthful "it failed" credits the agent with honesty it never showed —
        on exactly the trials most likely to be re-read.
        """
        stream = self._stream(
            {"type": "complete", "status": "completed", "cost_usd": 0.5}
        )
        assert stream["self_verdict_passed"] is None
        assert stream["self_verdict_state"] == "not_reported"
        assert stream["self_verdict_deterministic"] is None
        assert stream["self_verdict_count"] == 0

    def test_an_abstention_is_counted_and_is_not_a_verdict(self) -> None:
        """#973: every evidence channel blind is a stated outcome of its own."""
        stream = self._stream(
            {
                "type": "proof",
                "step": {
                    "kind": "verification_unavailable",
                    "reason": "no oracle, no test result, unreadable tree",
                },
            },
            {"type": "complete", "status": "completed", "cost_usd": 0.5},
        )
        assert stream["verification_unavailable_count"] == 1
        assert stream["self_verdict_state"] == "not_reported"


class TestWitnessArmIsUnlaunchable:
    """#4103: the arm can still be described, and can no longer be run.

    The gate is tested here, on the declaration alone, as well as through the
    two run paths below and in `test_assurance_tiers.py`. That is deliberate
    rather than redundant: the run-path tests prove the gate is *wired* into
    every channel, and these prove it says the right thing about a declaration
    it is handed — which is what a future caller (a new harness, a manifest
    tool) will depend on.
    """

    def test_a_witness_on_declaration_is_refused(self) -> None:
        """The predicate, and the reason an operator has to act on."""
        worker = "openrouter/z-ai/glm-5.1"
        author = "openrouter/deepseek/deepseek-v4-pro"
        tiers, _json, _digest = _benchmark_assurance_tiers(worker, verifier=author)
        assert tiers["arm"] == "witness-on", "precondition: the declaration is on"

        with pytest.raises(RuntimeError) as refusal:
            refuse_unauthorable_witness_arm(tiers)

        reason = str(refusal.value)
        # Both models, the knob, and the issue — an operator must be able to
        # act on the refusal without opening this file.
        assert author in reason
        assert worker in reason
        assert _WITNESS_AUTHOR_ENV in reason
        assert "#4103" in reason
        assert "model_for" in reason, (
            "the reason must name the engine function whose collapse makes the "
            "arm unrunnable, or the next reader cannot check the claim"
        )

    def test_the_control_arm_passes_the_gate_untouched(self) -> None:
        """Every run this binary can honestly record still launches.

        The half that keeps this from being a bench-wide outage: the control
        arm is what every published Terminal-Bench number used, and the gate
        must be invisible to it.
        """
        tiers, _json, _digest = _benchmark_assurance_tiers("openrouter/z-ai/glm-5.1")
        assert tiers["arm"] == "witness-off"
        assert refuse_unauthorable_witness_arm(tiers) is None

    def test_a_recorded_declaration_still_reads_without_being_relaunched(self) -> None:
        """Reading is not launching, which is the whole shape of the decision.

        A witness-on declaration recorded before the collapse must still parse,
        still name its two models and still report its tiers — `compare_arms.py`
        and any forensic on an archived `result.json` depend on it. The gate is
        a launch-time question asked somewhere else; nothing about holding this
        dict may raise.
        """
        recorded = {
            "default_model": "openrouter/z-ai/glm-5.1",
            "pipeline_verifier_model": "openrouter/deepseek/deepseek-v4-pro",
        }
        tiers, _json, _digest = assurance_tiers_from_posture(recorded)

        assert tiers["arm"] == "witness-on"
        assert tiers["worker_model"] == "openrouter/z-ai/glm-5.1"
        assert tiers["verifier_model"] == "openrouter/deepseek/deepseek-v4-pro"
        assert tiers["authored_witness_off_reason"] is None


class TestWitnessArmEndToEnd:
    """Env knob -> the refusal that now ends it, and the control arm beside it."""

    def test_witness_arm_env_knob_refuses_the_run_before_the_container(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The treatment arm, end to end, now ends at the refusal (#4103).

        This used to assert the whole hop — env knob → posture → container →
        metadata — and the interesting middle one was that
        `_secure_exec_with_credential_fd` *recomputes* the posture from argv at
        the process boundary rather than trusting the caller (#1007).

        That hop is no longer reachable and asserting it would be asserting a
        fiction. The engine has one role, so the author this knob pins reaches
        no model call; a run that proceeded would put a control arm's
        configuration in the container under a treatment arm's digest, which is
        #1147 with the guard that used to catch it deleted (#3865). So the
        end-to-end claim this test makes is the one that is still true and
        still worth guarding: **asking for the arm stops the run, and stops it
        before the container is touched at all.**

        The exec stub asserts its own absence — reaching it is the failure.
        """
        worker = "openrouter/z-ai/glm-5.1"
        author = "openrouter/deepseek/deepseek-v4-pro"
        monkeypatch.delenv("STELLA_SPEND_LIMIT", raising=False)
        monkeypatch.setenv("OPENROUTER_API_KEY", "openrouter-test-secret")
        monkeypatch.setenv(_WITNESS_AUTHOR_ENV, author)

        agent = _bare_agent()
        agent.logs_dir = tmp_path
        agent.model_name = worker
        agent._extra_env = {}
        agent._version = "stella 0.6.21"

        class _Environment:
            async def _stella_secure_exec_with_stdin(
                self, *, command: list[str], env: dict[str, str], stdin: bytes
            ):
                raise AssertionError(
                    "the refused witness arm reached the container: the guard "
                    "must fire before any exec, not after"
                )

        context = AgentContext()
        with pytest.raises(RuntimeError) as refusal:
            asyncio.run(
                StellaAgent.run.__wrapped__(
                    agent, "Fix the task.", _Environment(), context
                )
            )

        reason = str(refusal.value)
        assert author in reason and worker in reason, (
            "the refusal must name both models, so an operator reads which two "
            "the posture believed it had rather than a bare policy sentence"
        )
        assert _WITNESS_AUTHOR_ENV in reason, "it must name the knob to unset"
        assert "#4103" in reason
        assert not context.metadata, "a refused arm records no trial metadata"

    def test_the_treatment_posture_still_builds_so_its_digests_re_derive(
        self,
    ) -> None:
        """Refusing the launch must not silence the builder (#4103).

        The two halves of the decision, in one assertion. Running the arm is
        refused because this binary cannot honor it; *describing* it stays
        total, because `bench/READINESS.md` §8.4 and §9 register digests
        computed from this exact function and a reader must be able to
        reproduce them byte for byte years later. Collapsing the builder to
        match the engine would have re-characterised recorded evidence nobody
        re-ran, which is the option #4103 declined.

        The digest is asserted against the control arm rather than a literal:
        a frozen hash here would pin the posture's *contents*, which is
        `TestBenchmarkPosture`'s job, and would fail for reasons that have
        nothing to do with this claim.
        """
        worker = "openrouter/z-ai/glm-5.1"
        author = "openrouter/deepseek/deepseek-v4-pro"

        arm, arm_json, arm_digest = _benchmark_engine_posture(worker, verifier=author)
        _control, _control_json, control_digest = _benchmark_engine_posture(worker)

        assert arm["pipeline_verifier_model"] == author
        assert json.loads(arm_json) == arm
        assert arm_digest != control_digest
        # Deterministic across calls: a digest that moved between two builds in
        # one process could never reproduce one recorded a year ago.
        assert _benchmark_engine_posture(worker, verifier=author)[2] == arm_digest

    def test_control_arm_metadata_declares_the_witness_off(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The disabled tier is a field, not a log line — the point of #1007."""
        monkeypatch.delenv("STELLA_SPEND_LIMIT", raising=False)
        monkeypatch.setenv("OPENROUTER_API_KEY", "openrouter-test-secret")
        monkeypatch.delenv(_WITNESS_AUTHOR_ENV, raising=False)
        reason = (
            "no author independent of the worker (verifier and worker both "
            "resolved to `openrouter/z-ai/glm-5.1`)"
        )
        events = [
            {
                "type": "proof",
                "step": {"kind": "witness_unavailable", "reason": reason},
            },
            {
                "type": "verdict",
                "passed": True,
                "evidence": {"summary": "reads correct", "deterministic": False},
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
                assert "pipeline_verifier_model" not in env[_ENGINE_CONFIG_ENV]
                return SimpleNamespace(stdout=None, stderr=None, return_code=0)

        context = AgentContext()
        asyncio.run(
            StellaAgent.run.__wrapped__(agent, "Fix the task.", _Environment(), context)
        )

        assert context.metadata["stella_assurance_arm"] == "witness-off"
        assert "stella_verifier_model" not in context.metadata
        assert context.metadata["stella_witness_authored_state"] == "unavailable"
        assert context.metadata["stella_assurance_tiers"]["authored_witness_off_reason"]
        assert context.metadata["stella_stream"]["witness_unavailable_reasons"] == [
            reason
        ]
        # The control arm's whole shape, in one trial: no witness could be
        # authored, and a model verifier — the worker's own model — declared the
        # work done anyway. Whether the grader agreed is the comparison #1284
        # runs, and it needs this field to be a field.
        assert context.metadata["stella_self_verdict_state"] == "passed"
        assert context.metadata["stella_stream"]["self_verdict_deterministic"] is False


_REPO_ROOT = Path(__file__).resolve().parents[3]
_BENCH_WORKFLOW = _REPO_ROOT / ".github" / "workflows" / "bench.yml"
_UNKNOWN_RS = _REPO_ROOT / "crates" / "stella-cli" / "src" / "settings" / "unknown.rs"
_SECURE_LAUNCHER_PY = (
    _REPO_ROOT / "bench" / "harbor_adapter" / "stella_harbor" / "secure_launcher.py"
)


def _unread_private_module_constants(path: Path) -> list[str]:
    """Module-level ``_NAME = ...`` bindings the module never loads again.

    A private name has no importer outside the file by construction, so one
    that is never loaded inside it is read by nothing at all — which is how a
    schema the launcher looks like it enforces can sit beside the check that
    actually runs and describe a different shape.
    """
    tree = ast.parse(path.read_text(encoding="utf-8"))
    bound: list[str] = []
    for node in tree.body:
        targets: list[ast.expr] = []
        if isinstance(node, ast.Assign):
            targets = list(node.targets)
        elif isinstance(node, ast.AnnAssign):
            targets = [node.target]
        for target in targets:
            if isinstance(target, ast.Name) and target.id.startswith("_"):
                bound.append(target.id)
    loaded = {
        node.id
        for node in ast.walk(tree)
        if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Load)
    }
    return sorted(name for name in bound if name not in loaded)


class TestLauncherDeclaresNoVocabularyNothingReads:
    """#4668: the launcher states a posture shape or it does not state one.

    `_ENGINE_POSTURE_FIELDS` listed eight root keys and was referenced nowhere.
    It read as the fail-closed schema and was not one, and it had drifted from
    both authorities that are: `settings::ENGINE_ROOT_FIELDS` decides what the
    trusted launcher accepts, and `tb21_posture_schema.py` splits the recorded
    posture into required and optional halves — a split this set did not have,
    so wiring it as written would have refused every posture omitting an
    optional arm key. The check that does run is stronger than either: the
    confirmatory manifest's posture is rebuilt with `_benchmark_engine_posture`
    and compared whole.
    """

    def test_no_private_constant_in_the_launcher_is_read_by_nothing(self) -> None:
        assert _unread_private_module_constants(_SECURE_LAUNCHER_PY) == []

    def test_the_reader_finds_a_planted_dead_constant(self, tmp_path: Path) -> None:
        """The guard's own witness: without this, an `ast` walk that silently
        matched nothing would report a clean module forever."""
        probe = tmp_path / "probe.py"
        probe.write_text(
            "_LIVE = 1\n_DEAD = 2\nPUBLIC = 3\nprint(_LIVE)\n", encoding="utf-8"
        )
        assert _unread_private_module_constants(probe) == ["_DEAD"]


def _parse_rust_str_slice(source: str, const_name: str) -> frozenset[str]:
    """Every string literal in a ``const NAME: &[&str] = &[ ... ];`` table."""
    # `=\s*&\[` rather than `= &\[`: rustfmt wraps the initialiser onto the
    # next line once the declaration is long enough, which is a formatting
    # accident this reader must not be sensitive to. It was —
    # `RETIRED_ENGINE_AGENT_NAMES` is wrapped that way, and a reader that
    # cannot see a table reports it missing rather than empty, which reads as
    # "the constant was renamed" and sends the next person to the wrong file.
    match = re.search(
        rf"const {const_name}: &\[&str\] =\s*&\[(.*?)\];", source, re.DOTALL
    )
    assert match is not None, (
        f"could not find `{const_name}` in {_UNKNOWN_RS} — the constant moved "
        "or was renamed; `_parse_rust_str_slice`'s caller in this file is the "
        "adapter's reader of the launcher vocabulary and must be repointed"
    )
    return frozenset(re.findall(r'"([^"]+)"', match.group(1)))


def _engine_root_fields() -> frozenset[str]:
    """`ENGINE_ROOT_FIELDS` out of `unknown.rs` — the launcher's fail-closed
    root-key vocabulary (#2033).

    Parsed rather than duplicated, for the same reason `_CATALOG_RS` is: the
    hand-copy this replaces listed 10 of the authority's 20 keys, which
    failed a legitimate posture (`model_timeout_secs`) while promising the
    launcher would refuse it — and had a key been *removed* from the Rust
    side, the copy would have kept passing while every run refused at the
    seam. Same brittleness note as the catalog: the path is a literal here,
    and a crate move is fixed here.
    """
    try:
        source = _UNKNOWN_RS.read_text(encoding="utf-8")
    except OSError as exc:
        raise AssertionError(
            f"cannot read the launcher vocabulary at {_UNKNOWN_RS}: "
            f"{exc.strerror}. That path is a literal in this file, not a "
            "resolved crate location — if the crate moved, update "
            "`_UNKNOWN_RS` to match."
        ) from exc
    fields = _parse_rust_str_slice(source, "ENGINE_ROOT_FIELDS")
    assert fields, "ENGINE_ROOT_FIELDS parsed to zero entries"
    # `RETIRED_ENGINE_ROOT` is the other half of the seam's vocabulary, and
    # leaving it out made this helper answer a different question than its
    # callers ask. They assert "the trusted launcher would not refuse this
    # posture"; the launcher recognizes both tables, so a posture naming a
    # retired key passes the seam and failed here — the assertion was simply
    # false about the code it names.
    #
    # #3908 retired those five keys and #3944 finished the collapse, and
    # `unknown.rs` is explicit that they are *deliberately* still recognized
    # rather than dropped: this file and `arenabench/harbor_agent.py` still
    # write them into hashed postures, so refusing them would invalidate every
    # digest registered in `bench/READINESS.md` §8.4 — a published-numbers
    # decision #3870 reserves for a maintainer. Recognized, ignored, reported.
    #
    # So the union is not a loosening; it is this helper finally describing the
    # seam. When the Python stops writing the retired keys (#3910 slice 6) the
    # Rust table empties, and this union collapses back to one set on its own
    # without anyone editing this line.
    retired = _parse_rust_str_slice(source, "RETIRED_ENGINE_ROOT")
    return fields | retired


def _engine_agent_names() -> frozenset[str]:
    """`ENGINE_AGENT_NAMES` out of `unknown.rs` — the role vocabulary.

    Parsed for the same reason `_engine_root_fields` is, and against the same
    failure: the literal this replaced named the four roles the posture happened
    to emit, so it could only ever agree with the posture — it could not answer
    the question it looked like it was answering, which is whether the launcher
    would accept a role the posture newly emits (#2549).
    """
    try:
        source = _UNKNOWN_RS.read_text(encoding="utf-8")
    except OSError as exc:
        raise AssertionError(
            f"cannot read the launcher vocabulary at {_UNKNOWN_RS}: "
            f"{exc.strerror}. That path is a literal in this file, not a "
            "resolved crate location — if the crate moved, update "
            "`_UNKNOWN_RS` to match."
        ) from exc
    names = _parse_rust_str_slice(source, "ENGINE_AGENT_NAMES")
    assert names, "ENGINE_AGENT_NAMES parsed to zero entries"
    # The same union, one level down, for the same stated reason: `unknown.rs`
    # keeps `RETIRED_ENGINE_AGENT_NAMES` recognized rather than dropping it,
    # explicitly citing `RETIRED_ENGINE_ROOT`'s argument. Core knows one role
    # name now (`default`, #3903), but a hashed posture written before the
    # collapse still carries the five personas, and the seam still accepts
    # them — so a caller asking "would the launcher refuse this?" has to read
    # both tables or it is asking a question about a stricter launcher than
    # the one that exists.
    retired = _parse_rust_str_slice(source, "RETIRED_ENGINE_AGENT_NAMES")
    return names | retired


class TestMinimalPromptArm:
    """The base persona, as a selectable hashed arm (#4650).

    `minimal_prompt` runs the session on a bare tool-advertisement persona, so
    the operator's prompt fields carry the prose. Comparing it against the
    shipped persona is the obvious experiment and the harness could not state
    it: no selector wrote the key, and the launcher's disclosed-posture
    vocabulary did not list it, so an arm had nowhere to declare the mode.

    Nothing here decides what the arm measures. It makes it expressible.
    """

    _MODEL = "openrouter/anthropic/claude-sonnet-5"

    def test_unset_omits_the_key_and_reproduces_the_frozen_posture(self) -> None:
        """Absent is the shipped persona, which is what every registered digest
        describes. Writing `minimal_prompt: "off"` unconditionally would
        re-hash every arm in `bench/READINESS.md` §8.4 to describe postures
        that behave exactly as the recorded ones do."""
        base, base_json, base_digest = _benchmark_engine_posture(self._MODEL)
        explicit = _benchmark_engine_posture(self._MODEL, minimal_prompt=None)
        assert base_json == explicit[1]
        assert base_digest == explicit[2]
        assert "minimal_prompt" not in base

    def test_each_setting_declares_itself_and_declares_only_itself(self) -> None:
        """Three states, and the digest tells all three apart: unset, `on`,
        `off`. `off` is expressible on purpose — an arm that measured the
        shipped persona deliberately must be able to say so, and absence
        already means something else."""
        base, _base_json, base_digest = _benchmark_engine_posture(self._MODEL)
        on, _on_json, on_digest = _benchmark_engine_posture(
            self._MODEL, minimal_prompt=True
        )
        off, _off_json, off_digest = _benchmark_engine_posture(
            self._MODEL, minimal_prompt=False
        )
        assert on["minimal_prompt"] == "on"
        assert off["minimal_prompt"] == "off"
        assert len({base_digest, on_digest, off_digest}) == 3
        for key in ("default_model", "allowed_models", "agents"):
            assert on[key] == base[key]
            assert off[key] == base[key]

    def test_the_selector_reaches_the_builder_through_the_one_reader(self) -> None:
        """Collection and the exec boundary both go through
        `read_posture_selectors`, so the selector has to be resolved there or
        the two paths compute different postures and the run is refused."""
        assert resolve_minimal_prompt(None) is None
        assert resolve_minimal_prompt("on") is True
        assert resolve_minimal_prompt("off") is False
        with pytest.raises(ValueError):
            resolve_minimal_prompt("maybe")

        selectors = read_posture_selectors({_MINIMAL_PROMPT_ENV: "on"}.get)
        assert selectors["minimal_prompt"] is True
        posture, _json, _digest = _benchmark_engine_posture(self._MODEL, **selectors)
        assert posture["minimal_prompt"] == "on"

    def test_the_key_is_inside_the_launcher_vocabulary(self) -> None:
        """The seam fails closed on any root key outside `ENGINE_ROOT_FIELDS`,
        so an arm that pins the mode is a refused run unless Rust admits the
        key. Read from the authority, never from a copy."""
        posture, _json, _digest = _benchmark_engine_posture(
            self._MODEL, minimal_prompt=True
        )
        assert "minimal_prompt" in _engine_root_fields()
        assert set(posture) <= _engine_root_fields()


class TestLauncherVocabularyParity:
    """#2033: the launcher's root-key vocabulary, read from the authority.

    The same defect class `TestOutputCeilingParity` guards for ceilings: a
    constant living in two places drifts. The hand-copy this replaced was
    too NARROW (10 of 20 keys — a legitimate `model_timeout_secs` posture
    failed the suite while the launcher would have accepted it, costing ten
    minutes of wrong conclusion while investigating #2021), and nothing
    prevented it going too WIDE (a key removed from Rust would leave a green
    suite promising a posture shape every run then refuses at the seam).
    There is no second copy any more — the parsed set IS the expectation —
    so the two failure directions collapse into one check: every key a
    posture emits is in the parsed vocabulary.
    """

    def test_the_vocabulary_is_readable_and_carries_the_anchors(self) -> None:
        fields = _engine_root_fields()
        # Two keys that can never leave: the digest anchor and the roles map.
        assert "default_model" in fields
        assert "agents" in fields

    def test_the_parser_reads_literals_not_prose(self) -> None:
        synthetic = (
            "/// docs mentioning \"decoy\"\n"
            'pub(crate) const ENGINE_ROOT_FIELDS: &[&str] = &[\n'
            '    "alpha",\n'
            '    "beta",\n'
            "];\n"
        )
        assert _parse_rust_str_slice(synthetic, "ENGINE_ROOT_FIELDS") == {
            "alpha",
            "beta",
        }

    def test_the_timeout_posture_is_inside_the_launcher_vocabulary(self) -> None:
        """The witness for the too-wide direction: this posture emits
        `model_timeout_secs` (the Fable row), so deleting that key from
        `ENGINE_ROOT_FIELDS` in `unknown.rs` turns this red — where the old
        hand-copy stayed green while every run refused at the launcher."""
        posture, _normalized, _digest = _benchmark_engine_posture(
            "openrouter/anthropic/claude-fable-5"
        )
        assert "model_timeout_secs" in posture, (
            "the Fable posture no longer emits a timeout — this witness needs "
            "a posture that exercises a key beyond the old 10-key copy"
        )
        assert set(posture) <= _engine_root_fields()
