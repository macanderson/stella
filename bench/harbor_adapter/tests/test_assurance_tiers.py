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
  `agents.default.model` > `default_model` (`AgentEngineConfig::model_for`). A
  declaration that resolves roles by any other rule is free to disagree with
  the run it describes, which is the whole defect class.

  That ladder used to carry a middle rung — the flat `pipeline_<role>_model` —
  and this module still reads it. The divergence is deliberate and bounded:
  #3908 collapsed the engine to the one role core has and **retired** the five
  flat keys rather than removing them, precisely because this harness and
  `arenabench` still write them into hashed postures, and refusing them would
  re-hash every digest registered in `bench/READINESS.md` §8.4. The Python
  stops writing them in slice 6 (#3910). Until then the two sides are
  knowingly out of step, so the mirror tests below pin the order of the rungs
  that *survive* and assert the extra rung is exactly the retired set — a
  named divergence rather than a drift nobody noticed.
"""

from __future__ import annotations

import asyncio
import json
import re
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
_BENCH_WORKFLOW = _REPO_ROOT / ".github" / "workflows" / "bench.yml"
_MODEL_FOR_DECL = "pub fn model_for("
_RETIRED_ROOT_DECL = "pub(crate) const RETIRED_ENGINE_ROOT: &[&str] = &["


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


def _model_for_body() -> str:
    """The body of `AgentEngineConfig::model_for`, brace to matching brace."""
    source = _model_for_source()
    start = source.index(_MODEL_FOR_DECL)
    return source[start : source.index("\n    }\n", start)]


def _retired_engine_root() -> set[str]:
    """The flat role keys the engine still *recognizes* after #3908.

    Parsed rather than copied: a list duplicated into this file is a second
    cell that drifts, and it would drift in exactly the direction that costs —
    this harness still writing a key the trusted launcher has begun refusing,
    discovered at launch with the posture already hashed.
    """
    holders = [
        f for f in _SETTINGS_MODULE if f.exists() and _RETIRED_ROOT_DECL in f.read_text()
    ]
    assert len(holders) == 1, (
        f"`RETIRED_ENGINE_ROOT` is declared in {len(holders)} settings files "
        f"({[str(f) for f in holders]}); this mirror needs exactly one to name "
        "the vocabulary the launcher still accepts"
    )
    source = holders[0].read_text()
    start = source.index(_RETIRED_ROOT_DECL) + len(_RETIRED_ROOT_DECL)
    return set(re.findall(r'"([^"]+)"', source[start : source.index("];", start)]))


def test_the_engine_still_resolves_a_role_in_the_order_this_module_mirrors() -> None:
    """Pin the mirror to its original: `AgentEngineConfig::model_for`.

    The declaration is only trustworthy while it resolves a role the way the
    engine does. Reordering `model_for` without reordering
    `resolve_posture_role_model` re-opens #2134 in a new place, and nothing
    else in either tree would notice — the two live in different languages,
    different test suites, and different CI jobs.

    Only the rungs both sides still share are pinned here. #3908 removed the
    engine's middle rung; that this module still reads it is asserted as a
    named, tracked divergence by the test below, rather than silently
    tolerated by weakening this one.
    """
    body = _model_for_body()
    rungs = [
        body.index(".and_then(|a| a.model"),
        body.index("self.default_model"),
    ]
    assert rungs == sorted(rungs), (
        "`model_for` no longer reads `agents.default.model` > `default_model`; "
        "`resolve_posture_role_model` mirrors that order and has to change "
        "with it (#2134)"
    )


def test_the_flat_role_keys_this_module_reads_are_retired_by_the_engine() -> None:
    """The one rung this mirror has and the engine does not is the retired set.

    #3908 collapsed `agent_engine_config` to the one role core has and dropped
    the flat `pipeline_<role>_model` keys out of `model_for`. It deliberately
    did not drop them out of the settings *vocabulary*: they are recognized,
    ignored and reported by name in `RETIRED_ENGINE_ROOT`, because this harness
    and `arenabench` still write them into postures whose digests are
    registered in `bench/READINESS.md` §8.4, and refusing them would re-hash
    every one of those — the published-numbers call #3870 reserves for a
    maintainer.

    So the divergence is legitimate exactly while both halves hold: the engine
    reads none of these keys, and it still recognizes all of them. Slice 6
    (#3910) closes it from this side. If either half moves first, this is where
    a maintainer finds out — rather than a benchmark launch being refused with
    the posture already hashed, or a rung growing back under a mirror that no
    longer checks it.
    """
    body = _model_for_body()
    flat_keys = set(_FLAT_ROLE_MODEL_KEY.values())

    read_again = sorted(key for key in flat_keys if key in body)
    assert not read_again, (
        f"`model_for` reads {read_again} again — the engine grew back a rung "
        "this module mirrors, so `resolve_posture_role_model`'s order is no "
        "longer a mirror of it but a guess (#2134)"
    )

    dropped = sorted(flat_keys - _retired_engine_root())
    assert not dropped, (
        f"{dropped} are still written into postures by this harness but are no "
        "longer named in `RETIRED_ENGINE_ROOT`, so the trusted-launcher seam "
        "refuses them and every digest in `bench/READINESS.md` §8.4 needs "
        "re-hashing; slice 6 (#3910) has to land on this side first"
    )


def test_the_bench_workflow_runs_when_the_settings_module_changes() -> None:
    """A ratchet that reads a file must be triggered by that file.

    `test_posture.py`'s catalog assertion, made for the second Rust path a
    bench suite reads. The two mirrors above parse
    `crates/stella-cli/src/settings/` — the engine's ladder and its retired-key
    vocabulary — and that directory was not named in `bench.yml`'s change
    filter, so a PR touching only it set `changed=false` and skipped the whole
    suite. #3908 was exactly that PR: green on its own branch, and red on
    `main` from the merge onward, because a push to `main` runs the suites
    unconditionally. The filter is a hand-written literal on both sides, so the
    two are compared here rather than trusted to stay in step.

    Matched against the filter *expression* rather than the file, deliberately:
    the whole file includes the paragraph of comment explaining the pattern, so
    a whole-file search goes green on prose alone — it would still pass for
    someone who wrote the rationale and deleted the alternative.
    """
    relative = (_SETTINGS_SRC / "settings").relative_to(_REPO_ROOT).as_posix()
    lines = _BENCH_WORKFLOW.read_text(encoding="utf-8").splitlines()
    filters = [line for line in lines if "grep -Eq" in line]
    assert len(filters) == 1, (
        f"expected exactly one `grep -Eq` change filter in "
        f"{_BENCH_WORKFLOW.name}, found {len(filters)}; this assertion can no "
        "longer name which one gates the suites"
    )
    # The filter is a POSIX ERE, so its literals carry escaped dots; compare
    # against the unescaped text.
    assert relative in filters[0].replace("\\", ""), (
        f"{_BENCH_WORKFLOW.name}'s change filter does not name {relative}, so "
        "a PR touching only the engine's settings module skips this suite — "
        "including the two mirror checks above, whose only input is that "
        "module."
    )
