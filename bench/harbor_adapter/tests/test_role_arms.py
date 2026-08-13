"""The read-only roles as selectable, hashed arms (#2549).

Companion to `test_posture.py`, split from it for the reason that file was split
from `test_adapter.py`: the file-size gate treats a separable concern as its own
module rather than as more length on one already near its ceiling.

The subject. A Terminal-Bench claim run pins `agents.worker.effort` — typically
`xhigh` — and `research` and `plan` carried no row at all. They do not inherit
`agents.default`: `AgentEngineAgents::get` is a straight per-role lookup with no
fallback, and `over_worker` (`crates/stella-cli/src/agent/engine.rs`) merges each
role's own tuning over **the worker's**, field by field. So the arm's worker tier
reached both roles, and on match `7d025330abad` research spent 76s across 15
calls to emit a few hundred reasoning tokens. #2553 gave the engine the knob;
this harness could not reach it.

The constraint, and why every test below is really one test. Emitting the two
rows unconditionally would have re-hashed every registered arm — the posture
digest identifies the SUT (`bench/READINESS.md` §8.4.4) — to describe postures
that behave exactly as the recorded ones already did. A rename did that once
(#1394) and ended cross-boundary comparison. So the rows are **opt-in**, and the
first test here is the one that matters: unset, the builder returns the byte
for byte identical posture it always did.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor import (  # noqa: E402 - after importorskip by design
    _HOST_ONLY_STELLA_ENV,
    _benchmark_engine_posture,
)
from stella_harbor.posture import (  # noqa: E402 - after importorskip by design
    _ADMISSIBLE_ROLE_EFFORTS,
    _PLAN_EFFORT_ENV,
    _PLAN_MODEL_ENV,
    _PLAN_REASONING_ENV,
    _RESEARCH_EFFORT_ENV,
    _RESEARCH_MODEL_ENV,
    _RESEARCH_REASONING_ENV,
    POSTURE_SELECTOR_ENV,
    read_posture_selectors,
    resolve_posture_role_model,
    resolve_role_effort,
    resolve_role_reasoning,
)

_MODEL = "openrouter/anthropic/claude-sonnet-5"
_CHEAP = "openrouter/anthropic/claude-haiku-4.5"

_REPO_ROOT = Path(__file__).resolve().parents[3]
_ANALYSIS_DIR = _REPO_ROOT / "bench" / "terminal_bench_analysis"


def _posture_schema():
    """The study-manifest posture schema, imported from the analysis package.

    A path literal and a `sys.path` insert rather than a dependency, for the
    reason `test_posture.py` reads `unknown.rs` off a literal path: the two
    packages install separately and neither depends on the other, but the
    contract between them is real and nothing else checks it. A crate or
    directory move is fixed here, and the assertion says so.
    """
    if not _ANALYSIS_DIR.is_dir():
        raise AssertionError(
            f"cannot find the analysis package at {_ANALYSIS_DIR}. That path is "
            "a literal in this file, not a resolved location — if the package "
            "moved, update `_ANALYSIS_DIR` to match."
        )
    if str(_ANALYSIS_DIR) not in sys.path:
        sys.path.insert(0, str(_ANALYSIS_DIR))
    import tb21_posture_schema

    return tb21_posture_schema


class TestUnsetReproducesTheRegisteredPosture:
    """The load-bearing half: carrying this code changes no recorded digest."""

    def test_unset_selectors_emit_neither_row_nor_flat_key(self) -> None:
        """Absence is the encoding of "inherit", exactly as for every arm above.

        Writing `agents.research = {"effort": "xhigh", "reasoning": "on"}` — the
        values the role already resolves to — would change every hash in the
        tree to describe a posture identical to the one they already described.
        """
        posture, _normalized, _digest = _benchmark_engine_posture(_MODEL)
        assert set(posture["agents"]) == {"default", "worker", "verifier", "triage"}
        assert "pipeline_research_model" not in posture
        assert "pipeline_plan_model" not in posture

    def test_the_registered_digest_is_byte_identical(self) -> None:
        """The pinned digest, asserted from this side too.

        `test_adapter.py` pins it as the registered SUT's identity. It is pinned
        again here because this module is where a change would come from, and a
        digest that moves on an unselected arm is not a test failure to be
        re-blessed — it is every registered arm re-hashed.
        """
        _posture, _normalized, digest = _benchmark_engine_posture(
            "openrouter/deepseek/deepseek-v4-pro"
        )
        assert digest == (
            "1191911e2bc5aeef2949f0592a0c3d2da31f55e05b464018a8440febe0e94bd3"
        )


class TestRoleArmsReachThePosture:
    """What a selected arm puts in the posture, and what it leaves alone."""

    def test_effort_alone_moves_effort_and_inherits_the_workers_reasoning(
        self,
    ) -> None:
        """A one-knob arm must be a one-variable change.

        The row is emitted whole — both fields, like every other role — so the
        manifest schema can keep validating an exact shape. The half nobody
        selected therefore has to carry the value the role *already* resolved
        to, or materialising the row would itself be a second variable.
        """
        base, _base_json, base_digest = _benchmark_engine_posture(_MODEL)
        armed, _armed_json, armed_digest = _benchmark_engine_posture(
            _MODEL, research_effort="low"
        )
        assert armed["agents"]["research"] == {"effort": "low", "reasoning": "on"}
        assert "plan" not in armed["agents"], "plan is a separate arm"
        assert armed_digest != base_digest
        for role in ("default", "worker", "verifier", "triage"):
            assert armed["agents"][role] == base["agents"][role]

    def test_reasoning_alone_moves_reasoning_and_inherits_the_selected_tier(
        self,
    ) -> None:
        """The inherited half tracks the *arm's* worker, not the frozen literal.

        Turning effort down while `reasoning` still rides the worker's `on`
        leaves the role thinking on every call, which on a lookup that reports
        what it read is most of the cost. So the two are separately selectable —
        and the effort this fills in must be the worker effort this run chose.
        """
        posture, _normalized, _digest = _benchmark_engine_posture(
            _MODEL, worker_effort="high", research_reasoning=False
        )
        assert posture["agents"]["research"] == {"effort": "high", "reasoning": "off"}

    def test_both_roles_arm_independently(self) -> None:
        """Research and plan are two arms, not one switch.

        Plan writes the work order every later stage is judged against — the one
        short call where thinking is the product — so an arm that turns research
        down must be able to leave plan alone.
        """
        posture, _normalized, _digest = _benchmark_engine_posture(
            _MODEL,
            research_effort="low",
            research_reasoning=False,
            plan_effort="medium",
        )
        assert posture["agents"]["research"] == {"effort": "low", "reasoning": "off"}
        assert posture["agents"]["plan"] == {"effort": "medium", "reasoning": "on"}

    def test_a_model_pin_lands_in_the_flat_key_and_widens_the_vocabulary(
        self,
    ) -> None:
        """`allowed_models` is a whitelist; a pin outside it resolves to nothing.

        Same shape as the triage pin, and the same failure if the vocabulary is
        not widened with it: the pin is refused at resolve time, the role drops
        back to what it inherits, and the run bills the worker's model while the
        digest claims another.
        """
        posture, _normalized, _digest = _benchmark_engine_posture(
            _MODEL, research_model=_CHEAP, plan_model=_CHEAP
        )
        assert posture["pipeline_research_model"] == _CHEAP
        assert posture["pipeline_plan_model"] == _CHEAP
        assert _CHEAP in posture["allowed_models"]
        assert _MODEL in posture["allowed_models"]
        assert posture["allowed_models"].count(_CHEAP) == 1, "widened once, not twice"

    def test_a_model_pin_alone_emits_no_tuning_row(self) -> None:
        """Who runs the role and how hard it thinks are two questions.

        A pin says nothing about effort, so it must not synthesise a row — that
        would make every model arm silently a tuning arm as well.
        """
        posture, _normalized, _digest = _benchmark_engine_posture(
            _MODEL, research_model=_CHEAP
        )
        assert "research" not in posture["agents"]


class TestRoleArmsFailClosed:
    """A run scored under a configuration nobody chose is worse than no run."""

    def test_an_unrecognised_tier_is_refused_not_defaulted(self) -> None:
        """The refusal matters more here than for the worker, not less.

        The tiers this admits and the worker's do not overlap at the low end, so
        a typo that fell back to inheriting would produce a number measuring the
        exact configuration the arm was selected to move away from.
        """
        with pytest.raises(ValueError, match="research effort must be one of"):
            _benchmark_engine_posture(_MODEL, research_effort="lowish")
        with pytest.raises(ValueError, match="plan effort must be one of"):
            _benchmark_engine_posture(_MODEL, plan_effort="")

    def test_every_admissible_tier_is_accepted(self) -> None:
        """The vocabulary exists to catch a typo, not to pick an arm."""
        for tier in _ADMISSIBLE_ROLE_EFFORTS:
            posture, _normalized, _digest = _benchmark_engine_posture(
                _MODEL, research_effort=tier
            )
            assert posture["agents"]["research"]["effort"] == tier

    def test_reasoning_takes_the_settings_vocabulary_not_a_json_bool(self) -> None:
        """`on`/`off` — plus `1`/`0` for the shell that has an integer to hand.

        A bare `"false"` is deliberately not a second spelling of off: it is the
        shape a lost or mistyped value takes, and reading it as a selection is
        how a run gets attributed to an arm nobody chose.
        """
        assert resolve_role_reasoning("off", role="research") is False
        assert resolve_role_reasoning("0", role="research") is False
        assert resolve_role_reasoning("on", role="plan") is True
        assert resolve_role_reasoning(None, role="plan") is None
        with pytest.raises(ValueError, match="research reasoning must be one of"):
            resolve_role_reasoning("false", role="research")

    def test_unset_is_inherit_and_empty_is_a_refusal(self) -> None:
        """`None` never reaches the posture; an empty string never silently does."""
        assert resolve_role_effort(None, role="research") is None
        with pytest.raises(ValueError, match="research effort"):
            resolve_role_effort("   ", role="research")

    def test_a_pin_must_share_the_workers_provider(self) -> None:
        """One provider credential reaches a trial, over the anonymous FD.

        A pin on a second provider has nothing to authenticate with and would
        fail every call it is used for — refused here, before anything is spent.
        """
        for kwargs in (
            {"research_model": "anthropic/claude-haiku-4.5"},
            {"plan_model": "anthropic/claude-haiku-4.5"},
        ):
            role = next(iter(kwargs)).split("_", 1)[0]
            with pytest.raises(ValueError, match=f"{role} model must share"):
                _benchmark_engine_posture(_MODEL, **kwargs)


class TestTheArmSurvivesEverySeamItCrosses:
    """Three fail-closed boundaries stand between this dict and a running trial."""

    def test_every_selector_is_registered_host_only(self) -> None:
        """The ambient check fails closed: unregistered is a refused run.

        Not a tidiness assertion — an unlisted `STELLA_TURN_TIMEOUT` killed all
        ten trials of a run. `POSTURE_SELECTOR_ENV` exists so the reading list
        and the registration list cannot be two hand-kept copies, and this is
        the check that they are in fact the same list.
        """
        assert set(POSTURE_SELECTOR_ENV) <= _HOST_ONLY_STELLA_ENV
        for env in (
            _RESEARCH_EFFORT_ENV,
            _RESEARCH_REASONING_ENV,
            _RESEARCH_MODEL_ENV,
            _PLAN_EFFORT_ENV,
            _PLAN_REASONING_ENV,
            _PLAN_MODEL_ENV,
        ):
            assert env in POSTURE_SELECTOR_ENV

    def test_the_selectors_resolve_through_one_reader(self) -> None:
        """Collection and the exec boundary must resolve a selector identically.

        The exec boundary rebuilds the posture and refuses the run unless it
        matches byte for byte, so two spellings of "read the selectors" is a
        refused run waiting to happen. Both paths call `read_posture_selectors`.
        """
        selected = {
            _RESEARCH_EFFORT_ENV: "low",
            _RESEARCH_REASONING_ENV: "off",
            _PLAN_MODEL_ENV: _CHEAP,
        }
        kwargs = read_posture_selectors(selected.get)
        assert kwargs["research_effort"] == "low"
        assert kwargs["research_reasoning"] is False
        assert kwargs["plan_model"] == _CHEAP
        assert kwargs["plan_effort"] is None
        posture, _normalized, _digest = _benchmark_engine_posture(_MODEL, **kwargs)
        assert posture["agents"]["research"] == {"effort": "low", "reasoning": "off"}

    def test_an_armed_posture_stays_inside_the_launcher_vocabulary(self) -> None:
        """`trusted_engine_config_shape_is_strict` refuses the RUN on an unknown key.

        The seam fails closed rather than dropping what it does not recognise,
        so a posture naming a key or a role the launcher has never heard of does
        not degrade to the control arm — the trial dies at launch. Both
        vocabularies are parsed from `unknown.rs`, never copied.
        """
        # Imported rather than restated, inverting the call `test_posture.py`
        # makes for its one-line `_bare_agent` factory: these two parse Rust
        # source off a path literal, and a second copy of that is exactly the
        # hand-maintained duplicate they exist to replace.
        from tests.test_posture import _engine_agent_names, _engine_root_fields

        posture, _normalized, _digest = _benchmark_engine_posture(
            _MODEL,
            research_effort="low",
            research_reasoning=False,
            research_model=_CHEAP,
            plan_effort="medium",
            plan_model=_CHEAP,
        )
        assert set(posture) <= _engine_root_fields()
        assert set(posture["agents"]) <= _engine_agent_names()

    def test_the_manifest_schema_accepts_every_key_the_builder_can_emit(
        self,
    ) -> None:
        """The analysis refuses a manifest whose posture it cannot describe.

        `_validate_engine_posture_manifest_schema` demanded exact equality, so a
        selected arm's key was "extra" and its manifest was refused — and the
        required-key list had already fallen behind every optional key the
        builder grew. Derived from the builder rather than restated, because a
        restated list is the thing that fell behind.
        """
        schema = _posture_schema()
        armed, _normalized, _digest = _benchmark_engine_posture(
            _MODEL,
            verifier="openrouter/anthropic/claude-fable-5",
            triage_model=_CHEAP,
            research_effort="low",
            research_reasoning=False,
            research_model=_CHEAP,
            plan_effort="medium",
            plan_model=_CHEAP,
            max_revisions=4,
            candidates=2,
            verifier_evidence_demand=True,
            model_timeout_secs=1572,
        )
        permitted = (
            schema.STUDY_MANIFEST_ENGINE_POSTURE_FIELDS
            | schema.STUDY_MANIFEST_ENGINE_POSTURE_OPTIONAL_FIELDS
        )
        assert set(armed) <= permitted, "a selected arm's key is not 'extra'"
        assert set(armed) >= schema.STUDY_MANIFEST_ENGINE_POSTURE_FIELDS, (
            "the required keys are required of every arm"
        )
        permitted_roles = (
            schema.STUDY_MANIFEST_ENGINE_POSTURE_AGENT_ROLES
            | schema.STUDY_MANIFEST_ENGINE_POSTURE_OPTIONAL_AGENT_ROLES
        )
        assert set(armed["agents"]) <= permitted_roles
        # Optional means optional in BOTH directions: the unarmed posture must
        # still satisfy the required half on its own, or every historical
        # manifest is refused by the schema that claims to describe it.
        unarmed, _unarmed_json, _unarmed_digest = _benchmark_engine_posture(_MODEL)
        assert set(unarmed["agents"]) == (
            schema.STUDY_MANIFEST_ENGINE_POSTURE_AGENT_ROLES
        )
        # Every row the builder can emit is shaped exactly as the schema says.
        for role, row in armed["agents"].items():
            assert (
                set(row)
                == schema.STUDY_MANIFEST_ENGINE_POSTURE_AGENT_FIELDS_BY_ROLE[role]
            )


class TestUnpinnedTheseRolesInheritTheWorker:
    """The rung `model_for` does not have, and the resolver used to guess at."""

    def test_an_unpinned_role_resolves_to_the_worker_not_default_model(self) -> None:
        """`own_model_spec_for`'s split, mirrored rather than approximated.

        Triage and the verifier have a standing identity — unpinned, they still
        have a model of their own, and `default_model` is it. These two are
        documented as "run whatever the worker runs", so resolving them to
        `default_model` reports one model for a role that buys another the
        moment anything re-points the worker. In the frozen posture the two
        coincide, which is exactly why a resolver that got this wrong would keep
        agreeing with reality until the first arm that pinned the worker.
        """
        posture = {
            "default_model": _MODEL,
            "pipeline_worker_model": _CHEAP,
        }
        for role in ("research", "plan"):
            model, origin = resolve_posture_role_model(
                posture, role, default_model=_MODEL
            )
            assert (model, origin) == (_CHEAP, "pipeline_worker_model")
        # Triage keeps the standing-identity rung, and that contrast is the point.
        assert resolve_posture_role_model(posture, "triage", default_model=_MODEL) == (
            _MODEL,
            "default_model",
        )

    def test_a_pin_still_outranks_the_inheritance(self) -> None:
        """Both rungs above the fallback keep working, in the engine's order."""
        posture = {
            "default_model": _MODEL,
            "pipeline_worker_model": _CHEAP,
            "pipeline_research_model": _MODEL,
            "agents": {"plan": {"model": _CHEAP}},
        }
        assert resolve_posture_role_model(
            posture, "research", default_model=_MODEL
        ) == (_MODEL, "pipeline_research_model")
        assert resolve_posture_role_model(posture, "plan", default_model=_MODEL) == (
            _CHEAP,
            "agents.plan.model",
        )
