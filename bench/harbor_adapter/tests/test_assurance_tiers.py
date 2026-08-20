"""What the assurance-tier declaration reads, and what it says when the arm is off.

Companion to `test_posture.py`, split from it for the reason that file was split
from `test_adapter.py`: the file-size gate treats a separable concern as its own
module rather than as more length on an existing one.

The subject is #2134. Match `cc00894779ff` recorded, in the same `result.json`,
a posture naming `openrouter/moonshotai/kimi-k3` as the verifier and a
declaration saying `verifier_model: null`, `arm: witness-off`, with the reason
"every role inherits default_model" — a sentence the same file disproves. The
declaration was recomputed from the host-side `STELLA_WITNESS_AUTHOR_MODEL`
selector, which an ArenaBench roles config never touches: it reaches the engine
through the `_build_engine_posture` seam instead.

Two questions are covered here, and they are different:

- **Which channel is read.** The declaration derives from the resolved posture,
  so every channel into it — the host selector, a harness's overridden builder —
  is honored by construction rather than one at a time.
- **How a role resolves inside that posture.** The engine reads
  `agents.<role>.model` > the flat `pipeline_<role>_model` > `default_model`
  (`AgentEngineConfig::model_for`). A declaration that resolves roles by any
  other rule is free to disagree with the run it describes, which is the whole
  defect class.
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from harbor.models.agent.context import AgentContext  # noqa: E402

from stella_harbor import (  # noqa: E402 - after importorskip by design
    _ENGINE_CONFIG_ENV,
    _WITNESS_AUTHOR_ENV,
    StellaAgent,
    assurance_tiers_from_posture,
)
from stella_harbor.posture import (  # noqa: E402 - after importorskip by design
    _FLAT_ROLE_MODEL_KEY,
    resolve_posture_role_model,
)

# The three roles of match `cc00894779ff`, kept verbatim so the regression this
# file pins reads as the run it came from.
_WORKER = "openrouter/anthropic/claude-sonnet-5"
_VERIFIER = "openrouter/moonshotai/kimi-k3"
_TRIAGE = "openrouter/z-ai/glm-5.2"

# The sentence the broken declaration recorded. Asserted absent rather than
# merely "the new text is present": the failure #2134 is about is a *false*
# string reaching a forensic reader, so its disappearance is the fix.
_FALSE_OFF_REASON_FRAGMENT = "every role inherits default_model"


def _roles_posture(**overrides: Any) -> dict[str, Any]:
    """A posture in the shape `ArenaStellaAgent._build_engine_posture` emits.

    Flat role keys plus per-role posture entries that carry no `model` — which
    is exactly the arrangement that made the old selector-only reader blind.
    """
    posture: dict[str, Any] = {
        "default_model": _WORKER,
        "pipeline_verifier_model": _VERIFIER,
        "pipeline_triage_model": _TRIAGE,
        "allowed_models": [_WORKER, _VERIFIER, _TRIAGE],
        "auto_mode": "off",
        "effort_auto": "off",
        "reasoning_auto": "off",
        "headless_scope_bypass": "on",
        "agents": {
            "default": {"effort": "xhigh", "reasoning": "on"},
            "worker": {"effort": "xhigh", "reasoning": "on"},
            "verifier": {"effort": "xhigh", "reasoning": "on"},
            "triage": {"effort": "low", "reasoning": "off"},
        },
    }
    posture.update(overrides)
    return posture


def _bare_agent() -> StellaAgent:
    """A StellaAgent instance bypassing the Harbor base ``__init__``."""
    return StellaAgent.__new__(StellaAgent)


class _RolesConfigAgent(StellaAgent):
    """The `_build_engine_posture` seam, overridden the way ArenaBench does.

    A stand-in for `arenabench.harbor_agent.ArenaStellaAgent` rather than an
    import of it: the adapter is the thing under test and must not grow a test
    dependency on a package that lives outside this project.
    """

    posture_override: dict[str, Any] = {}

    def _build_engine_posture(
        self, model: str, *, verifier: str | None
    ) -> tuple[dict[str, Any], str, str]:
        posture = dict(self.posture_override)
        normalized = json.dumps(
            posture, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        )
        import hashlib

        return posture, normalized, hashlib.sha256(normalized.encode()).hexdigest()


def _run_trial(
    agent: StellaAgent, tmp_path: Path, events: list[dict[str, Any]]
) -> AgentContext:
    """Drive one trial to completion and return the context it populated."""
    (tmp_path / "stella-events.jsonl").write_text(
        "\n".join(json.dumps(event) for event in events)
    )
    agent.logs_dir = tmp_path
    agent.model_name = _WORKER
    agent._extra_env = {}
    agent._version = "stella 0.7.11"

    class _Environment:
        async def _stella_secure_exec_with_stdin(
            self, *, command: list[str], env: dict[str, str], stdin: bytes
        ):
            # The seam's posture is the one channel into the container, and the
            # declaration has to describe *that* dict — not a second opinion
            # computed beside it.
            assert json.loads(env[_ENGINE_CONFIG_ENV])["pipeline_verifier_model"] == (
                _VERIFIER
            )
            return SimpleNamespace(stdout=None, stderr=None, return_code=0)

    context = AgentContext()
    asyncio.run(
        type(agent).run.__wrapped__(agent, "Fix the task.", _Environment(), context)
    )
    return context


_COMPLETED_TRIAL = [
    {"type": "proof", "step": {"kind": "warrant", "required": True, "diff_lines": 30}},
    {"type": "complete", "status": "completed", "cost_usd": 0.42},
]


class TestRolesConfigReachesTheDeclaration:
    """#2134's headline: a verifier the host selector never saw."""

    def test_a_harness_roles_config_verifier_puts_the_witness_arm_on(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The witness test for #2134.

        A roles config sets `pipeline_verifier_model` through the posture seam
        and never touches `STELLA_WITNESS_AUTHOR_MODEL`. The declaration used to
        be recomputed from that selector, so this trial recorded `witness-off`,
        `verifier_model: null`, and an off-reason asserting facts about roles
        nothing had examined — for a run whose engine had an independent author
        the whole time.
        """
        monkeypatch.delenv("STELLA_SPEND_LIMIT", raising=False)
        monkeypatch.setenv("OPENROUTER_API_KEY", "openrouter-test-secret")
        monkeypatch.delenv(_WITNESS_AUTHOR_ENV, raising=False)

        agent = _RolesConfigAgent.__new__(_RolesConfigAgent)
        agent.posture_override = _roles_posture()
        context = _run_trial(agent, tmp_path, _COMPLETED_TRIAL)

        assert context.metadata["stella_assurance_arm"] == "witness-on"
        assert context.metadata["stella_verifier_model"] == _VERIFIER
        tiers = context.metadata["stella_assurance_tiers"]
        assert tiers["verifier_model"] == _VERIFIER
        assert tiers["worker_model"] == _WORKER
        assert tiers["tiers"]["authored_witness"] == "on"
        assert tiers["tiers"]["model_verdict"] == "on-independent-of-worker"
        assert tiers["authored_witness_off_reason"] is None

    def test_the_declaration_and_the_posture_name_the_same_verifier(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """One source, so the two metadata blocks cannot contradict each other.

        The contradiction is the artifact a bench forensic reads first: #2134
        was diagnosed by opening `stella_engine_posture` and
        `stella_assurance_tiers` side by side in one `result.json`.
        """
        monkeypatch.delenv("STELLA_SPEND_LIMIT", raising=False)
        monkeypatch.setenv("OPENROUTER_API_KEY", "openrouter-test-secret")
        monkeypatch.delenv(_WITNESS_AUTHOR_ENV, raising=False)

        agent = _RolesConfigAgent.__new__(_RolesConfigAgent)
        agent.posture_override = _roles_posture()
        context = _run_trial(agent, tmp_path, _COMPLETED_TRIAL)

        posture = context.metadata["stella_engine_posture"]
        tiers = context.metadata["stella_assurance_tiers"]
        assert posture["pipeline_verifier_model"] == tiers["verifier_model"]
        assert context.metadata["stella_verifier_model"] == tiers["verifier_model"]

    def test_an_outer_timeout_reconstructs_the_declaration_from_the_seam(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """A trial killed before `run()` built the posture still declares the arm.

        These are the trials most likely to be re-read, and the reconstruction
        used to call the base builder's inputs — which for a roles-config run
        describes a posture the run would never have used.
        """
        monkeypatch.delenv("STELLA_SPEND_LIMIT", raising=False)
        monkeypatch.setenv("OPENROUTER_API_KEY", "openrouter-test-secret")
        monkeypatch.delenv(_WITNESS_AUTHOR_ENV, raising=False)
        (tmp_path / "stella-events.jsonl").write_text(
            json.dumps(_COMPLETED_TRIAL[0]) + "\n"
        )

        agent = _RolesConfigAgent.__new__(_RolesConfigAgent)
        agent.posture_override = _roles_posture()
        agent.logs_dir = tmp_path
        agent.model_name = _WORKER
        agent._extra_env = {}
        agent._version = "stella 0.7.11"

        context = AgentContext()
        agent.populate_context_post_run(context)

        assert context.metadata["stella_assurance_arm"] == "witness-on"
        assert context.metadata["stella_verifier_model"] == _VERIFIER


class TestRoleResolutionMirrorsTheEngine:
    """`agents.<role>.model` > flat `pipeline_<role>_model` > `default_model`."""

    def test_the_agents_entry_outranks_the_flat_key_for_the_verifier(self) -> None:
        posture = _roles_posture()
        posture["agents"]["verifier"] = {"effort": "xhigh", "model": _WORKER}

        model, origin = resolve_posture_role_model(
            posture, "verifier", default_model=_WORKER
        )
        assert (model, origin) == (_WORKER, "agents.verifier.model")

        tiers, _json, _digest = assurance_tiers_from_posture(posture)
        # The flat key still names kimi-k3; the engine would never read it,
        # and neither may the declaration.
        assert tiers["arm"] == "witness-off"
        assert "agents.verifier.model" in tiers["authored_witness_off_reason"]

    def test_a_pinned_worker_leaves_the_verifier_independent_on_default_model(
        self,
    ) -> None:
        """ "Inherits" is not the same claim as "is the worker's model".

        With only the *worker* pinned, the verifier falls through to
        `default_model` — a different model, so the engine authors a witness.
        Reading the absence of `pipeline_verifier_model` as "same as the worker"
        turns that run's declaration into the same false negative #2134 filed.
        """
        posture = _roles_posture()
        del posture["pipeline_verifier_model"]
        posture["pipeline_worker_model"] = _TRIAGE

        tiers, _json, _digest = assurance_tiers_from_posture(posture)
        assert tiers["worker_model"] == _TRIAGE
        assert tiers["verifier_model"] == _WORKER
        assert tiers["arm"] == "witness-on"
        assert tiers["authored_witness_off_reason"] is None

    def test_both_roles_inheriting_one_default_is_the_control_arm(self) -> None:
        posture = _roles_posture()
        del posture["pipeline_verifier_model"]

        tiers, _json, _digest = assurance_tiers_from_posture(posture)
        assert tiers["arm"] == "witness-off"
        assert tiers["verifier_model"] is None
        assert tiers["worker_model"] == _WORKER
        assert tiers["tiers"]["model_verdict"] == "on-same-model-as-worker"

    def test_an_unparseable_role_model_is_refused_rather_than_guessed(self) -> None:
        """A malformed posture fails closed: a guess here IS the false metadata."""
        malformed: tuple[dict[str, Any], ...] = (
            {"pipeline_verifier_model": 7},
            {"agents": ["not", "a", "map"]},
        )
        for override in malformed:
            with pytest.raises(ValueError):
                assurance_tiers_from_posture(_roles_posture(**override))

        with pytest.raises(ValueError):
            posture = _roles_posture()
            posture["agents"]["verifier"] = {"model": "   "}
            assurance_tiers_from_posture(posture)


class TestTheOffReasonIsTrue:
    """Reason strings are load-bearing for bench forensics."""

    def test_the_off_reason_names_the_predicate_that_actually_failed(self) -> None:
        posture = _roles_posture(pipeline_verifier_model=_WORKER)

        tiers, _json, _digest = assurance_tiers_from_posture(posture)
        reason = tiers["authored_witness_off_reason"]
        assert _FALSE_OFF_REASON_FRAGMENT not in reason
        # The two models it compared, and the key each one came from — the
        # setting an operator would edit to restore the arm.
        assert reason.count(f"`{_WORKER}`") == 1
        assert "`pipeline_verifier_model`" in reason
        assert "`default_model`" in reason

    def test_the_off_reason_never_claims_a_role_it_did_not_read(self) -> None:
        """Triage carries a pin here, which is what made the old sentence false."""
        posture = _roles_posture(pipeline_verifier_model=_WORKER)
        assert posture["pipeline_triage_model"] == _TRIAGE

        tiers, _json, _digest = assurance_tiers_from_posture(posture)
        reason = tiers["authored_witness_off_reason"]
        assert _FALSE_OFF_REASON_FRAGMENT not in reason
        assert "triage" not in reason


_REPO_ROOT = Path(__file__).resolve().parents[3]
_SETTINGS_SRC = _REPO_ROOT / "crates" / "stella-cli" / "src"
_SETTINGS_MODULE = [
    _SETTINGS_SRC / "settings.rs",
    *sorted((_SETTINGS_SRC / "settings").glob("*.rs")),
]
_MODEL_FOR_DECL = "pub fn model_for("


def _model_for_source() -> str:
    """The one file in the `settings` module that declares `model_for`.

    Deliberately a search rather than a fixed path: splitting `settings.rs`
    into submodules moves the declaration without changing a line of it, and
    a hardcoded path turns that ordinary refactor into a red bench job
    (#3390 did exactly this, moving it to `settings/engine.rs`).
    """
    holders = [
        f for f in _SETTINGS_MODULE if f.exists() and _MODEL_FOR_DECL in f.read_text()
    ]
    assert holders, (
        "`AgentEngineConfig::model_for` is declared nowhere under "
        f"{_SETTINGS_SRC / 'settings'} — the posture module mirrors its "
        "fallback order and must follow the rename"
    )
    assert len(holders) == 1, (
        f"`{_MODEL_FOR_DECL}` is declared in more than one settings file "
        f"({[str(f) for f in holders]}); this mirror can no longer name which "
        "one the engine resolves through"
    )
    return holders[0].read_text()


def test_the_engine_still_resolves_a_model_in_the_order_this_module_mirrors() -> None:
    """Pin the mirror to its original: `AgentEngineConfig::model_for`.

    The declaration is only trustworthy while it resolves the way the engine
    does. Reordering `model_for` without reordering
    `resolve_posture_role_model` re-opens #2134 in a new place, and nothing
    else in either tree would notice — the two live in different languages,
    different test suites, and different CI jobs.

    **The engine has one role now.** `model_for` used to take a `kind` and
    read `agents.<kind>.model` > the flat `pipeline_<kind>_model` >
    `default_model`. #3908/#3944 (`doc:roleless-core` §6 slice 4) collapsed
    the six personas to the one role core actually has, so the signature lost
    its parameter and the middle rung went with it — four of those five flat
    keys were already inert before the collapse, read by nothing at all.

    What that leaves is asserted in two halves below, because
    ``resolve_posture_role_model`` still ranks the flat key and this test is
    the only thing that would notice the day that stops being deliberate:

    1. the two rungs the engine really has, in order;
    2. that the flat keys are *declared* retired rather than quietly dropped.

    Half 2 is what keeps this honest. The mirror's middle rung is dead, and
    `RETIRED_ENGINE_ROOT`'s own comment says the fail-closed tightening lands
    with **slice 6 (#3910), once the Python stops writing them** — so the
    harness writing them today is a tracked interim state, not drift. When
    #3910 lands, this assertion is what fails and sends the next reader to
    `resolve_posture_role_model`.
    """
    source = _model_for_source()
    start = source.index(_MODEL_FOR_DECL)
    body = source[start : source.index("\n    }\n", start)]
    rungs = [
        body.index("self.agent()"),
        body.index("self.default_model"),
    ]
    assert rungs == sorted(rungs), (
        "`model_for` no longer reads `agents.default.model` > `default_model`; "
        "`resolve_posture_role_model` mirrors that order and has to change "
        "with it (#2134)"
    )
    assert "pipeline_verifier_model" not in body, (
        "`model_for` reads a flat `pipeline_<role>_model` key again — if the "
        "per-role rung is back, `resolve_posture_role_model`'s middle rung is "
        "live again too and this test should assert its position, not its "
        "absence (#3908)"
    )

    retired = (_SETTINGS_SRC / "settings" / "unknown.rs").read_text()
    # Anchor on the declaration, not the first mention — the name appears in a
    # doc comment above it, and slicing from there reads the wrong span.
    decl = "const RETIRED_ENGINE_ROOT"
    assert decl in retired, (
        "the retired engine-root keys are no longer declared in "
        "`settings/unknown.rs`; `resolve_posture_role_model` still writes and "
        "ranks them, so their status has to be readable from the engine (#3910)"
    )
    declared = retired[retired.index(decl) :]
    declared = declared[: declared.index("];")]
    missing = [
        key for key in _FLAT_ROLE_MODEL_KEY.values() if f'"{key}"' not in declared
    ]
    assert not missing, (
        f"the harness writes {missing} but the engine no longer recognizes "
        "them even to report them — slice 6 (#3910) tightened this to "
        "fail-closed, so `resolve_posture_role_model` must stop writing the "
        "flat keys now"
    )
