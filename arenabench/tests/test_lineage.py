# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Lineage: the config surface, and the marker that must never come off.

A seeded match has agents that already saw its tasks. Every test here pins one
of the two halves that make that publishable rather than dangerous.

**Off costs nothing.** A match with no ``lineage`` must produce the identical
spec, the identical seat environment, the identical provenance JSON and the
identical comparability key it produced before this feature existed — because
every published number depends on that being true.

**On is impossible to hide.** The id reaches provenance, the provenance key,
the trends group key, and the TOML copy that goes to S3; and the one function
that could reduce several matches to one figure refuses a mixed set outright.
"""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from arenabench import provenance
from arenabench.config import (
    MatchTemplateError,
    dump_match,
    match_from_toml,
    match_to_toml_dict,
)
from arenabench.model import Contestant, MatchSpec
from arenabench.runner import LINEAGE_ENV, ContestantRun, MatchRunner
from arenabench.series import match_row


def _toml_dict(**match: object) -> dict:
    return {
        "match": {
            "name": "m",
            "dataset": "terminal-bench-2.1",
            "tasks": ["fix-git"],
            **match,
        },
        "contestant": [
            {
                "id": "a",
                "name": "stella",
                "agent": "stella",
                "engine": {"model": "anthropic/claude-sonnet-5"},
            }
        ],
    }


class TestConfigSurface:
    def test_absent_means_clean(self) -> None:
        assert match_from_toml(_toml_dict()).lineage == ""

    def test_a_lineage_id_is_carried(self) -> None:
        spec = match_from_toml(_toml_dict(lineage="self-improve.v1"))
        assert spec.lineage == "self-improve.v1"

    def test_a_generation_can_be_pinned(self) -> None:
        assert match_from_toml(_toml_dict(lineage="beta@7")).lineage == "beta@7"

    def test_a_malformed_id_is_a_template_error(self) -> None:
        """Refused rather than ignored: a value the store cannot address would
        run the match clean while provenance said it was seeded."""
        with pytest.raises(MatchTemplateError, match=r"match\.lineage"):
            match_from_toml(_toml_dict(lineage="not a lineage"))
        with pytest.raises(MatchTemplateError, match=r"match\.lineage"):
            match_from_toml(_toml_dict(lineage=7))

    def test_it_round_trips_through_the_toml_the_cloud_uploads(self) -> None:
        """`dump_match` is what `Cloud.submit_run` writes to
        ``runs/<id>/match.toml``, so the marker has to survive it."""
        spec = match_from_toml(_toml_dict(lineage="beta@2"))
        assert match_to_toml_dict(spec)["match"]["lineage"] == "beta@2"
        text = dump_match(spec)
        assert 'lineage = "beta@2"' in text
        assert match_from_toml_text(text).lineage == "beta@2"

    def test_a_clean_dump_reloads_clean(self) -> None:
        spec = match_from_toml(_toml_dict())
        assert match_from_toml_text(dump_match(spec)).lineage == ""


def match_from_toml_text(text: str) -> MatchSpec:
    import tomllib

    return match_from_toml(tomllib.loads(text))


class TestSeatEnvironment:
    """`STELLA_LINEAGE` reaches a Stella seat only when the match asked."""

    def _env(self, spec: MatchSpec, tmp_path: Path) -> tuple[dict, list[str]]:
        seat = Contestant.from_json({"name": "s", "agent": "stella", "engine": {}})
        runner = MatchRunner.__new__(MatchRunner)
        run = ContestantRun(
            contestant=seat,
            job_name="job",
            job_dir=tmp_path / "job",
            log_path=tmp_path / "job.log",
        )
        return runner._agent_environment(spec, seat, run), run.notes

    def test_a_clean_match_exports_nothing(self, tmp_path: Path) -> None:
        env, notes = self._env(match_from_toml(_toml_dict()), tmp_path)
        assert LINEAGE_ENV not in env
        assert not any("lineage" in note for note in notes)

    def test_a_seeded_match_exports_the_id_and_says_so(self, tmp_path: Path) -> None:
        env, notes = self._env(match_from_toml(_toml_dict(lineage="lin")), tmp_path)
        assert env[LINEAGE_ENV] == "lin"
        assert any("SEEDED from lineage lin" in note for note in notes)


class TestProvenanceMarker:
    """The contamination marker, and what happens if it is taken off."""

    def _clean(self) -> provenance.Provenance:
        return provenance.Provenance(
            dataset_key="terminal-bench-2.1",
            dataset_digest="sha256:abcdef0123456789",
            harbor_version="0.6.1",
            sut_commit="0" * 40,
        )

    def _seeded(self, **kwargs) -> provenance.Provenance:
        record = self._clean()
        record.lineage_id = "lin"
        record.lineage_seats = {
            "a": {"lineage": "lin", "generations": [3], "trials": 5, "unseeded": 0}
        }
        for key, value in kwargs.items():
            setattr(record, key, value)
        return record

    def test_a_clean_record_is_spelled_exactly_as_before(self) -> None:
        """Every key written before lineage existed must keep its spelling, or
        the whole archive stops grouping against itself."""
        record = self._clean()
        assert record.contaminated is False
        assert record.comparability_key == (
            "terminal-bench-2.1@abcdef01/harbor0.6.1/stella00000000"
        )
        payload = record.to_json()
        assert "contaminated" not in payload
        assert "lineage_label" not in payload

    def test_a_seeded_record_cannot_spell_a_clean_key(self) -> None:
        seeded = self._seeded()
        assert seeded.contaminated is True
        assert seeded.comparability_key != self._clean().comparability_key
        assert seeded.comparability_key.endswith("/lineage:lin@3")

    def test_the_generation_is_part_of_the_apparatus(self) -> None:
        """Generation 1 and generation 40 of a chain are different amounts of
        prior knowledge, so they must not group together either."""
        gen3 = self._seeded()
        gen9 = self._seeded()
        gen9.lineage_seats["a"]["generations"] = [9]
        assert gen3.comparability_key != gen9.comparability_key

    def test_a_declared_but_unobserved_lineage_is_still_contaminated(self) -> None:
        """A match killed before any trial wrote a manifest still asked to be
        seeded, and that intent is what a reader has to be warned about."""
        record = self._clean()
        record.lineage_id = "lin"
        assert record.contaminated is True
        assert record.comparability_key.endswith("/lineage:lin?")

    def test_the_marker_survives_into_the_serialised_record(self) -> None:
        payload = self._seeded().to_json()
        assert payload["contaminated"] is True
        assert payload["lineage_id"] == "lin"
        assert payload["lineage_label"] == "lineage:lin@3"
        assert payload["comparability_key"].endswith("/lineage:lin@3")
        # And it survives a round trip, so a re-read record is still marked.
        assert provenance.Provenance.from_json(payload).contaminated is True

    def test_a_seeded_and_a_clean_run_cannot_be_summed(self) -> None:
        """The loud failure. It cannot fire while the key carries the lineage
        label — which is the point: deleting that label from `_key` turns a
        silent wrong average into this error and a red test."""
        provenance.assert_aggregable([self._clean(), self._clean()])
        with pytest.raises(provenance.ContaminatedAggregateError) as caught:
            provenance.assert_aggregable([self._clean(), self._seeded()])
        assert len(caught.value.keys) == 2

    def test_an_unprovenanced_record_is_not_silently_tolerated(self) -> None:
        with pytest.raises(provenance.ContaminatedAggregateError):
            provenance.assert_aggregable([self._clean(), None])


def _trial(job_dir: Path, name: str, manifest: dict | None) -> None:
    agent_dir = job_dir / name / "agent"
    agent_dir.mkdir(parents=True)
    if manifest is not None:
        (agent_dir / "lineage").mkdir()
        (agent_dir / "lineage" / "manifest.json").write_text(
            json.dumps(manifest), encoding="utf-8"
        )


class TestObservation:
    """Reading back what the trials were actually seeded with."""

    def _match_dir(self, tmp_path: Path) -> Path:
        job_dir = tmp_path / "jobs" / "m-stella"
        job_dir.mkdir(parents=True)
        (tmp_path / "jobs" / "m-stella.seat.json").write_text(
            json.dumps({"contestant": "a", "agent": "stella", "name": "stella"}),
            encoding="utf-8",
        )
        return tmp_path

    def test_generations_and_unseeded_trials_are_both_counted(
        self, tmp_path: Path
    ) -> None:
        match_dir = self._match_dir(tmp_path)
        job_dir = match_dir / "jobs" / "m-stella"
        _trial(
            job_dir,
            "t1",
            {
                "state": "first-generation",
                "lineage": "lin",
                "task_id": "tb/fix-git",
                "seeded_generation": None,
                "harvested_generation": 0,
            },
        )
        _trial(
            job_dir,
            "t2",
            {
                "state": "seeded",
                "lineage": "lin",
                "task_id": "tb/fix-git",
                "seeded_generation": 0,
                "harvested_generation": 1,
            },
        )
        seats = provenance.observe_lineage(match_dir)
        assert seats["a"] == {
            "lineage": "lin",
            "generations": [0],
            "harvested": [0, 1],
            "tasks": ["tb/fix-git"],
            "trials": 2,
            "unseeded": 1,
        }

    def test_a_clean_match_observes_nothing(self, tmp_path: Path) -> None:
        match_dir = self._match_dir(tmp_path)
        _trial(match_dir / "jobs" / "m-stella", "t1", None)
        _trial(match_dir / "jobs" / "m-stella", "t2", {"state": "off"})
        assert provenance.observe_lineage(match_dir) == {}

    def test_stamping_folds_it_into_the_record(self, tmp_path: Path) -> None:
        match_dir = self._match_dir(tmp_path)
        _trial(
            match_dir / "jobs" / "m-stella",
            "t1",
            {"state": "seeded", "lineage": "lin", "seeded_generation": 4},
        )
        provenance.write_match(match_dir, provenance.Provenance(dataset_key="d"))
        stamped = provenance.stamp_lineage(match_dir)
        assert stamped is not None
        assert stamped.lineage_id == "lin"
        assert stamped.contaminated is True
        # And it is on disk, not only in the returned object.
        assert provenance.read_match(match_dir).contaminated is True  # type: ignore[union-attr]

    def test_stamping_never_downgrades_a_contaminated_record(
        self, tmp_path: Path
    ) -> None:
        """A re-stamp against a half-deleted archive must not make a seeded
        match look clean — that direction is the whole hazard."""
        match_dir = self._match_dir(tmp_path)
        _trial(match_dir / "jobs" / "m-stella", "t1", None)
        provenance.write_match(match_dir, provenance.Provenance(lineage_id="lin"))
        assert provenance.stamp_lineage(match_dir) is None
        assert provenance.read_match(match_dir).contaminated is True  # type: ignore[union-attr]


class _FakeMatch:
    """The duck-typed match shape `series.match_row` reads."""

    def __init__(self, spec: MatchSpec, prov: provenance.Provenance | None) -> None:
        self.spec = spec
        self.provenance = prov
        self.status = "finished"
        self.created_at = 1.0
        self.finished_at = 2.0

    def metrics_for(self, _seat_id: str) -> dict:
        return {}


class TestSeriesGrouping:
    """No chart may draw one line through a seeded and a clean point."""

    def _spec(self) -> MatchSpec:
        return MatchSpec.from_json(
            {
                "name": "m",
                "dataset": "terminal-bench-2.1",
                "tasks": ["fix-git"],
                "contestants": [{"id": "a", "name": "s", "agent": "stella"}],
            }
        )

    def _record(self, lineage: str = "") -> provenance.Provenance:
        record = provenance.Provenance(
            dataset_key="terminal-bench-2.1", dataset_digest="sha256:abcdef01"
        )
        record.lineage_id = lineage
        if lineage:
            record.lineage_seats = {"a": {"lineage": lineage, "generations": [2]}}
        return record

    def test_a_clean_row_is_unmarked(self) -> None:
        row = match_row(_FakeMatch(self._spec(), self._record()))
        assert row["comparability"]["contaminated"] is False
        assert row["comparability"]["lineage"] == ""

    def test_a_seeded_row_keys_apart_and_says_why(self) -> None:
        clean = match_row(_FakeMatch(self._spec(), self._record()))
        seeded = match_row(_FakeMatch(self._spec(), self._record("lin")))
        assert seeded["comparability"]["contaminated"] is True
        assert seeded["comparability"]["lineage"] == "lineage:lin@2"
        assert (
            seeded["comparability"]["group_key"] != clean["comparability"]["group_key"]
        )
        assert clean["comparability"]["group_key"] in seeded["comparability"][
            "group_key"
        ]

    def test_a_record_predating_lineage_still_renders(self) -> None:
        """Duck-typed stand-ins and legacy records carry neither attribute."""
        legacy = SimpleNamespace(
            dataset_digest="sha256:abcdef01",
            sut_commit="",
            to_json=lambda: {"dataset_digest": "sha256:abcdef01"},
        )
        row = match_row(_FakeMatch(self._spec(), legacy))  # type: ignore[arg-type]
        assert row["comparability"]["contaminated"] is False
