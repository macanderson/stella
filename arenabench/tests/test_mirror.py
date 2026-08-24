# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The mirror: durable-copy SQL emission, idempotency, and provenance."""

from __future__ import annotations

import hashlib
import json
import sqlite3
from pathlib import Path

from arenabench import experiments, mirror

STAMP = "2026-08-23T00:00:00Z"


def _emit(tmp_path: Path, *documents: dict) -> str:
    db = tmp_path / "experiments.db"
    for document in documents:
        experiments.store_results(document, db)
    return mirror.mirror_sql(mirror.mirror_rows(db), "Mac@local", STAMP)


def _apply(target: sqlite3.Connection, sql: str) -> None:
    target.executescript(sql)


def test_emitted_sql_executes_and_lands_every_row(tmp_path: Path) -> None:
    document = {
        "schema": "arenabench-experiment-document/1",
        "calculation_version": "calc/1",
        "experiment": {"id": "exp-001", "title": "A vs B", "status": "open"},
        "trials": [{"task": "t1"}],
    }
    sql = _emit(tmp_path, document)
    target = sqlite3.connect(":memory:")
    try:
        _apply(target, sql)
        (row,) = target.execute(
            "SELECT experiment_id, title, status, doc_schema, "
            "calculation_version, migrated, migration_source, mirrored_at, "
            "results FROM experiment_results"
        )
    finally:
        target.close()
    assert row[:8] == (
        "exp-001",
        "A vs B",
        "open",
        "arenabench-experiment-document/1",
        "calc/1",
        1,
        "Mac@local",
        STAMP,
    )
    assert json.loads(row[8]) == document


def test_mirroring_twice_does_not_duplicate(tmp_path: Path) -> None:
    sql = _emit(tmp_path, {"experiment": {"id": "exp-001"}})
    target = sqlite3.connect(":memory:")
    try:
        _apply(target, sql)
        _apply(target, sql)
        (count,) = target.execute("SELECT COUNT(*) FROM experiment_results").fetchone()
    finally:
        target.close()
    assert count == 1


def test_row_key_is_the_canonical_document_hash(tmp_path: Path) -> None:
    document = {"experiment": {"id": "exp-001"}, "b": 2, "a": 1}
    (row,) = mirror.mirror_rows(_stored(tmp_path, document))
    canonical = json.dumps(document, ensure_ascii=False, sort_keys=True)
    assert row["doc_sha256"] == hashlib.sha256(canonical.encode()).hexdigest()


def test_quoting_survives_documents_containing_quotes(tmp_path: Path) -> None:
    document = {"experiment": {"id": "exp-001", "title": "it's; DROP TABLE x"}}
    sql = _emit(tmp_path, document)
    target = sqlite3.connect(":memory:")
    try:
        _apply(target, sql)
        (title,) = target.execute("SELECT title FROM experiment_results").fetchone()
    finally:
        target.close()
    assert title == "it's; DROP TABLE x"


def test_local_created_at_travels_to_the_durable_row(tmp_path: Path) -> None:
    sql = _emit(tmp_path, {"experiment": {"id": "exp-001"}})
    target = sqlite3.connect(":memory:")
    try:
        _apply(target, sql)
        (created_at,) = target.execute("SELECT created_at FROM experiment_results").fetchone()
    finally:
        target.close()
    assert created_at is not None


def _stored(tmp_path: Path, *documents: dict) -> Path:
    db = tmp_path / "experiments.db"
    for document in documents:
        experiments.store_results(document, db)
    return db
