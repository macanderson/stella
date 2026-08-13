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


def test_schema_is_one_non_null_results_column(tmp_path: Path) -> None:
    db = tmp_path / "experiments.db"
    conn = experiments.connect(db)
    try:
        info = list(conn.execute("PRAGMA table_info(experiment_results)"))
    finally:
        conn.close()
    assert [(row[1], row[2], row[3]) for row in info] == [("results", "JSONB", 1)]


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
