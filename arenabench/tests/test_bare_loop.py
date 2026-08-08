# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The bare-loop arm: declared on the engine, and never quietly assumed.

Every test here guards one hop of the same chain — match file → ``Engine``
→ seat environment → adapter argv → published manifest — because the defect
this arm was born from was not a crash. An exported ``STELLA_NO_PIPELINE``
was scrubbed by ``runner._base_environment`` before Harbor started, so the
run took the staged pipeline while the match file, the launcher, the runner
log and the seat label all said bare loop. Nothing reported it.

So the bias is toward the values that produce a *plausible* arm rather than
an error: a word spelled to decline that reads as consent, a selector on a
seat that has no pipeline to switch off, a default that drifts. A benchmark
that crashes gets fixed; one that silently measured the other arm gets
published.

Its own module rather than more length on ``test_arena.py``: that file sits
one edit from the repository's 1500-line limit, and the rule is that
separable new code becomes its own module. Its adapter-side counterpart is
``bench/harbor_adapter/tests/test_loop_mode.py``.

No Docker, no network, no model key: every fixture is synthetic.
"""

from __future__ import annotations

import pytest

from arenabench.model import MatchSpec


class TestBareLoopArm:
    def test_bare_loop_survives_json_and_toml_and_reaches_the_seat_env(self):
        """A bare-loop arm must survive every launch path and reach the seat.

        The runner scrubs every ambient ``STELLA_*`` so a host export cannot
        stand in for a seat's declared configuration, which means an exported
        ``STELLA_NO_PIPELINE`` silently ran the pipeline instead — the arm
        looked configured and was not. Declaring it on the engine is what makes
        it travel with the match. Witness: field absent → each hop drops it and
        the seat env carries no selector.
        """
        import tomllib

        from arenabench.config import dump_match, match_from_toml

        spec = MatchSpec.from_json(
            {
                "dataset": "terminal-bench-2.1",
                "contestants": [
                    {
                        "name": "s",
                        "agent": "stella",
                        "engine": {"model": "m", "bare_loop": True},
                    },
                ],
            }
        )
        assert spec.contestants[0].engine.bare_loop is True
        rehydrated = MatchSpec.from_json(spec.to_json())
        assert rehydrated.contestants[0].engine.bare_loop is True
        parsed = match_from_toml(tomllib.loads(dump_match(spec)))
        assert parsed.contestants[0].engine.bare_loop is True
        # The label has to say so: the roles are inert on a bare-loop arm, and
        # a label naming only them advertises stages that never ran.
        assert "bare loop" in parsed.contestants[0].engine.label

        # Default stays off, and stays absent from a rendered template, so
        # every existing match renders back unchanged.
        plain = MatchSpec.from_json(
            {
                "dataset": "terminal-bench-2.1",
                "contestants": [
                    {"name": "s", "agent": "stella", "engine": {"model": "m"}},
                ],
            }
        )
        assert plain.contestants[0].engine.bare_loop is False
        assert "bare_loop" not in dump_match(plain)

    def test_a_word_spelled_to_decline_never_selects_the_bare_loop(self):
        """`bool("false")` is True, and that answer would run the wrong arm.

        The JSON side is fed by round-tripped machine output, so an
        undeclarable value is corruption and falls back to the closed default
        rather than selecting an arm nobody asked for.
        """
        for spelled_off in ("false", "False", "no", "off", "0", "", "  "):
            spec = MatchSpec.from_json(
                {
                    "dataset": "terminal-bench-2.1",
                    "contestants": [
                        {
                            "name": "s",
                            "agent": "stella",
                            "engine": {"model": "m", "bare_loop": spelled_off},
                        },
                    ],
                }
            )
            assert spec.contestants[0].engine.bare_loop is False, spelled_off

        # Neither arm, and nothing to guess from: closed.
        for undeclarable in ("maybe", 2, [], {}, "1.0"):
            spec = MatchSpec.from_json(
                {
                    "dataset": "terminal-bench-2.1",
                    "contestants": [
                        {
                            "name": "s",
                            "agent": "stella",
                            "engine": {"model": "m", "bare_loop": undeclarable},
                        },
                    ],
                }
            )
            assert spec.contestants[0].engine.bare_loop is False, undeclarable

        # Every spelling of yes still opens it, so strictness costs nothing a
        # match author would notice.
        for spelled_on in (True, 1, "true", "TRUE", " yes ", "on"):
            spec = MatchSpec.from_json(
                {
                    "dataset": "terminal-bench-2.1",
                    "contestants": [
                        {
                            "name": "s",
                            "agent": "stella",
                            "engine": {"model": "m", "bare_loop": spelled_on},
                        },
                    ],
                }
            )
            assert spec.contestants[0].engine.bare_loop is True, spelled_on

    def test_a_toml_template_refuses_a_bare_loop_it_cannot_read(self):
        """The human-authored side refuses instead of defaulting.

        Someone wrote that word and meant something by it; running the other
        arm silently is the exact failure this whole selector exists to avoid.
        """
        from arenabench.config import MatchTemplateError, match_from_toml

        with pytest.raises(MatchTemplateError) as caught:
            match_from_toml(
                {
                    "dataset": "terminal-bench-2.1",
                    "contestant": [
                        {
                            "name": "s",
                            "agent": "stella",
                            "engine": {"model": "m", "bare_loop": "sometimes"},
                        },
                    ],
                }
            )
        assert "bare_loop" in str(caught.value)

    def test_a_non_stella_seat_may_not_declare_a_bare_loop(self):
        """A key that reads as effective and is not, refused at parse.

        Same refusal as `sut_ref` on a non-stella seat and for the same reason
        (#2082): the CLI flag never reaches an agent that has no pipeline, so
        a template declaring it would advertise an arm it never ran.
        """
        from arenabench.config import MatchTemplateError, match_from_toml

        with pytest.raises(MatchTemplateError) as caught:
            match_from_toml(
                {
                    "dataset": "terminal-bench-2.1",
                    "contestant": [
                        {
                            "name": "cc",
                            "agent": "claude-code",
                            "engine": {"model": "m", "bare_loop": True},
                        },
                    ],
                }
            )
        assert "bare_loop applies only to a stella seat" in str(caught.value)

