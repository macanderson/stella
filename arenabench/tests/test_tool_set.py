"""A match may declare which tools a seat is offered, and must record it.

The experiment #3032 asks for — a baseline with four tools, then one tool
added per arm — is only interpretable if two things hold. The declaration has
to survive every launch path and reach the seat's environment, and it has to
land in the match's published identity, or two arms that differ only in what
the agent could see become indistinguishable the moment the run is over.

The first test in each class is the one that protects the existing archive: a
match that declares no tool set must behave exactly as it did before this
field existed, down to the rendered template and the comparability key.
"""

from __future__ import annotations

import tomllib

import pytest

from arenabench import provenance
from arenabench.config import MatchTemplateError, dump_match, match_from_toml
from arenabench.model import MatchSpec

_BASELINE = ["task_list", "save_state", "get_state", "get_environment"]


def _spec(engine: dict, agent: str = "stella") -> MatchSpec:
    return MatchSpec.from_json(
        {
            "dataset": "terminal-bench-2.1",
            "contestants": [
                {"name": "s", "agent": agent, "engine": {"model": "m", **engine}}
            ],
        }
    )


class TestDeclaration:
    def test_an_undeclared_match_is_unchanged_in_every_hop(self) -> None:
        """The promise to every measurement taken before this field existed."""
        plain = _spec({})
        assert plain.contestants[0].engine.tool_set == ()
        assert MatchSpec.from_json(plain.to_json()).contestants[0].engine.tool_set == ()
        rendered = dump_match(plain)
        assert "tool_set" not in rendered
        assert match_from_toml(tomllib.loads(rendered)).contestants[0].engine.tool_set == ()

    def test_a_declared_set_survives_json_and_toml_and_keeps_its_order(self) -> None:
        spec = _spec({"tool_set": _BASELINE})
        assert list(spec.contestants[0].engine.tool_set) == _BASELINE
        rehydrated = MatchSpec.from_json(spec.to_json())
        assert list(rehydrated.contestants[0].engine.tool_set) == _BASELINE
        parsed = match_from_toml(tomllib.loads(dump_match(spec)))
        assert list(parsed.contestants[0].engine.tool_set) == _BASELINE

    def test_the_label_says_the_arm_was_restricted(self) -> None:
        # An arm offered four tools and one offered the catalog are different
        # apparatus; a label naming only the model presents them as one.
        assert "4 tools" in _spec({"tool_set": _BASELINE}).contestants[0].engine.label
        assert "tools" not in _spec({}).contestants[0].engine.label


class TestRefusals:
    def test_a_preset_word_is_refused_rather_than_resolved(self) -> None:
        # `STELLA_LEAN_TOOLS=1` is valid to Stella and selects a set nobody
        # wrote down. A published arm names its tools.
        with pytest.raises(MatchTemplateError, match="list of tool names"):
            match_from_toml(_toml('tool_set = "1"'))

    def test_an_entry_that_is_not_tool_shaped_is_refused(self) -> None:
        with pytest.raises(MatchTemplateError, match="not tool-shaped"):
            match_from_toml(_toml('tool_set = ["task_list", "save state"]'))

    def test_an_empty_list_is_refused_rather_than_read_as_the_catalog(self) -> None:
        # `[]` reads as "no tools", which is not a runnable arm. Omitting the
        # key is how a template declines the field, and the two must not be
        # spelled the same way.
        with pytest.raises(MatchTemplateError, match="omit the key"):
            match_from_toml(_toml("tool_set = []"))

    def test_a_non_stella_seat_may_not_declare_a_tool_set(self) -> None:
        # Refused rather than ignored, like `sut_ref` and `bare_loop`: only the
        # Stella adapter forwards the declaration, so anywhere else it is a
        # template claiming a restriction the arm never ran.
        with pytest.raises(MatchTemplateError, match="only to a stella seat"):
            match_from_toml(_toml('tool_set = ["task_list"]', agent="claude-code"))

    def test_the_same_refusal_holds_on_the_validate_path(self) -> None:
        problems = _spec({"tool_set": ["task_list"]}, agent="claude-code").validate()
        assert any("tool_set applies only to a stella seat" in p for p in problems)


