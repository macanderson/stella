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

from arenabench.config import MatchTemplateError, match_from_toml
from arenabench.harbor_agent import arena_posture
from arenabench.model import Engine, ResponsibilityConfig

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


def _engine(body: str) -> Engine:
    return match_from_toml(tomllib.loads(HEADER + body)).contestants[0].engine


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

    def test_a_string_false_is_read_as_the_arm_it_spells(self):
        """`enabled = "false"` declines, where `bool("false")` would consent.

        The same misreading `declared_flag` exists to prevent for `bare_loop`
        (#2334), asserted here because this key has its own parse path.
        """
        engine = _engine(
            '[contestant.engine.responsibilities.triage]\nenabled = "false"\n'
        )
        assert engine.responsibilities["triage"].enabled is False
