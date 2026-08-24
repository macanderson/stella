# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The experiments store: durable, database-backed experiment documents.

One SQLite database at ``arenabench_home() / "experiments.db"`` holding one
table, ``experiment_results``. The document is still the record: ``results``
stores each experiment document whole, exactly as assembled from the
ground-truth artifacts it cites, so the schema cannot silently drop a field
the document carries. The columns beside it are identity and provenance,
never a second copy of the document's content:

- ``id`` — an explicit ``INTEGER PRIMARY KEY``. The first schema had no key
  at all and leaned on implicit rowids for its "latest row wins" rule, but
  a ``VACUUM`` may renumber implicit rowids, which silently reorders the
  store. An aliased rowid is stable.
- ``experiment_id`` — ``$.experiment.id`` extracted at write time and
  indexed, so looking one experiment up reads one row instead of parsing
  every stored document.
- ``created_at`` — when the row was stored (UTC). ``NULL`` on rows written
  before the column existed: an unknown time is recorded as unknown, the
  same rule ``bench/telemetry_store``'s ``cost_norm_status`` applies to
  costs it cannot reconstruct.
- ``migrated`` / ``migration_source`` — whether the row has been mirrored
  to the durable copy (see :mod:`arenabench.mirror`), and the
  ``machine@tier`` label the mirror stamped it with.

Everything here is generic product surface: an "experiment" is any JSON
document a caller assembles (hypothesis, comparability keys, trials,
metrics). Agent names, models, and datasets are data *inside* documents,
never concepts this module knows about.

Storage conventions follow ``bench/telemetry_store/ingest.py`` — stdlib
:mod:`sqlite3`, an all-``IF NOT EXISTS`` schema applied on every connect,
and an additive idempotent :func:`_migrate` for what ``IF NOT EXISTS``
cannot express. The ``results`` column is declared ``JSONB`` (valid DDL on
both SQLite and PostgreSQL). SQLite stores what it is given: on a linked
SQLite new enough to have the JSONB functions (>= 3.45) writes go through
``jsonb()`` and are stored in the compact binary encoding; on an older
SQLite the same document is stored as JSON text. Reads normalize through
``json()`` wherever it exists, so a database file written by either
encoding round-trips on any host.

