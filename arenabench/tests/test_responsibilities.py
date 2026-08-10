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

import re
import tomllib
from pathlib import Path

import pytest

from arenabench.config import MatchTemplateError, dump_match, match_from_toml
from arenabench.harbor_agent import arena_posture
from arenabench.model import (
    RESPONSIBILITY_AGENTS,
    Engine,
    MatchSpec,
    ResponsibilityConfig,
)

_ROSTER_RS = (
    Path(__file__).resolve().parents[2]
    / "crates"
    / "stella-pipeline"
    / "src"
    / "roster.rs"
)

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

    The list an authoring surface offers as reassignment targets is Stella's
    `AgentId::BUILTIN`, and ArenaBench derives it as `ROLES` minus `default`
    rather than keeping a fourth hand-maintained copy of a Rust vocabulary —
    the drift `scripts/check-role-names.sh` exists for (#1449).

    The premise, that Stella's bindable agents are exactly its configurable
    non-default roles, is an *inference*. It holds today, and this reads the
    Rust source to say so out loud: an unresolvable name is a
    `RosterError::UnknownAgent` raised inside the container after the image is
    built, so a select offering one spends a container build on a typo and
    reports it as the seat failing.
    """

    def test_the_derived_list_is_stellas_builtin_agent_set(self) -> None:
        source = _ROSTER_RS.read_text(encoding="utf-8")
        match = re.search(
            r"BUILTIN:\s*&'static\s*\[&'static\s*str\]\s*=\s*&\[(?P<names>[^\]]*)\]",
            source,
        )
        assert match, f"could not find AgentId::BUILTIN in {_ROSTER_RS}"
        builtin = tuple(re.findall(r'"([^"]+)"', match.group("names")))
        assert builtin, "AgentId::BUILTIN parsed to zero names"
        assert set(RESPONSIBILITY_AGENTS) == set(builtin), (
            "the reassignment targets ArenaBench offers and the agents Stella "
            f"resolves have diverged: {sorted(RESPONSIBILITY_AGENTS)} vs "
            f"{sorted(builtin)}"
        )

    def test_the_interactive_default_is_not_a_reassignment_target(self) -> None:
        """`default` is the step loop and owns no pipeline responsibility.

        It is the one member of `ROLES` the derivation drops, so its absence is
        the derivation's whole content and is asserted rather than assumed.
        """
        assert "default" not in RESPONSIBILITY_AGENTS
