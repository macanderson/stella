# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Mirror the local experiments store into the durable benchmark database.

``bench/telemetry_store/schema.sql`` names the arrangement this module
implements: the local SQLite copy is the working set and the Postgres copy
is the durable one. This module turns the working set into SQL the durable
side can apply — it deliberately emits SQL text instead of holding a
database connection, because the durable copy is reached over SSM (no open
port, no credentials on the workstation) and the only thing that channel
carries is text.

The emitted SQL is idempotent end to end. The schema is all
``IF NOT EXISTS``; every row's primary key is the SHA-256 of its canonical
document bytes, inserted with ``ON CONFLICT DO NOTHING``, so re-running a
mirror — or two machines mirroring the same document — converges instead
of duplicating. The DDL and the inserts are valid on both PostgreSQL and
SQLite, which is what lets the tests here prove the SQL by executing it
rather than by matching strings.

Every mirrored row carries ``migrated = 1`` and a ``migration_source`` of
the form ``machine@tier`` (``Mac@local``), so the durable copy names the
working set each row came from. The local rows are stamped with the same
pair by :func:`arenabench.experiments.mark_migrated` — only after the
durable side has verifiably accepted them.

Usage::

    python3 -m arenabench.mirror emit --source Mac@local --out mirror.sql
    python3 -m arenabench.mirror mark --source Mac@local
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from collections.abc import Mapping, Sequence
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from .experiments import mark_migrated, stored_rows

__all__ = [
    "PRODUCTION_SCHEMA",
    "mirror_rows",
    "mirror_sql",
]

#: The durable copy of ``experiment_results``. Same philosophy as the
#: local store — the document is the record, the columns beside it are
#: identity and provenance — with two differences the durable tier forces:
#: the key is the content hash (machine-independent, so any number of
#: working sets can mirror into one table), and the listing headers
#: (title, status, schema, calculation version) are extracted so the
#: durable side can answer a gallery query without parsing documents.
#: ``doc_schema`` rather than ``schema`` because the bare word collides
#: with the SQL namespace concept on PostgreSQL. ``results`` is TEXT, not
#: JSONB, and that is the point: PostgreSQL's jsonb normalizes key order
#: and whitespace, so a JSONB column could never re-derive ``doc_sha256``
#: from what it stores. TEXT keeps the stored bytes the bytes the hash
#: names; a reader that wants json operators casts at query time.
PRODUCTION_SCHEMA = """\
CREATE TABLE IF NOT EXISTS experiment_results (
    doc_sha256          TEXT PRIMARY KEY,
    experiment_id       TEXT,
    title               TEXT,
    status              TEXT,
    doc_schema          TEXT,
    calculation_version TEXT,
    created_at          TEXT,
    migrated            INTEGER NOT NULL DEFAULT 0,
    migration_source    TEXT,
    mirrored_at         TEXT NOT NULL,
    results             TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS experiment_results_experiment_idx
    ON experiment_results(experiment_id);
"""


def _canonical(document: Mapping[str, Any]) -> str:
    """The document's canonical bytes: sorted keys, no ASCII escaping.

    The same serialization :func:`arenabench.experiments.store_results`
    writes, so the hash of a mirrored row equals the hash of the local row
    it came from.
    """
    return json.dumps(document, ensure_ascii=False, sort_keys=True)


def mirror_rows(db_path: Path | None = None) -> list[dict[str, Any]]:
    """Every local experiment document, shaped for the durable table.

    One dict per document: the content hash, the extracted listing
    headers, the local row's ``created_at``, and the canonical text.
    Reading through :func:`~arenabench.experiments.stored_rows` keeps this
    the only module that knows the durable shape while the experiments
    module stays the only one that knows the local one.
    """
    rows = []
    for stored in stored_rows(db_path):
        document = stored["document"]
        text = _canonical(document)
        experiment = document.get("experiment")
        header = experiment if isinstance(experiment, Mapping) else {}
        rows.append(
            {
                "doc_sha256": hashlib.sha256(text.encode()).hexdigest(),
                "experiment_id": header.get("id"),
                "title": header.get("title"),
                "status": header.get("status"),
                "doc_schema": document.get("schema"),
                "calculation_version": document.get("calculation_version"),
                "created_at": stored["created_at"],
                "results": text,
            }
        )
    return rows


def _literal(value: str | None) -> str:
    """``value`` as a SQL string literal, ``NULL`` when absent.

    Single quotes are doubled and nothing else is escaped — the portable
    quoting shared by PostgreSQL (with ``standard_conforming_strings``,
    the default since 9.1) and SQLite.
    """
    if value is None:
        return "NULL"
    return "'" + value.replace("'", "''") + "'"


def mirror_sql(
    rows: Sequence[Mapping[str, Any]],
    source: str,
    mirrored_at: str,
) -> str:
    """One idempotent script: schema, then every row as a keyed insert.

    ``source`` is the ``machine@tier`` label stamped into
    ``migration_source``; ``mirrored_at`` is passed in rather than read
    from the clock so the same rows always produce the same script.
    """
    statements = [PRODUCTION_SCHEMA]
    for row in rows:
        values = ", ".join(
            (
                _literal(row["doc_sha256"]),
                _literal(row["experiment_id"]),
                _literal(row["title"]),
                _literal(row["status"]),
                _literal(row["doc_schema"]),
                _literal(row["calculation_version"]),
                _literal(row.get("created_at")),
                "1",
                _literal(source),
                _literal(mirrored_at),
                _literal(row["results"]),
            )
        )
        statements.append(
            "INSERT INTO experiment_results "
            "(doc_sha256, experiment_id, title, status, doc_schema, "
            "calculation_version, created_at, migrated, migration_source, "
            "mirrored_at, results)\n"
            f"VALUES ({values})\n"
            "ON CONFLICT (doc_sha256) DO NOTHING;"
        )
    return "\n".join(statements) + "\n"


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    emit = sub.add_parser("emit", help="write the mirror SQL for every local row")
    emit.add_argument("--db", type=Path, default=None, help="local experiments.db")
    emit.add_argument("--source", required=True, help="machine@tier provenance label")
    emit.add_argument("--out", type=Path, required=True, help="where to write the SQL")

    mark = sub.add_parser("mark", help="stamp local rows as migrated")
    mark.add_argument("--db", type=Path, default=None, help="local experiments.db")
    mark.add_argument("--source", required=True, help="machine@tier provenance label")

    args = parser.parse_args(argv)
    if args.command == "emit":
        rows = mirror_rows(args.db)
        stamp = datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
        args.out.write_text(mirror_sql(rows, args.source, stamp))
        print(f"{len(rows)} rows -> {args.out}")
        return 0
    count = mark_migrated(args.source, args.db)
    print(f"{count} rows marked migrated ({args.source})")
    return 0


if __name__ == "__main__":  # pragma: no cover - exercised via the CLI
    sys.exit(main())
