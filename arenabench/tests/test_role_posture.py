# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Per-role posture: what a seat may pin, and what its silence must not say.

A sibling of `test_arena.py`'s `TestPosture` rather than more lines in it —
that file reached the repository's 1500-line ceiling — and the split is along a
real seam: everything here is about the posture's *role vocabulary*, which is
the one part of the seat schema that has to stay in lockstep with four other
languages (`scripts/check-role-names.sh`).

The undeclared case is the one worth reading twice. The posture is what the
digest covers, so a row emitted for a role nobody configured would silently
re-hash every already-registered arm and make two runs of the same
configuration incomparable.
"""

from __future__ import annotations

from arenabench.harbor_agent import arena_posture
from arenabench.model import Engine, RoleConfig


class TestResearchAndPlanRoles:
    def test_a_seat_can_turn_research_down_without_touching_the_worker(self):
        """**The witness for #2374.**

        On match ``7d025330abad`` the posture that reached the container had
        four rows and research was not one of them, so fifteen research calls
        ran at the worker's ``xhigh`` and spent 76 seconds emitting a few
        hundred reasoning tokens. There was no seat key that could say
        otherwise. This is that key.
        """
        engine = Engine(
            model="z-ai/glm-5.2",
            effort="xhigh",
            roles={
                "research": RoleConfig(effort="low", reasoning=False),
                "plan": RoleConfig(effort="medium"),
            },
        )
        posture, _, _ = arena_posture("openrouter/z-ai/glm-5.2", engine)
        assert posture["agents"]["research"] == {"effort": "low", "reasoning": "off"}
        assert posture["agents"]["plan"]["effort"] == "medium"
        assert posture["agents"]["worker"] == {"effort": "xhigh", "reasoning": "on"}

    def test_a_research_model_lands_on_the_flat_key_the_engine_reads(self):
        engine = Engine(
            model="z-ai/glm-5.2", roles={"research": RoleConfig(model="z-ai/glm-5.2-air")}
        )
        posture, _, _ = arena_posture("openrouter/z-ai/glm-5.2", engine)
        assert posture["pipeline_research_model"] == "openrouter/z-ai/glm-5.2-air"
        assert "openrouter/z-ai/glm-5.2-air" in posture["allowed_models"]

    def test_an_undeclared_research_role_emits_no_row_at_all(self):
        """Silence must stay silence, in the posture and therefore in the
        digest.

        Emitting ``{"effort": "xhigh"}`` for a role the seat never mentioned
        would state a choice nobody made AND re-hash every already-registered
        arm, making two runs of the same configuration incomparable. The
        engine inherits the worker for these two on its own.
        """
        engine = Engine(model="m", effort="xhigh")
        posture, _, digest = arena_posture("openrouter/m", engine)
        assert "research" not in posture["agents"]
        assert "plan" not in posture["agents"]
        assert "pipeline_research_model" not in posture
        # Byte-identical to a posture built from an engine that has no notion
        # of these roles at all — the property that keeps old arms comparable.
        assert set(posture["agents"]) == {"default", "worker", "verifier", "triage"}
        _, _, again = arena_posture("openrouter/m", Engine(model="m", effort="xhigh"))
        assert digest == again
