# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The experiments store: durable, database-backed experiment documents.

Phase 1 of the Experiments product, and deliberately the smallest durable
shape that is honest about what it is: one SQLite database at
``arenabench_home() / "experiments.db"`` holding one table,
``experiment_results``, with a single non-null JSON document column named
``results``. No normalization yet — an experiment document is stored whole,
exactly as assembled from the ground-truth artifacts it cites, so the
schema cannot silently drop a field the document carries. Normalizing into
typed tables is a later, separate decision made against real documents
rather than ahead of them.

Everything here is generic product surface: an "experiment" is any JSON
document a caller assembles (hypothesis, comparability keys, trials,
metrics). Agent names, models, and datasets are data *inside* documents,
never concepts this module knows about.

Storage conventions follow the two stores that already exist in this
repository:

- ``bench/telemetry_store/ingest.py`` — stdlib :mod:`sqlite3`, an
  all-``IF NOT EXISTS`` schema applied on every connect, additive-only
  idempotent migration. Copied here so the two stores age the same way.
- The column is declared ``JSONB`` (valid DDL on both SQLite and
  PostgreSQL). SQLite stores what it is given: on a linked SQLite new
  enough to have the JSONB functions (>= 3.45) writes go through
  ``jsonb()`` and are stored in the compact binary encoding; on an older
  SQLite the same document is stored as JSON text. Reads normalize through
  ``json()`` wherever it exists, so a database file written by either
  encoding round-trips on any host — the portability concern
  ``bench/telemetry_store/schema.sql`` documents, answered by
  feature-detection instead of by refusing the binary encoding.

WAL journaling is enabled because ArenaBench is multi-process by design
(``arenabench serve`` and ``arenabench run`` share one workspace), and the
documents are serialized with sorted keys so identical documents produce
identical bytes — the same byte-stability discipline the SUT applies to
its prompts.

Its own module rather than more length on ``server.py``, per the
repository's file-size ratchet rule: separable new code becomes a module.
"""

from __future__ import annotations

import json
import sqlite3
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from .sut import arenabench_home

__all__ = [
    "connect",
    "experiment_document",
    "experiments_db_path",
    "experiments_payload",
    "load_results",
    "store_results",
]

#: The whole schema, applied on every connect. ``IF NOT EXISTS`` makes it
#: idempotent; there is deliberately no version table — additive changes
#: follow ``bench/telemetry_store/ingest.py::migrate`` when they arrive.
#:
#: One column, by design (see the module doc): the document is the record.
#: Identity, timestamps, and comparability keys live *inside* ``results``
#: and are queryable with ``json_extract`` — promoting them to columns is
#: the normalization step this phase explicitly defers.
_SCHEMA = """\
CREATE TABLE IF NOT EXISTS experiment_results (
    results JSONB NOT NULL
);
"""

#: The JSONB function family arrived in SQLite 3.45.0. Older linked
#: SQLites still read and write this store — as JSON text.
_JSONB_MIN_VERSION = (3, 45, 0)


def experiments_db_path() -> Path:
    """Where the experiments database lives, honouring ``ARENABENCH_HOME``."""
    return arenabench_home() / "experiments.db"


def _jsonb_available() -> bool:
    """Whether the linked SQLite has the ``jsonb()`` function family."""
    return sqlite3.sqlite_version_info >= _JSONB_MIN_VERSION


def connect(db_path: Path | None = None) -> sqlite3.Connection:
    """Open (creating if absent) the experiments database.

    Applies the schema on every call, so callers never see a database
    missing its table. WAL because the server and a concurrent run are
    separate processes sharing one workspace.
    """
    path = experiments_db_path() if db_path is None else db_path
    path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path)
    conn.execute("PRAGMA journal_mode = WAL")
    conn.execute("PRAGMA foreign_keys = ON")
    conn.executescript(_SCHEMA)
    return conn


def store_results(document: Mapping[str, Any], db_path: Path | None = None) -> None:
    """Persist one experiment document into ``experiment_results``.

    The document must be a JSON-serializable mapping — an experiment is a
    document, not a bare scalar. Serialization uses sorted keys so the
    same document always produces the same stored bytes.
    """
    if not isinstance(document, Mapping):
        raise TypeError(
            f"an experiment document is a JSON object, got {type(document).__name__}"
        )
    text = json.dumps(document, ensure_ascii=False, sort_keys=True)
    sql = (
        "INSERT INTO experiment_results (results) VALUES (jsonb(?))"
        if _jsonb_available()
        else "INSERT INTO experiment_results (results) VALUES (?)"
    )
    conn = connect(db_path)
    try:
        with conn:
            conn.execute(sql, (text,))
    finally:
        conn.close()


def load_results(db_path: Path | None = None) -> list[dict[str, Any]]:
    """Every stored experiment document, in insertion order.

    Reads normalize through ``json()`` when the linked SQLite has it, so
    rows written in either encoding (binary JSONB or JSON text) come back
    as the same Python mapping. A workspace that never stored an
    experiment gets an empty list — looking must not create a database.
    """
    path = experiments_db_path() if db_path is None else db_path
    if not path.exists():
        return []
    select = (
        "SELECT json(results) FROM experiment_results"
        if _jsonb_available()
        else "SELECT results FROM experiment_results"
    )
    conn = connect(path)
    try:
        return [json.loads(row[0]) for row in conn.execute(select)]
    finally:
        conn.close()


def experiments_payload(db_path: Path | None = None) -> dict[str, Any]:
    """Every stored document summarized for a listing surface (#3215).

    One row per document: the document's own ``experiment`` header plus
    the calculation version, which is what a gallery needs to name a card.
    The full document stays behind :func:`experiment_document` — a listing
    that shipped whole documents would grow with every trial they record.
    """
    summaries = []
    for document in load_results(db_path):
        experiment = document.get("experiment")
        header = experiment if isinstance(experiment, Mapping) else {}
        summaries.append(
            {
                "id": header.get("id"),
                "title": header.get("title"),
                "status": header.get("status"),
                "schema": document.get("schema"),
                "calculation_version": document.get("calculation_version"),
            }
        )
    return {"experiments": summaries}


def experiment_document(
    experiment_id: str, db_path: Path | None = None
) -> dict[str, Any] | None:
    """The full stored document whose experiment id matches, or ``None``.

    When one id was stored more than once, the *latest* row wins: a re-run
    of a calculation supersedes its predecessor, and rows are read in
    insertion order.
    """
    found = None
    for document in load_results(db_path):
        experiment = document.get("experiment")
        if isinstance(experiment, Mapping) and experiment.get("id") == experiment_id:
            found = document
    return found
