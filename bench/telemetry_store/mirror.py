"""Emit the SQL that copies a local telemetry store into the durable one.

Usage:
    python3 mirror.py --db bench.db [--run TAG ...] [--out mirror.sql]
        [--manifest mirror.json] [--source Mac@local]

`schema.sql` says the local SQLite copy is the working set and the Postgres
copy is the durable one, and `runs.migrated` / `runs.migration_source` record
which is which. This is the half that moves the rows.

It emits SQL rather than opening a connection, so the transport is somebody
else's problem: the durable store sits behind SSM on a private instance with no
open database port, and a text file is what fits through that. The same text
applies to a scratch SQLite database, which is how the idempotency test runs
with no cloud at all.

## The rules the emitted SQL keeps

* **Valid on SQLite and PostgreSQL**, per `schema.sql`'s own header: no
  `AUTOINCREMENT`, no boolean literals, no `JSONB`. Only two syntaxes here are
  not ancient — `ON CONFLICT DO NOTHING` (SQLite 3.24+, PostgreSQL 9.5+) and
  nothing else.
* **Idempotent.** Every primary key in this schema is an id the ingester
  supplied, so re-applying the same file inserts nothing a second time. A run
  already on the durable side keeps the row it has; this never overwrites one,
  because the local copy is the working set and the durable copy is the
  record.
* **Children after parents**, so the foreign keys hold on the way in:
  `runs` -> `trials` -> everything keyed off a trial.
* **One transaction.** A half-applied mirror is orphan rows, so the file opens
  `BEGIN` and closes `COMMIT`.
* **Verify before mark.** The stamp of `migrated = 1` / `migration_source` is
  an `UPDATE` at the end of the durable-side transaction, after every row it
  vouches for. The *local* rows are marked by whoever ships this file, and only
  after comparing the durable side's counts against the manifest — see
  `verification_sql` and `SHIPPING_GAP`.

## What it does not do

Ship anything. The SSM wrapper that gzips this, chunks it, sends it, runs
`verification_sql`, compares the manifest and only then marks the local rows is
`SHIPPING_GAP`. The template named for it in #4614
(`arenabench/scripts/mirror-experiments.sh`) left this repository with the
ejection to `macanderson/arenabench` (#4642), so it is being written rather
than generalized.
"""

from __future__ import annotations

import argparse
import json
import math
import socket
import sys

from ingest import connect, now

# Where the SSM shipping wrapper is being written.
SHIPPING_GAP = "#5097"

# Every table this mirror carries, parents first. The order is the foreign-key
# order and is what makes the emitted file appliable in one pass; `runs` is
# handled apart from the rest because it also takes the migration stamp.
PARENT_TABLE = "runs"
CHILD_TABLES = (
    "trials",
    "events",
    "tool_calls",
    "verifier_tests",
    "artifacts",
    "step_grades",
    "turn_grades",
    "execution_grades",
)
ALL_TABLES = (PARENT_TABLE, *CHILD_TABLES)

# How each table is narrowed to the selected runs. `trials` is the join every
# other child reaches `runs` through, and `execution_grades` carries `run_id`
# of its own, but going through `trials` for all of them keeps one predicate to
# reason about rather than two.
_RUN_PREDICATE = {
    "runs": "run_id IN ({placeholders})",
    "trials": "run_id IN ({placeholders})",
}
_TRIAL_PREDICATE = (
    "trial_id IN (SELECT trial_id FROM trials WHERE run_id IN ({placeholders}))"
)


class UnmirrorableValue(ValueError):
    """A cell that cannot be written as portable SQL.

    Raised rather than coerced. Every coercion available here — a NaN cost
    written as NULL, a NUL byte dropped from a message — changes the data on
    the durable side while looking exactly like a clean mirror, and the durable
    copy is the one that outlives the machine that can be checked against.
    """


def _literal(value):
    """One cell as a SQL literal both engines read the same way.

    Strings double their single quotes and nothing else: PostgreSQL runs with
    `standard_conforming_strings` on, so a backslash is a backslash in both
    engines, and inventing an escape here would make the two disagree.
    """
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        # `schema.sql` stores 0/1 integers because SQLite has no boolean type,
        # and a `TRUE` literal would not survive the round trip.
        return "1" if value else "0"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        if not math.isfinite(value):
            raise UnmirrorableValue(f"{value!r} has no portable SQL spelling")
        return repr(value)
    if isinstance(value, bytes):
        return "X'" + value.hex() + "'"
    text = str(value)
    if "\x00" in text:
        raise UnmirrorableValue("a NUL byte cannot be stored in a PostgreSQL text column")
    return "'" + text.replace("'", "''") + "'"


def columns_of(conn, table):
    """The table's column names, read from the store rather than listed here.

    A hardcoded column list is a second copy of the schema, and the first thing
    a schema migration breaks. The durable side is expected to carry the same
    columns; a mismatch is a genuine schema drift and should fail loudly on
    apply rather than be papered over here.
    """
    return [row[1] for row in conn.execute(f"PRAGMA table_info({table})")]


def _where(table, run_ids):
    if not run_ids:
        return "", []
    placeholders = ",".join("?" for _ in run_ids)
    template = _RUN_PREDICATE.get(table, _TRIAL_PREDICATE)
    return " WHERE " + template.format(placeholders=placeholders), list(run_ids)


def rows_of(conn, table, run_ids):
    """Every row of `table` belonging to the selected runs, oldest key first.

    Ordered by the primary key so two emissions of an unchanged store produce
    byte-identical files, which is what lets a reviewer diff a mirror.
    """
    where, params = _where(table, run_ids)
    key = columns_of(conn, table)[0]
    return conn.execute(f"SELECT * FROM {table}{where} ORDER BY {key}", params).fetchall()


