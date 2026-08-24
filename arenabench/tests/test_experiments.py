# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The experiments store: schema shape, round-trips, and encoding tolerance."""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path

import pytest

from arenabench import experiments


def test_home_resolution_honours_arenabench_home(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("ARENABENCH_HOME", str(tmp_path / "home"))
    assert experiments.experiments_db_path() == tmp_path / "home" / "experiments.db"


def test_schema_has_identity_provenance_and_the_document(tmp_path: Path) -> None:
    db = tmp_path / "experiments.db"
    conn = experiments.connect(db)
    try:
        info = list(conn.execute("PRAGMA table_info(experiment_results)"))
    finally:
        conn.close()
    assert [(row[1], row[2], row[3]) for row in info] == [
        ("id", "INTEGER", 0),
        ("experiment_id", "TEXT", 0),
        ("created_at", "TEXT", 0),
        ("migrated", "INTEGER", 1),
        ("migration_source", "TEXT", 0),
        ("results", "JSONB", 1),
    ]


def test_legacy_one_column_store_is_rebuilt_with_rows_kept(tmp_path: Path) -> None:
    """A database from the first schema gains the identity columns.

    The rebuild must keep every row in order, backfill ``experiment_id``
    from the documents, and leave ``created_at`` NULL — the legacy store
    never recorded a write time, and the migration must not invent one.
    """
    db = tmp_path / "experiments.db"
    first = {"experiment": {"id": "exp-001"}, "calculation_version": "calc/1"}
    second = {"experiment": {"id": "exp-002"}, "calculation_version": "calc/2"}
    legacy = sqlite3.connect(db)
    try:
        with legacy:
            legacy.execute("CREATE TABLE experiment_results (results JSONB NOT NULL)")
            for document in (first, second):
                legacy.execute(
                    "INSERT INTO experiment_results (results) VALUES (?)",
                    (json.dumps(document, sort_keys=True),),
                )
    finally:
        legacy.close()

    assert experiments.load_results(db) == [first, second]
    rows = experiments.stored_rows(db)
    assert [row["experiment_id"] for row in rows] == ["exp-001", "exp-002"]
    assert [row["created_at"] for row in rows] == [None, None]
    assert [row["migrated"] for row in rows] == [0, 0]
    assert experiments.experiment_document("exp-002", db) == second


def test_store_stamps_experiment_id_and_created_at(tmp_path: Path) -> None:
    db = tmp_path / "experiments.db"
    experiments.store_results({"experiment": {"id": "exp-001"}}, db)
    (row,) = experiments.stored_rows(db)
    assert row["experiment_id"] == "exp-001"
    assert row["created_at"] is not None
    assert row["migrated"] == 0
    assert row["migration_source"] is None


def test_mark_migrated_stamps_only_unmigrated_rows(tmp_path: Path) -> None:
    db = tmp_path / "experiments.db"
    experiments.store_results({"experiment": {"id": "exp-001"}}, db)
    assert experiments.mark_migrated("Mac@local", db) == 1
    (row,) = experiments.stored_rows(db)
    assert row["migrated"] == 1
    assert row["migration_source"] == "Mac@local"
    # Already-stamped rows are not restamped by a later mirror.
    assert experiments.mark_migrated("Other@local", db) == 0
    (row,) = experiments.stored_rows(db)
    assert row["migration_source"] == "Mac@local"


def test_store_and_load_round_trip(tmp_path: Path) -> None:
    db = tmp_path / "experiments.db"
    document = {
        "experiment": {"id": "exp-001", "hypothesis": "arm A beats arm B"},
        "trials": [{"task": "t1", "outcome": "verified_solve"}],
    }
    experiments.store_results(document, db)
    assert experiments.load_results(db) == [document]


def test_documents_come_back_in_insertion_order(tmp_path: Path) -> None:
    db = tmp_path / "experiments.db"
    first = {"experiment": {"id": "exp-001"}}
    second = {"experiment": {"id": "exp-002"}}
    experiments.store_results(first, db)
    experiments.store_results(second, db)
    assert experiments.load_results(db) == [first, second]


def test_non_mapping_document_is_rejected(tmp_path: Path) -> None:
    with pytest.raises(TypeError, match="JSON object"):
        experiments.store_results(["not", "a", "document"], tmp_path / "experiments.db")  # type: ignore[arg-type]


def test_null_results_are_impossible(tmp_path: Path) -> None:
    conn = experiments.connect(tmp_path / "experiments.db")
    try:
        with pytest.raises(sqlite3.IntegrityError):
            conn.execute("INSERT INTO experiment_results (results) VALUES (NULL)")
    finally:
        conn.close()


def test_reads_tolerate_text_rows(tmp_path: Path) -> None:
    """A row stored as JSON text (an older linked SQLite) still loads.

    The store feature-detects ``jsonb()``, so one database file can hold a
    mix of encodings across hosts; the read path must not care.
    """
    db = tmp_path / "experiments.db"
    document = {"experiment": {"id": "exp-legacy"}}
    conn = experiments.connect(db)
    try:
        with conn:
            conn.execute(
                "INSERT INTO experiment_results (results) VALUES (?)",
                (json.dumps(document, sort_keys=True),),
            )
    finally:
        conn.close()
    assert experiments.load_results(db) == [document]


def test_loading_a_missing_database_returns_empty_and_creates_nothing(
    tmp_path: Path,
) -> None:
    db = tmp_path / "experiments.db"
    assert experiments.load_results(db) == []
    assert not db.exists()


def test_payload_summarizes_each_document(tmp_path: Path) -> None:
    db = tmp_path / "experiments.db"
    experiments.store_results(
        {
            "schema": "arenabench-experiment-document/1",
            "calculation_version": "calc/1",
            "experiment": {"id": "exp-001", "title": "A vs B", "status": "open"},
            "trials": [{"task": "t1"}],
        },
        db,
    )
    assert experiments.experiments_payload(db) == {
        "experiments": [
            {
                "id": "exp-001",
                "title": "A vs B",
                "status": "open",
                "schema": "arenabench-experiment-document/1",
                "calculation_version": "calc/1",
            }
        ]
    }


def test_document_lookup_finds_by_id_and_latest_wins(tmp_path: Path) -> None:
    db = tmp_path / "experiments.db"
    experiments.store_results(
        {"experiment": {"id": "exp-001"}, "calculation_version": "calc/1"}, db
    )
    experiments.store_results(
        {"experiment": {"id": "exp-001"}, "calculation_version": "calc/2"}, db
    )
    document = experiments.experiment_document("exp-001", db)
    assert document is not None
    assert document["calculation_version"] == "calc/2"
    assert experiments.experiment_document("exp-missing", db) is None


@pytest.mark.skipif(
    sqlite3.sqlite_version_info < (3, 45, 0),
    reason="linked SQLite predates the jsonb() function family",
)
def test_writes_use_binary_jsonb_where_available(tmp_path: Path) -> None:
    db = tmp_path / "experiments.db"
    experiments.store_results({"experiment": {"id": "exp-bin"}}, db)
    conn = experiments.connect(db)
    try:
        (storage_class,) = conn.execute(
            "SELECT typeof(results) FROM experiment_results"
        ).fetchone()
    finally:
        conn.close()
    assert storage_class == "blob"
