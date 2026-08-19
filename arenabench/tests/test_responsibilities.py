# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Single-stage ablation arms: declared per responsibility, never inferred.

``bare_loop`` is the blunt instrument — it settles triage, plan, witness and
verify at once, because all four ARE the pipeline — so an arm using it can
attribute a measured difference to "the pipeline" and to nothing smaller.
These declarations are the narrow one (#2381): triage off with everything
else running, or the verdict reassigned, so a paired run isolates one stage.

The bias here matches ``test_bare_loop.py``'s, for the same reason: the
failure that matters is not a crash but a *plausible* arm — a template that
spells an ablation, silently does not get one, and publishes a number
described by the wrong posture. So every test below asserts either that the
declaration reaches the seat or that a bad one is refused outright.

No Docker, no network, no model key: every fixture is synthetic.
"""

from __future__ import annotations

import tomllib

import pytest

from arenabench.config import MatchTemplateError, dump_match, match_from_toml
from arenabench.harbor_agent import arena_posture
from arenabench.model import (
    RESPONSIBILITY_AGENTS,
    ROLES,
    Engine,
    MatchSpec,
    ResponsibilityConfig,
)

#: Stella's normative home for the role vocabulary, relative to a checkout.
#: ``scripts/check-role-names.sh`` names this same function as "the truth" and
#: reads its match arms the same way, so the two halves of the contract are
#: anchored to one file rather than to each other.
_ROLE_KEY_RS = Path("crates") / "stella-cli" / "src" / "config_wiring.rs"

#: One match arm of ``role_key()``: ``EngineAgentKind::Verifier => "verifier",``
_ROLE_ARM = re.compile(r'EngineAgentKind::\w+\s*=>\s*"([a-z_]+)"')


def _role_key_names(source: str) -> frozenset[str]:
    """Every role spelling ``role_key()`` returns, read out of the Rust.

    Scoped to that function's body — a bare sweep for the arm shape would also
    collect every other ``match kind`` in the file, and those are legitimately
    partial (``model_source`` skips ``Default``). The body ends at the first
    column-zero ``}``, which is how ``check-role-names.sh``'s awk delimits it.
    """
    body = re.search(r"pub fn role_key\b.*?\n\}", source, re.DOTALL)
    if body is None:
        return frozenset()
    return frozenset(_ROLE_ARM.findall(body.group()))


HEADER = """
[match]
id = "ablation"
dataset = "terminal-bench-2.1"
[[contestant]]
name = "stella"
agent = "stella"
[contestant.engine]
api = "openrouter"
model = "z-ai/glm-5.2"
"""


def _spec(body: str) -> MatchSpec:
    return match_from_toml(tomllib.loads(HEADER + body))


def _engine(body: str) -> Engine:
    return _spec(body).contestants[0].engine


class TestDeclaration:
    def test_an_ablation_survives_toml_json_and_reaches_the_posture(self):
        """The full chain: match file → Engine → round-trip → seat posture.

        Each hop is a place the declaration has historically been dropped, and
        a drop is invisible: the arm runs, scores, and publishes under a
        posture naming an ablation that never happened.
        """
        engine = _engine(
            "[contestant.engine.responsibilities.triage]\nenabled = false\n"
        )
        assert engine.responsibilities["triage"].enabled is False

        # Round-tripped through JSON, which is how the runner hands the engine
        # to the seat (`ARENABENCH_ENGINE_JSON`).
        assert Engine.from_json(engine.to_json()).responsibilities == engine.responsibilities

        posture, _, _ = arena_posture("openrouter/z-ai/glm-5.2", engine)
        assert posture["responsibilities"] == {"triage": {"enabled": False}}

    def test_a_reassignment_travels_separately_from_an_ablation(self):
        """The two axes are independent and must not collapse into each other."""
        engine = _engine(
            "[contestant.engine.responsibilities.witness_author]\nagent = \"triage\"\n"
        )
        posture, _, _ = arena_posture("openrouter/z-ai/glm-5.2", engine)
        assert posture["responsibilities"] == {"witness_author": {"agent": "triage"}}

    def test_only_the_fields_the_arm_set_are_recorded(self):
        """A posture must not claim a pin the operator never made.

        Writing `enabled: true` beside a reassignment would record an
        assertion about a second axis, and the digest that covers the posture
        would then differ from an identically-configured arm that spelled it
        the other way.
        """
        engine = _engine(
            "[contestant.engine.responsibilities.verdict]\nagent = \"worker\"\n"
        )
        posture, _, _ = arena_posture("openrouter/z-ai/glm-5.2", engine)
        assert "enabled" not in posture["responsibilities"]["verdict"]


class TestDefaultArmIsUntouched:
    def test_an_arm_declaring_nothing_emits_no_key_and_keeps_its_digest(self):
        """The shipped pipeline must be byte-identical to what it was.

        The posture is what the trial's digest covers, so an unconditional
        `responsibilities: {}` would change the recorded identity of every arm
        that ablates nothing — invalidating comparison against every run
        recorded before this key existed.
        """
        plain = Engine(api="openrouter", model="z-ai/glm-5.2")
        posture, _, digest = arena_posture("openrouter/z-ai/glm-5.2", plain)
        assert "responsibilities" not in posture

        ablated = Engine(
            api="openrouter",
            model="z-ai/glm-5.2",
            responsibilities={"triage": ResponsibilityConfig(enabled=False)},
        )
        _, _, ablated_digest = arena_posture("openrouter/z-ai/glm-5.2", ablated)
        assert digest != ablated_digest, "an ablation must change the arm's identity"


class TestRefusals:
    """A bad declaration must fail the template, never degrade to the default.

    This is the whole reason the key is parsed strictly: the outcome a
    selector must never have is to run the arm it was spelled to decline.
    """

    @pytest.mark.parametrize(
        ("body", "fragment"),
        [
            pytest.param(
                '[contestant.engine.responsibilities.triarge]\nenabled = false\n',
                "not an ablatable responsibility",
                id="typo'd responsibility",
            ),
            pytest.param(
                '[contestant.engine.responsibilities.reflection]\nenabled = false\n',
                "not an ablatable responsibility",
                id="real call role the pipeline does not issue",
            ),
            pytest.param(
                '[contestant.engine.responsibilities.triage]\nenabled = "maybe"\n',
                "declares neither arm",
                id="non-boolean enabled",
            ),
            pytest.param(
                "responsibilities = 3\n",
                "must be a table",
                id="scalar where a table belongs",
            ),
        ],
    )
    def test_a_bad_declaration_refuses_the_template(self, body: str, fragment: str):
        with pytest.raises(MatchTemplateError) as caught:
            _engine(body)
        assert fragment in str(caught.value)

    def test_a_roster_on_a_comparator_seat_refuses_the_template(self):
        """A declaration that reaches nothing must fail the file.

        `bare_loop` has been refused on a non-stella seat since #2308, three
        lines above this key's own check, and for exactly the reason that
        applies here: nothing reads it, so the template says the arm ablated a
        stage, the arm runs whole, and the published number describes neither.
        The asymmetry was an oversight, not a policy.
        """
        template = {
            "match": {"id": "ablation", "dataset": "terminal-bench-2.1"},
            "contestant": [
                {
                    "name": "claude code",
                    "agent": "claude-code",
                    "engine": {
                        "api": "anthropic",
                        "model": "claude-sonnet-5",
                        "responsibilities": {"triage": {"enabled": False}},
                    },
                }
            ],
        }
        with pytest.raises(MatchTemplateError) as caught:
            match_from_toml(template)
        assert "applies only to a stella seat" in str(caught.value)

    def test_a_string_false_is_read_as_the_arm_it_spells(self):
        """`enabled = "false"` declines, where `bool("false")` would consent.

        The same misreading `declared_flag` exists to prevent for `bare_loop`
        (#2334), asserted here because this key has its own parse path.
        """
        engine = _engine(
            '[contestant.engine.responsibilities.triage]\nenabled = "false"\n'
        )
        assert engine.responsibilities["triage"].enabled is False


class TestTheDownloadPathKeepsIt:
    """"Download this match" must hand back the arm that ran, roster included.

    `dump_match` emitted `api`, `model`, `reasoning`, `effort`, `base_url`,
    `bare_loop` and `roles` — and not `responsibilities`. So the one path whose
    entire purpose is reproducibility silently dropped the ablation: the .toml
    described the full pipeline, and `arenabench run it.toml` reran a different
    experiment than the one whose number was published.

    Latent while the roster could only be set by hand-editing TOML, and live the
    moment the web form could set it.
    """

    @pytest.mark.parametrize(
        "body",
        [
            pytest.param(
                "[contestant.engine.responsibilities.triage]\nenabled = false\n",
                id="ablation",
            ),
            # The other axis. `enabled` and `agent` are written independently, so
            # a renderer can carry one and drop the other.
            pytest.param(
                '[contestant.engine.responsibilities.verdict]\nagent = "worker"\n',
                id="reassignment",
            ),
            pytest.param(
                "[contestant.engine.responsibilities.witness_author]\n"
                'enabled = true\nagent = "verifier"\n',
                id="both axes at once",
            ),
        ],
    )
    def test_a_roster_survives_render_and_reparse(self, body: str):
        spec = _spec(body)
        reparsed = match_from_toml(tomllib.loads(dump_match(spec)))
        assert (
            reparsed.contestants[0].engine.responsibilities
            == spec.contestants[0].engine.responsibilities
        )

    def test_an_arm_that_ablates_nothing_renders_no_roster_block(self):
        """Emitted only when set, so every existing template renders back
        byte-identical and a saved match cannot gain a roster it never had."""
        assert "responsibilities" not in dump_match(_spec(""))


class TestReassignmentTargets:
    """`RESPONSIBILITY_AGENTS` is derived, so the derivation gets asserted.

    ArenaBench derives the list an authoring surface offers as reassignment
    targets from `ROLES` minus `default`, rather than keeping a fourth
    hand-maintained copy of a Rust vocabulary — the drift
    `scripts/check-role-names.sh` exists for (#1449).

    `test_the_derived_list_is_stellas_builtin_agent_set` used to sit here and
    cross-check that derivation against `AgentId::BUILTIN`, read out of
    `crates/stella-pipeline/src/roster.rs`. **That crate was deleted from the
    workspace in #3865 and `AgentId::BUILTIN` does not exist anywhere any
    more**, so the assertion could no longer pass — it raised
    `FileNotFoundError` on a path that is gone, which is what turned
    `harbor_adapter + analyzer pytest` red on `main` (#3901 predicted exactly
    this: the Python bench surfaces wanted checking for *live breakage*, not
    assuming to be prose).

    It is removed rather than repointed, for two reasons worth stating so the
    next reader does not restore it:

    1. **The property it protected is still enforced, by a gate step.**
       `scripts/check-role-names.sh` holds `ROLES` in `model.py` to
       `role_key()` in `crates/stella-cli/src/config_wiring.rs` — a normative
       home that still exists — across all four producers. Repointing this
       test at the same source would be a fourth copy of a check that is
       already green, which is the duplication #1449 argued against.
    2. **Re-creating a `BUILTIN` list to satisfy a bench test would be
       actively wrong.** `doc:roleless-core` (epic #3903) is removing role
       names from the engine on purpose: core is to know one name, `default`.
       A bench assertion demanding that core publish a roster of the others
       would pull against that directive.

    The sibling below still asserts the half that needs no Rust file: the
    derivation's whole content is that `default` is dropped.
    """

    def test_the_interactive_default_is_not_a_reassignment_target(self) -> None:
        """`default` is the step loop and owns no pipeline responsibility.

        It is the one member of `ROLES` the derivation drops, so its absence is
        the derivation's whole content and is asserted rather than assumed.
        """
        assert "default" not in RESPONSIBILITY_AGENTS
