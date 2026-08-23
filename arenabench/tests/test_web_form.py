# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The web form is an authoring surface, and it is checked like one.

#2411 removed per-trial ceilings from three authoring surfaces — the ``Engine``
type, the TOML loader, and the HTTP create path — and added a guard reading the
*text* of every committed template
(``test_matches.py::test_no_committed_template_declares_a_per_trial_ceiling``).
It left the fourth. ``ui/components/setup/seat-card.tsx`` kept rendering a
"Budget $/task" and a "Max output tok" input, and ``matchPayload`` shipped the
whole draft object to the server with ``{...seat.engine}``.

The server counted a cap key's *presence*, not its value, so a blank field was
enough: **every** launch from the web UI was refused, with an error naming keys
the operator could not see and had never set. The form was unusable from the
moment #2461 landed (2026-08-09) and nothing failed.

Removing the keys from the form fixed the source and not the operators: the
built bundle is generated and gitignored, so an arena served from any checkout
older than the last ``npm run build`` kept sending them. The server now drops a
declared ceiling and runs uncapped instead of refusing — the same executed
state, minus the wall. ``TestACeilingCannotBlockALaunch`` below is the witness.

Three lessons are pinned below. First, the cap keys must be absent from the
form's source, checked on the text the way the templates are. Second, a
declared ceiling must never be the thing that stops a launch. Third — the one
that generalises — the form's engine literal and ``Engine.from_json`` must name
the same fields, in both directions: a key the form sends that the engine does
not read is #2411 again, and a key the engine reads that the form does not send
is a Stella configuration an operator can only reach by hand-writing TOML,
which is what ``bare_loop`` and ``responsibilities`` were.

No Docker, no network, no model key: the sources and one in-process server.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from arenabench.harbor_agent import arena_posture
from arenabench.model import RESPONSIBILITIES, RESPONSIBILITY_AGENTS, Engine, MatchSpec
from arenabench.server import ArenaServer, _declared_caps, _uncapped

_UI = Path(__file__).resolve().parents[1] / "ui"
_SETUP_VIEW = _UI / "components" / "setup" / "setup-view.tsx"
_SEAT_CARD = _UI / "components" / "setup" / "seat-card.tsx"

#: Line prefixes that make a mention prose rather than code. The templates guard
#: skips ``#`` for the same reason: a file explaining the rule is not breaking
#: it, and every source below now carries such a sentence.
_COMMENT_PREFIXES = ("//", "/*", "*", "*/", "{/*")


def _is_comment(line: str) -> bool:
    return line.lstrip().startswith(_COMMENT_PREFIXES)


def _ui_sources() -> list[Path]:
    files = sorted(
        path
        for pattern in ("*.ts", "*.tsx")
        for path in _UI.rglob(pattern)
        if "node_modules" not in path.parts
    )
    assert files, f"no UI sources found under {_UI} — this guard would pass vacuously"
    return files


#: The two functions in ``setup-view.tsx`` that write an ``engine`` object
#: literal, and what each one is. Both are checked, because they answer
#: different questions and only the second one reaches the server.
_ENGINE_LITERALS = {
    "defaultSeat": "the draft a new seat starts as",
    "matchPayload": "the object the launch POSTs",
}


def _engine_literal_keys(source: str, *, within: str) -> set[str]:
    """Keys of the ``engine: {…}`` literal inside function ``within``.

    Read off the source rather than restated here, because a restatement is
    another copy of the same list and would agree with the form only until
    somebody edited one of them.

    Anchored on the function name, and that anchor is the whole point: written
    to search the file from the top it silently read ``defaultSeat``'s literal
    while its name and docstring said ``matchPayload``, and would then have
    passed on a payload builder that still spread the raw draft — the exact
    defect it exists to catch.
    """
    anchor = source.index(f"function {within}(")
    start = source.index("engine: {", anchor)
    depth = 0
    keys: set[str] = set()
    for line in source[start:].splitlines():
        depth += line.count("{") - line.count("}")
        # `name: value,` and the shorthand `name,` — both are keys on the wire.
        if match := re.match(r"\s*([A-Za-z_][A-Za-z0-9_]*)\s*[:,]", line):
            if match.group(1) != "engine":
                keys.add(match.group(1))
        if depth == 0:
            break
    assert keys, (
        f"{within}'s engine literal names no keys — it is a spread of some other "
        "object, so nothing here can audit what it sends"
    )
    return keys