WAL journaling is enabled because ArenaBench is multi-process by design
(``arenabench serve`` and ``arenabench run`` share one workspace), and the
documents are serialized with sorted keys so identical documents produce
identical bytes — the same byte-stability discipline the SUT applies to
its prompts.
"""

from __future__ import annotations

import json
import sqlite3
from collections.abc import Mapping
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from .sut import arenabench_home

__all__ = [
    "connect",
    "experiment_document",
    "experiments_db_path",
    "experiments_payload",
    "load_results",
    "mark_migrated",
    "store_results",
    "stored_rows",
]

#: Applied on every connect; ``IF NOT EXISTS`` makes it idempotent, and
#: :func:`_migrate` rebuilds a database created before these columns
#: existed. Identity and provenance are columns; the document's content
#: stays inside ``results`` and is queryable with ``json_extract``.
_TABLE_DDL = """\
CREATE TABLE IF NOT EXISTS experiment_results (
    id               INTEGER PRIMARY KEY,
    experiment_id    TEXT,
    created_at       TEXT,
    migrated         INTEGER NOT NULL DEFAULT 0,
    migration_source TEXT,
    results          JSONB NOT NULL
)"""

_INDEX_DDL = """\
CREATE INDEX IF NOT EXISTS experiment_results_experiment_idx
    ON experiment_results(experiment_id)"""

_SCHEMA = f"{_TABLE_DDL};\n{_INDEX_DDL};\n"

#: The JSONB function family arrived in SQLite 3.45.0. Older linked
#: SQLites still read and write this store — as JSON text.
_JSONB_MIN_VERSION = (3, 45, 0)


def experiments_db_path() -> Path:
    """Where the experiments database lives, honouring ``ARENABENCH_HOME``."""
    return arenabench_home() / "experiments.db"


def _jsonb_available() -> bool:
    """Whether the linked SQLite has the ``jsonb()`` function family."""
    return sqlite3.sqlite_version_info >= _JSONB_MIN_VERSION


def _results_as_json_sql() -> str:
    """The expression that reads ``results`` back as JSON text."""
    return "json(results)" if _jsonb_available() else "results"


def _migrate(conn: sqlite3.Connection) -> None:
    """Rebuild a legacy one-column store into the current shape.

    The first schema was ``(results JSONB NOT NULL)`` alone. Adding a
    primary key needs a table rebuild — ``ALTER TABLE ADD COLUMN`` cannot
    add one — so a legacy table is renamed aside, its rows copied in rowid
    order with ``experiment_id`` backfilled from the documents, and then
    dropped. ``created_at`` stays NULL on copied rows: the store never
    recorded when they were written, and inventing a timestamp would turn
    the migration date into fake history. Idempotent: a table that already
    has the ``id`` column is left alone.
    """
    columns = {row[1] for row in conn.execute("PRAGMA table_info(experiment_results)")}
    if not columns or "id" in columns:
        return
    read = _results_as_json_sql()
    with conn:
        conn.execute("ALTER TABLE experiment_results RENAME TO experiment_results_v0")
        conn.execute(_TABLE_DDL)
        conn.execute(_INDEX_DDL)
        conn.execute(
            "INSERT INTO experiment_results (experiment_id, results) "
            f"SELECT json_extract({read}, '$.experiment.id'), results "
            "FROM experiment_results_v0 ORDER BY rowid"
        )
        conn.execute("DROP TABLE experiment_results_v0")


def connect(db_path: Path | None = None) -> sqlite3.Connection:
    """Open (creating if absent) the experiments database.

    Applies the schema and the rebuild migration on every call, so callers
    never see a database missing its table or its columns. WAL because the
    server and a concurrent run are separate processes sharing one
    workspace.
    """
    path = experiments_db_path() if db_path is None else db_path
    path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path)
    conn.execute("PRAGMA journal_mode = WAL")
    conn.execute("PRAGMA foreign_keys = ON")
    _migrate(conn)
    conn.executescript(_SCHEMA)
    return conn


def store_results(document: Mapping[str, Any], db_path: Path | None = None) -> None:
    """Persist one experiment document into ``experiment_results``.

    The document must be a JSON-serializable mapping — an experiment is a
    document, not a bare scalar. Serialization uses sorted keys so the
    same document always produces the same stored bytes. The row is
    stamped with the document's own ``$.experiment.id`` and the write
    time; both are identity, not content, so the document itself stays
    byte-identical to what the caller assembled.
    """
    if not isinstance(document, Mapping):
        raise TypeError(
            f"an experiment document is a JSON object, got {type(document).__name__}"
        )
    text = json.dumps(document, ensure_ascii=False, sort_keys=True)
    experiment = document.get("experiment")
    experiment_id = experiment.get("id") if isinstance(experiment, Mapping) else None
    created_at = datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
    value = "jsonb(?)" if _jsonb_available() else "?"
    conn = connect(db_path)
    try:
        with conn:
            conn.execute(
                "INSERT INTO experiment_results "
                f"(experiment_id, created_at, results) VALUES (?, ?, {value})",
                (experiment_id, created_at, text),
            )
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
    conn = connect(path)
    try:
        rows = conn.execute(
            f"SELECT {_results_as_json_sql()} FROM experiment_results ORDER BY id"
        )
        return [json.loads(row[0]) for row in rows]
    finally:
        conn.close()


def stored_rows(db_path: Path | None = None) -> list[dict[str, Any]]:
    """Every row with its identity and provenance columns, in insertion order.

    What the mirror reads: the document plus the row metadata
    (``created_at``, ``migrated``, ``migration_source``) that
    :func:`load_results` deliberately strips. An absent database yields an
    empty list, same as :func:`load_results`.
    """
    path = experiments_db_path() if db_path is None else db_path
    if not path.exists():
        return []
    conn = connect(path)
    try:
        rows = conn.execute(
            "SELECT id, experiment_id, created_at, migrated, migration_source, "
            f"{_results_as_json_sql()} FROM experiment_results ORDER BY id"
        )
        return [
            {
                "id": row[0],
                "experiment_id": row[1],
                "created_at": row[2],
                "migrated": row[3],
                "migration_source": row[4],
                "document": json.loads(row[5]),
            }
            for row in rows
        ]
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
    of a calculation supersedes its predecessor. The lookup reads only the
    matching rows through the ``experiment_id`` index — never every
    document in the store.
    """
    path = experiments_db_path() if db_path is None else db_path
    if not path.exists():
        return None
    conn = connect(path)
    try:
        row = conn.execute(
            f"SELECT {_results_as_json_sql()} FROM experiment_results "
            "WHERE experiment_id = ? ORDER BY id DESC LIMIT 1",
            (experiment_id,),
        ).fetchone()
        return json.loads(row[0]) if row else None
    finally:
        conn.close()


def mark_migrated(source: str, db_path: Path | None = None) -> int:
    """Stamp every unmigrated row as mirrored from ``source``.

    ``source`` is a ``machine@tier`` label (``Mac@local``), recorded so the
    durable copy and the working set agree about where each row came from.
    Returns how many rows the stamp reached. Called by the mirror only
    after the durable copy has verifiably accepted the rows — stamping
    first would let a failed push read as a finished one.
    """
    conn = connect(db_path)
    try:
        with conn:
            cursor = conn.execute(
                "UPDATE experiment_results "
                "SET migrated = 1, migration_source = ? WHERE migrated = 0",
                (source,),
            )
        return cursor.rowcount
    finally:
        conn.close()