class TestProvenance:
    def test_an_unrestricted_record_keeps_its_exact_key_and_says_nothing(self) -> None:
        """Append-only, so the whole archive still groups against itself."""
        record = provenance.Provenance(dataset_key="tb", dataset_digest="sha256:abcd1234")
        assert "tools" not in record.comparability_key
        assert "tool_set_labels" not in record.to_json()
        assert record.restricted_tool_sets == ()

    def test_a_restricted_arm_carries_its_set_and_a_key_that_separates_it(self) -> None:
        record = provenance.Provenance(
            dataset_key="tb",
            dataset_digest="sha256:abcd1234",
            sut_seats={
                "lean": {"name": "lean", "ref": "main", "commit": "c" * 40, "sha256": ""},
                "full": {"name": "full", "ref": "main", "commit": "c" * 40, "sha256": ""},
            },
            tool_set_seats={"lean": list(_BASELINE)},
        )
        # The literal set is the record; the key is what stops the two arms
        # averaging into one number despite an identical SUT commit.
        assert record.to_json()["tool_set_seats"]["lean"] == _BASELINE
        lean_key = record.comparability_key_for("lean")
        full_key = record.comparability_key_for("full")
        assert lean_key != full_key
        assert lean_key.endswith(record.tool_set_label(_BASELINE))
        assert "tools4@" in lean_key
        assert "tools" not in full_key
        assert record.to_json()["tool_set_labels"] == {"lean": record.tool_set_label(_BASELINE)}

    def test_the_label_is_a_set_not_a_sequence(self) -> None:
        # Two templates listing the same tools in a different order ran the
        # same apparatus; a key that separated them would split one series.
        label = provenance.Provenance.tool_set_label
        assert label(["task_list", "save_state"]) == label(["save_state", "task_list"])
        assert label(["task_list", "save_state"]) != label(["task_list", "get_state"])

    def test_the_record_round_trips_through_json(self) -> None:
        record = provenance.Provenance(tool_set_seats={"lean": list(_BASELINE)})
        assert provenance.Provenance.from_json(record.to_json()).tool_set_seats == {
            "lean": _BASELINE
        }


class TestSeatEnvironment:
    """The hop the declaration exists for: template → the seat's environment.

    The runner scrubs every ambient ``STELLA_*``, so an operator's exported
    ``STELLA_LEAN_TOOLS`` cannot stand in for a seat's declaration — which is
    exactly why the declaration has to travel with the match.
    """

    def _env(self, spec: MatchSpec, tmp_path) -> dict:
        from arenabench.runner import ContestantRun, MatchRunner

        seat = spec.contestants[0]
        runner = MatchRunner.__new__(MatchRunner)
        run = ContestantRun(
            contestant=seat,
            job_name="job",
            job_dir=tmp_path / "job",
            log_path=tmp_path / "job.log",
        )
        return runner._agent_environment(spec, seat, run)

    def test_an_undeclared_match_exports_nothing(self, tmp_path) -> None:
        assert "STELLA_LEAN_TOOLS" not in self._env(_spec({}), tmp_path)

    def test_a_declared_set_reaches_the_seat_as_a_closed_set(self, tmp_path) -> None:
        # `only:` and not the bare list — the bare form adds the discovery
        # tools on top, and a four-tool arm that advertises seven is not the
        # arm the template declared or provenance records.
        env = self._env(_spec({"tool_set": _BASELINE}), tmp_path)
        assert env["STELLA_LEAN_TOOLS"] == "only:" + ",".join(_BASELINE)


def _toml(engine_line: str, *, agent: str = "stella") -> dict:
    return tomllib.loads(
        "\n".join(
            [
                "[match]",
                'dataset = "terminal-bench-2.1"',
                "[[contestant]]",
                'name = "s"',
                f'agent = "{agent}"',
                "[contestant.engine]",
                'model = "m"',
                engine_line,
            ]
        )
    )