def _insert(table, columns, row):
    values = ",".join(_literal(cell) for cell in row)
    return (
        f"INSERT INTO {table} ({','.join(columns)}) VALUES ({values})"
        " ON CONFLICT DO NOTHING;"
    )


def manifest(conn, run_ids, source):
    """What the durable side must hold once this file has applied.

    The other half of "verify before mark": the shipper compares these counts
    against `verification_sql`'s output and marks the local rows only if they
    agree. Counts rather than hashes because `ON CONFLICT DO NOTHING` means the
    durable side may legitimately hold rows this file did not carry, and a
    count of what is present answers the question a shipper actually has.
    """
    return {
        "source": source,
        "generated_at": now(),
        "runs": list(run_ids),
        "counts": {table: len(rows_of(conn, table, run_ids)) for table in ALL_TABLES},
    }


def verification_sql(run_ids):
    """The SELECTs whose output a shipper compares against `manifest`.

    Emitted rather than run, for the reason the whole module emits: nothing
    here can reach the durable side.
    """
    if run_ids:
        listed = ",".join(_literal(run_id) for run_id in run_ids)
        scope = {
            "runs": f" WHERE run_id IN ({listed})",
            "trials": f" WHERE run_id IN ({listed})",
        }
        child = f" WHERE trial_id IN (SELECT trial_id FROM trials WHERE run_id IN ({listed}))"
    else:
        scope, child = {}, ""
    lines = [
        f"SELECT '{table}' AS table_name, COUNT(*) AS rows FROM {table}"
        f"{scope.get(table, child)};"
        for table in ALL_TABLES
    ]
    return "\n".join(lines) + "\n"


def emit(conn, run_ids, source):
    """The whole mirror, as one appliable transaction."""
    lines = [
        "-- Generated by bench/telemetry_store/mirror.py. Idempotent:",
        "-- re-applying this file inserts nothing a second time.",
        f"-- source: {source}",
        f"-- generated: {now()}",
        "BEGIN;",
    ]

    run_columns = columns_of(conn, PARENT_TABLE)
    run_rows = rows_of(conn, PARENT_TABLE, run_ids)
    for row in run_rows:
        lines.append(_insert(PARENT_TABLE, run_columns, row))

    for table in CHILD_TABLES:
        columns = columns_of(conn, table)
        for row in rows_of(conn, table, run_ids):
            lines.append(_insert(table, columns, row))

    # The stamp goes last, after every row it vouches for, and it is an UPDATE
    # rather than part of the INSERT: a run already on the durable side takes
    # `ON CONFLICT DO NOTHING`, and its provenance still has to be recorded.
    key_index = run_columns.index("run_id")
    for row in run_rows:
        run_id = _literal(row[key_index])
        lines.append(
            f"UPDATE {PARENT_TABLE} SET migrated = 1, migration_source = {_literal(source)}"
            f" WHERE run_id = {run_id};"
        )

    lines.append("COMMIT;")
    return "\n".join(lines) + "\n"


def default_source():
    """`<machine>@local` -- the label the durable side records for this copy."""
    try:
        host = socket.gethostname().split(".")[0]
    except OSError:  # pragma: no cover - a hostless container
        host = "unknown"
    return f"{host}@local"


def mark_migrated(conn, run_ids, source):
    """Stamp the LOCAL rows, once the durable side has been verified.

    Separated from `emit` so the ordering cannot be got wrong by accident: this
    is the last step of a mirror, and calling it before comparing
    `verification_sql`'s output against `manifest` marks rows that may not have
    landed. The shipper in `SHIPPING_GAP` is what calls it.
    """
    where, params = _where(PARENT_TABLE, run_ids)
    conn.execute(
        f"UPDATE {PARENT_TABLE} SET migrated = 1, migration_source = ?{where}",
        [source, *params],
    )


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Emit idempotent SQL copying a telemetry store into the durable one."
    )
    ap.add_argument("--db", required=True)
    ap.add_argument(
        "--run",
        action="append",
        default=[],
        help="a run tag to mirror; repeatable. Every run when omitted.",
    )
    ap.add_argument("--source", default=None, help="machine label, default <host>@local")
    ap.add_argument("--out", default=None, help="write the SQL here instead of stdout")
    ap.add_argument("--manifest", default=None, help="write the expected row counts here")
    ap.add_argument(
        "--verification",
        default=None,
        help="write the durable-side count queries here",
    )
    a = ap.parse_args()

    source = a.source or default_source()
    conn = connect(a.db)
    run_ids = tuple(a.run)
    sql = emit(conn, run_ids, source)
    counts = manifest(conn, run_ids, source)

    if a.out:
        with open(a.out, "w") as fh:
            fh.write(sql)
    else:
        sys.stdout.write(sql)
    if a.manifest:
        with open(a.manifest, "w") as fh:
            json.dump(counts, fh, indent=2, sort_keys=True)
            fh.write("\n")
    if a.verification:
        with open(a.verification, "w") as fh:
            fh.write(verification_sql(run_ids))

    # Reported rather than assumed: #4614 asks for the size to be measured
    # before anyone believes one SSM command carries it, and one ingested
    # 89-task run holds tens of thousands of event rows.
    size = len(sql.encode())
    print(
        f"{sum(counts['counts'].values())} row(s) across {len(ALL_TABLES)} table(s),"
        f" {size} byte(s) of SQL",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
