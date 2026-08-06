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

from arenabench.credentials import (
    apply_saved_credentials,
    credentials_path,
    load_credentials,
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