def _authoring_engine_keys() -> set[str]:
    """Engine root keys an operator may declare, derived from the type itself.

    ``qualified_model`` is excluded because it is a computed property on the way
    *out* — ``from_json`` reads ``api`` and ``model`` and never it — so a form
    sending one would be stating a value the engine recomputes.
    """
    return set(Engine().to_json()) - {"qualified_model"}


class TestNoPerTrialCeilingInTheForm:
    """The regression this repository shipped, checked where it shipped it.

    A live binding is what breaks a launch; a comment naming the key is the file
    explaining why it has none.
    """

    def test_no_ui_source_names_a_per_trial_ceiling(self) -> None:
        offenders = [
            f"{path.relative_to(_UI)}:{number}: {line.strip()}"
            for path in _ui_sources()
            for number, line in enumerate(
                path.read_text(encoding="utf-8").splitlines(), start=1
            )
            if not _is_comment(line)
            and ("budget_usd" in line or "max_tokens" in line)
        ]
        assert not offenders, (
            "the web form declares a per-trial ceiling the server will drop, so "
            "the page would offer a control that changes nothing about the run: "
            f"{offenders}"
        )

    def test_the_payload_the_form_sends_declares_no_ceiling(self) -> None:
        """The shape the form now builds, read by the launch path's own reader.

        The old form put ``budget_usd: ""`` in this object and `_declared_caps`
        counted it, so this is the assertion that would have caught it — not a
        paraphrase of the reader, the reader.
        """
        sent = _engine_literal_keys(
            _SETUP_VIEW.read_text(encoding="utf-8"), within="matchPayload"
        )
        payload = {
            "contestants": [
                {
                    "id": "stella",
                    "name": "Stella",
                    "agent": "stella",
                    "engine": dict.fromkeys(sorted(sent), ""),
                }
            ]
        }
        assert _declared_caps(payload) == []


