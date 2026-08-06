# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The saved credential set: what it fills in, and what it must never override.

``apply_saved_credentials`` exists so an operator does not have to re-export
the same provider keys before every ``arenabench run``. The property that
makes that safe is precedence — a seat's own environment always wins — so
every witness here proves an override, not just a fill.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from arenabench.config import MatchTemplateError, match_from_toml, required_env
from arenabench.credentials import (
    apply_ambient_credentials,
    apply_saved_credentials,
    credentials_path,
    load_credentials,
    missing_required_credentials,
)
from arenabench.model import Contestant, Engine, MatchSpec


def _spec(*contestants: Contestant) -> MatchSpec:
    return MatchSpec(
        id="m",
        name="m",
        dataset="terminal-bench-2.1",
        tasks=(),
        contestants=tuple(contestants),
    )


class TestCredentialsPath:
    def test_defaults_under_the_arenabench_home(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        monkeypatch.delenv("ARENABENCH_CREDENTIALS", raising=False)
        monkeypatch.setenv("ARENABENCH_HOME", str(tmp_path))
        assert credentials_path() == tmp_path / "credentials.env"

    def test_explicit_override_wins(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        monkeypatch.setenv("ARENABENCH_HOME", str(tmp_path))
        monkeypatch.setenv("ARENABENCH_CREDENTIALS", str(tmp_path / "elsewhere.env"))
        assert credentials_path() == tmp_path / "elsewhere.env"


class TestLoadCredentials:
    def test_missing_file_is_not_an_error(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        monkeypatch.setenv("ARENABENCH_CREDENTIALS", str(tmp_path / "nope.env"))
        assert load_credentials() == {}

    def test_parses_the_same_dotenv_format_as_a_pasted_env(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        path = tmp_path / "credentials.env"
        path.write_text(
            'export ANTHROPIC_API_KEY=sk-ant-1\nOPENROUTER_API_KEY="sk-or-2"\n# a comment\n\n',
            encoding="utf-8",
        )
        monkeypatch.setenv("ARENABENCH_CREDENTIALS", str(path))
        assert load_credentials() == {
            "ANTHROPIC_API_KEY": "sk-ant-1",
            "OPENROUTER_API_KEY": "sk-or-2",
        }

    def test_screens_out_names_that_are_not_credential_shaped(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        path = tmp_path / "credentials.env"
        path.write_text(
            "ANTHROPIC_API_KEY=sk-ant-1\nPATH=/usr/bin\nPYTHONPATH=/tmp/evil\n",
            encoding="utf-8",
        )
        monkeypatch.setenv("ARENABENCH_CREDENTIALS", str(path))
        assert load_credentials() == {"ANTHROPIC_API_KEY": "sk-ant-1"}


class TestApplySavedCredentials:
    def test_fills_a_seat_that_has_nothing(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        path = tmp_path / "credentials.env"
        path.write_text("OPENROUTER_API_KEY=sk-or-1\n", encoding="utf-8")
        monkeypatch.setenv("ARENABENCH_CREDENTIALS", str(path))
        seat = Contestant(
            id="a", name="a", agent="stella", engine=Engine(api="openrouter", model="m")
        )
        (seeded,) = apply_saved_credentials(_spec(seat)).contestants
        assert seeded.env == {"OPENROUTER_API_KEY": "sk-or-1"}

    def test_the_seats_own_env_overrides_the_saved_file(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        path = tmp_path / "credentials.env"
        path.write_text("OPENROUTER_API_KEY=from-file\n", encoding="utf-8")
        monkeypatch.setenv("ARENABENCH_CREDENTIALS", str(path))
        seat = Contestant(
            id="a",
            name="a",
            agent="stella",
            engine=Engine(api="openrouter", model="m"),
            env={"OPENROUTER_API_KEY": "from-run"},
        )
        (seeded,) = apply_saved_credentials(_spec(seat)).contestants
        assert seeded.env == {"OPENROUTER_API_KEY": "from-run"}

    def test_never_leaks_a_key_the_seat_did_not_ask_for(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        path = tmp_path / "credentials.env"
        path.write_text(
            "OPENROUTER_API_KEY=sk-or-1\nANTHROPIC_API_KEY=sk-ant-1\n", encoding="utf-8"
        )
        monkeypatch.setenv("ARENABENCH_CREDENTIALS", str(path))
        # This seat is on Anthropic; it must never see the OpenRouter key.
        seat = Contestant(
            id="a", name="a", agent="stella", engine=Engine(api="anthropic", model="m")
        )
        (seeded,) = apply_saved_credentials(_spec(seat)).contestants
        assert seeded.env == {"ANTHROPIC_API_KEY": "sk-ant-1"}

    def test_no_saved_file_is_a_no_op(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        monkeypatch.setenv("ARENABENCH_CREDENTIALS", str(tmp_path / "nope.env"))
        seat = Contestant(
            id="a", name="a", agent="stella", engine=Engine(api="openrouter", model="m")
        )
        spec = _spec(seat)
        assert apply_saved_credentials(spec) is spec


def _template(env: dict | None, agent: str = "stella", api: str = "openrouter") -> dict:
    contestant: dict = {
        "id": "seat1",
        "agent": agent,
        "engine": {"api": api, "model": "z-ai/glm-5.2"},
    }
    if env is not None:
        contestant["env"] = env
    return {
        "match": {"name": "t", "dataset": "terminal-bench-2.1"},
        "contestant": [contestant],
    }


class TestDeclaredRequiredEnv:
    """A template's ``[contestant.env] required = [...]`` is a contract (#1777).

    The block used to wire nothing: the declaration parsed as a variable
    literally named ``required``, ``screen_env`` dropped it, and the seat
    launched with an empty environment — an operational abort scored as an
    agent loss. Every witness here proves the declaration is honoured or the
    launch is refused, never that an unauthenticated arm runs.
    """

    def test_a_template_declaration_is_the_seats_whole_contract(self) -> None:
        spec = match_from_toml(_template({"required": ["OPENROUTER_API_KEY"]}))
        (seat,) = spec.contestants
        assert seat.required_env == ("OPENROUTER_API_KEY",)
        # Declared beats derived: the list is exactly what the template said,
        # not the provider superset — declaring only one name is how a match
        # keeps an unintended credential out of a seat on purpose.
        assert required_env(spec) == {"seat1": ["OPENROUTER_API_KEY"]}

    def test_a_declared_name_present_in_the_environment_credentials_the_seat(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("OPENROUTER_API_KEY", "sk-or-ambient")
        spec = match_from_toml(_template({"required": ["OPENROUTER_API_KEY"]}))
        (seat,) = apply_ambient_credentials(spec).contestants
        assert seat.env == {"OPENROUTER_API_KEY": "sk-or-ambient"}
        assert missing_required_credentials(apply_ambient_credentials(spec)) == {}

    def test_the_seats_own_env_wins_over_the_ambient_value(self) -> None:
        seat = Contestant(
            id="a",
            name="a",
            agent="stella",
            engine=Engine(api="openrouter", model="m"),
            env={"OPENROUTER_API_KEY": "from-seat"},
            required_env=("OPENROUTER_API_KEY",),
        )
        (seeded,) = apply_ambient_credentials(
            _spec(seat), environ={"OPENROUTER_API_KEY": "from-host"}
        ).contestants
        assert seeded.env == {"OPENROUTER_API_KEY": "from-seat"}

    def test_a_seat_that_declared_nothing_gets_nothing_from_ambient(self) -> None:
        """The server-side scrub stands: ambient credentials reach a seat only
        through an explicit declaration, never implicitly."""
        seat = Contestant(
            id="a", name="a", agent="stella", engine=Engine(api="openrouter", model="m")
        )
        spec = _spec(seat)
        assert apply_ambient_credentials(
            spec, environ={"OPENROUTER_API_KEY": "sk-or"}
        ) is spec

    def test_the_json_required_key_is_a_declaration_not_a_variable(self) -> None:
        seat = Contestant.from_json(
            {
                "name": "s",
                "agent": "stella",
                "engine": {"api": "openrouter", "model": "m"},
                "env": {"required": ["OPENROUTER_API_KEY"]},
            }
        )
        assert seat.required_env == ("OPENROUTER_API_KEY",)
        assert seat.env == {}
        assert "required" not in seat.ignored_env

    def test_the_dotenv_and_plain_dict_forms_still_carry_values(self) -> None:
        pasted = Contestant.from_json(
            {"name": "s", "engine": {"model": "m"}, "env": "OPENROUTER_API_KEY=sk-1"}
        )
        assert pasted.env == {"OPENROUTER_API_KEY": "sk-1"}
        assert pasted.required_env == ()
        mapped = Contestant.from_json(
            {"name": "s", "engine": {"model": "m"}, "env": {"ZAI_API_KEY": "zk"}}
        )
        assert mapped.env == {"ZAI_API_KEY": "zk"}

    def test_a_non_credential_shaped_json_declaration_is_reported(self) -> None:
        """`required = ["PATH"]` over HTTP must never resolve the host's PATH
        into a seat — the name lands in `ignored_env`, visibly."""
        seat = Contestant.from_json(
            {
                "name": "s",
                "engine": {"model": "m"},
                "env": {"required": ["PATH", "OPENROUTER_API_KEY"]},
            }
        )
        assert seat.required_env == ("OPENROUTER_API_KEY",)
        assert "PATH" in seat.ignored_env

    def test_the_saved_file_fills_only_declared_names(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        """A subscription-only seat stays subscription-only: a metered key in
        the saved file must not credential a seat that declared it does not
        want one."""
        path = tmp_path / "credentials.env"
        path.write_text("ANTHROPIC_API_KEY=sk-ant-metered\n", encoding="utf-8")
        monkeypatch.setenv("ARENABENCH_CREDENTIALS", str(path))
        seat = Contestant(
            id="cc",
            name="cc",
            agent="claude-code",
            engine=Engine(api="anthropic", model="claude-fable-5"),
            required_env=("CLAUDE_CODE_OAUTH_TOKEN",),
        )
        (seeded,) = apply_saved_credentials(_spec(seat)).contestants
        assert seeded.env == {}

    def test_a_value_in_the_env_table_fails_validation(self) -> None:
        with pytest.raises(MatchTemplateError) as excinfo:
            match_from_toml(_template({"OPENROUTER_API_KEY": "sk-secret"}))
        assert any("names only, never" in problem for problem in excinfo.value.problems)

    def test_a_non_credential_shaped_declaration_fails_validation(self) -> None:
        with pytest.raises(MatchTemplateError) as excinfo:
            match_from_toml(_template({"required": ["PATH"]}))
        assert any("'PATH'" in problem for problem in excinfo.value.problems)

    def test_match_creation_refuses_a_seat_with_none_of_its_declared_names(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
    ) -> None:
        """The loud half of the contract: `POST /api/matches` must fail before
        any container starts, naming the missing variables — never launch an
        arm whose 401s score as an agent loss."""
        from arenabench.server import ArenaServer

        monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
        monkeypatch.setenv("ARENABENCH_CREDENTIALS", str(tmp_path / "none.env"))
        arena = ArenaServer(tmp_path / "ws")
        payload = {
            "name": "m",
            "dataset": "terminal-bench-2.1",
            "contestants": [
                {
                    "name": "stella",
                    "agent": "stella",
                    "engine": {"api": "openrouter", "model": "z-ai/glm-5.2"},
                    "env": {"required": ["OPENROUTER_API_KEY"]},
                }
            ],
        }
        with pytest.raises(ValueError) as excinfo:
            arena.create_match(payload)
        assert "OPENROUTER_API_KEY" in str(excinfo.value)
        assert arena.runner.matches == {}, "a refused match must not exist"

    def test_every_committed_template_declares_its_credentials(self) -> None:
        """The issue's repro, inverted: loading a committed template must keep
        the declared names, not drop them into an empty seat env."""
        from arenabench.config import load_match

        matches = Path(__file__).resolve().parents[1] / "matches"
        templates = sorted(matches.glob("*.toml"))
        assert templates, "the committed match templates have moved"
        for template in templates:
            spec = load_match(template)
            for seat in spec.contestants:
                assert seat.required_env, (
                    f"{template.name}: {seat.id} declares no credentials"
                )