class TestACeilingCannotBlockALaunch:
    """The second half of #2411's fix, and the one it was missing.

    Refusing the key put a knob *nothing in this codebase can honour* on the
    critical path of every launch. The built web bundle is generated and
    gitignored, so an operator running an arena from a checkout newer than
    their last ``npm run build`` kept POSTing ``budget_usd: ""`` from a form
    whose source no longer had the field — and got an error naming two keys
    they could neither see nor edit, on every launch, with no way through it.

    The measurement argument is untouched: the run is still uncapped, because
    `Engine` has nowhere to put a ceiling. What changes is that reaching that
    state is the server's job rather than the operator's.
    """

    def _payload(self, engine: dict) -> dict:
        return {
            "name": "m",
            "dataset": "terminal-bench-2.1",
            "contestants": [
                {
                    "id": "stella",
                    "name": "Stella",
                    "agent": "stella",
                    "engine": {"api": "openrouter", "model": "z-ai/glm-5.2", **engine},
                    "env": {"required": ["OPENROUTER_API_KEY"]},
                }
            ],
        }

    @pytest.mark.parametrize(
        "engine",
        [
            {"budget_usd": ""},
            {"budget_usd": 1.0, "max_tokens": 64000},
            {"roles": {"worker": {"max_tokens": 64000}}},
        ],
        ids=["blank-as-the-stale-bundle-sends-it", "engine-level", "role-level"],
    )
    def test_a_declared_ceiling_is_not_what_stops_the_launch(
        self,
        engine: dict,
        tmp_path: Path,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        """Run without a credential, so creation still refuses — for the right
        reason. The old path raised on the ceiling before ever looking at the
        seat's credentials; this asserts the ceiling is no longer in the way,
        without needing Docker to prove it.
        """
        monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
        monkeypatch.setenv("ARENABENCH_CREDENTIALS", str(tmp_path / "none.env"))
        arena = ArenaServer(tmp_path / "ws")
        with pytest.raises(ValueError) as caught:
            arena.create_match(self._payload(engine))
        message = str(caught.value)
        # Anchored on the whole message rather than a substring search: the
        # temp path this test runs in carries the test's own name, which
        # contains the words a `not in` check would look for.
        assert message.startswith("required credentials are not set"), message

    def test_the_stripped_payload_carries_no_ceiling_onward(self) -> None:
        """What `MatchSpec.from_json` is handed, at both levels."""
        cleaned = _uncapped(
            self._payload(
                {
                    "budget_usd": 1.0,
                    "roles": {"worker": {"max_tokens": 64000, "effort": "high"}},
                }
            )
        )
        (seat,) = cleaned["contestants"]
        assert _declared_caps(cleaned) == []
        assert seat["engine"]["model"] == "z-ai/glm-5.2", "only the cap is removed"
        assert seat["engine"]["roles"]["worker"] == {"effort": "high"}


class TestEngineKeyParity:
    """The form and the engine name the same configuration, both ways.

    Scoped to the engine *root*. The nested ``roles`` and ``responsibilities``
    tables have their own readers and their own tests
    (``test_role_posture.py``, ``test_responsibilities.py``); what is checked
    here is the boundary where the browser hands an object to Python.

    Both literals are checked. ``defaultSeat``'s is what carried the blank cap
    fields, and ``matchPayload``'s is what put them on the wire — the second is
    the one that broke launches, and a form can fail either way round.
    """

    @pytest.mark.parametrize("within", sorted(_ENGINE_LITERALS))
    def test_the_form_names_no_key_the_engine_does_not_read(self, within: str) -> None:
        sent = _engine_literal_keys(_SETUP_VIEW.read_text(encoding="utf-8"), within=within)
        extra = sent - _authoring_engine_keys()
        assert not extra, (
            f"{within} ({_ENGINE_LITERALS[within]}) names {sorted(extra)}, which "
            "Engine.from_json ignores — either the engine grew a field and did "
            "not, or this is #2411 again"
        )

    @pytest.mark.parametrize("within", sorted(_ENGINE_LITERALS))
    def test_the_form_offers_every_key_the_engine_reads(self, within: str) -> None:
        """The other direction, and the reason this class exists.

        ``bare_loop`` (#2308) and ``responsibilities`` (#2381) both shipped on
        the engine and never reached the form, so the only way to ablate a stage
        was to hand-write an ``arenabench.toml`` — and a template that declared
        one, loaded into the wizard, came back out with it silently dropped.
        """
        sent = _engine_literal_keys(_SETUP_VIEW.read_text(encoding="utf-8"), within=within)
        missing = _authoring_engine_keys() - sent
        assert not missing, (
            f"the engine reads {sorted(missing)} and {within} "
            f"({_ENGINE_LITERALS[within]}) does not — a Stella configuration "
            "reachable only by hand-writing TOML"
        )


class TestTheVocabularyIsServed:
    """Every list the form renders one control per comes from the server.

    Hardcoding either list in TypeScript is the drift ``check-role-names.sh``
    was written for (#1449): the engine refuses an unknown roster key at
    validation time, inside the container, after the image is built — so a form
    offering a name Python does not know charges a container build for a typo,
    and reports it as the seat failing.
    """

    @pytest.fixture
    def server(self, tmp_path: Path) -> ArenaServer:
        return ArenaServer(tmp_path / "workspace")

    def test_the_agents_endpoint_serves_the_roster_vocabulary(
        self, server: ArenaServer
    ) -> None:
        payload = server.agents()
        assert payload["responsibilities"] == list(RESPONSIBILITIES)
        assert payload["responsibility_agents"] == list(RESPONSIBILITY_AGENTS)


class TestTheFormsRosterReachesTheSeat:
    """An ablation configured in the browser survives to the posture.

    The chain the form's payload takes: JSON → ``MatchSpec`` → the seat's frozen
    posture. Each hop has dropped this declaration before (see
    ``test_responsibilities.py``), and a drop is invisible — the arm runs,
    scores, and publishes under a posture naming an ablation that never
    happened.
    """

    def _spec(self, engine: dict) -> MatchSpec:
        return MatchSpec.from_json(
            {
                "id": "form",
                "dataset": "terminal-bench-2.1",
                "contestants": [
                    {
                        "id": "stella",
                        "name": "Stella",
                        "agent": "stella",
                        "engine": {
                            "api": "openrouter",
                            "model": "z-ai/glm-5.2",
                            **engine,
                        },
                    }
                ],
            }
        )

    def test_an_ablation_set_in_the_form_reaches_the_posture(self) -> None:
        # `enabled: false` is what the form's "off" sends; "inherit" sends no
        # key at all, which is a different arm.
        engine = self._spec(
            {"responsibilities": {"verdict": {"enabled": False}}}
        ).contestants[0].engine
        posture, _, _ = arena_posture("openrouter/z-ai/glm-5.2", engine)
        assert posture["responsibilities"] == {"verdict": {"enabled": False}}

    def test_a_reassignment_set_in_the_form_reaches_the_posture(self) -> None:
        engine = self._spec(
            {"responsibilities": {"witness_author": {"agent": "verifier"}}}
        ).contestants[0].engine
        posture, _, _ = arena_posture("openrouter/z-ai/glm-5.2", engine)
        assert posture["responsibilities"] == {"witness_author": {"agent": "verifier"}}

    def test_the_bare_loop_toggle_reaches_the_seat(self) -> None:
        assert self._spec({"bare_loop": True}).contestants[0].engine.bare_loop is True
        assert self._spec({}).contestants[0].engine.bare_loop is False

    def test_an_untouched_form_declares_nothing(self) -> None:
        """The default seat must stay byte-identical to what it was.

        The posture is what a trial's digest covers, so a form that sent
        ``responsibilities: {}`` unconditionally would still be fine — the empty
        table emits no key — but a form that sent ``{"triage": {}}`` for every
        listed responsibility would re-hash every arm that ablates nothing and
        invalidate comparison against every run already recorded.
        """
        engine = self._spec({"responsibilities": {}, "bare_loop": False}).contestants[0].engine
        posture, _, _ = arena_posture("openrouter/z-ai/glm-5.2", engine)
        assert "responsibilities" not in posture


class TestRoleOverridesAreInertOnEveryArm:
    """The card must not tell an operator a role override will be honoured.

    ``bare_loop`` used to select between two loops, so the card greyed the role
    controls out for the arms that had opted into the bare one and left them
    live for everybody else. Stella's staged pipeline was deleted from its
    workspace (stella#3865): ``stella run --pipeline classic`` is refused, every
    remaining resolution lands on the raw step loop, and the adapter therefore
    publishes ``stella_loop_mode`` as ``bare_step_loop`` on every trial
    (stella#4023, corrected on the Python side in stella#4412). A live control
    on an undeclared arm is an offer this bench cannot keep (stella#4413).
    """

    def test_no_control_is_gated_on_a_seats_bare_loop_declaration(self) -> None:
        source = _SEAT_CARD.read_text(encoding="utf-8")
        mentions = [
            (number, line)
            for number, line in enumerate(source.splitlines(), start=1)
            if not _is_comment(line) and "bare_loop" in line
        ]
        assert mentions, (
            "the card names no `bare_loop` at all — the declaration is still an "
            "authoring field, so this guard would now pass vacuously"
        )
        offenders = [
            f"seat-card.tsx:{number}: {line.strip()}"
            for number, line in mentions
            if "disabled" in line or "Inert" in line or "pipelineInert" in line
        ]
        assert not offenders, (
            "role overrides are inert on every arm since stella#3865, so the "
            "card must not derive their inertness from `bare_loop` — an arm "
            "that declared nothing would read as one whose overrides apply:\n"
            + "\n".join(offenders)
        )

    def test_the_card_says_the_overrides_do_not_apply(self) -> None:
        """The other half: disabled controls with no reason are a puzzle."""
        live = "\n".join(
            line
            for line in _SEAT_CARD.read_text(encoding="utf-8").splitlines()
            if not _is_comment(line)
        )
        assert "raw step loop" in live, (
            "the card disables every role control and must say why, naming the "
            "loop that actually runs"
        )
