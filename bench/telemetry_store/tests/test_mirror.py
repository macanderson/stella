"""A mirror applied twice must be the same mirror applied once."""

from __future__ import annotations

import json
import sqlite3

import pytest
from ingest import connect
from mirror import (
    ALL_TABLES,
    UnmirrorableValue,
    emit,
    manifest,
    mark_migrated,
    rows_of,
    verification_sql,
)

SOURCE = "Mac@local"


def local_store(tmp_path, name="local.db", *, runs=("tag",)):
    """A working-set store holding one trial per run, with a row in every
    child table so the foreign-key ordering is exercised rather than assumed."""
    conn = connect(str(tmp_path / name))
    for run_id in runs:
        conn.execute(
            "INSERT OR REPLACE INTO runs (run_id,tag,kind,model,ingested_at,notes)"
            " VALUES (?,?,?,?,?,?)",
            (run_id, run_id, "scored", "z-ai/glm-5.2", "2026-08-15T09:00:00Z", None),
        )
        trial = f"{run_id}:stella:task__abcdefg"
        conn.execute(
            "INSERT OR REPLACE INTO trials (trial_id,run_id,arm,agent,task_name,trial_dir,"
            "reward,passed,started_at) VALUES (?,?,?,?,?,?,?,?,?)",
            (trial, run_id, "stella", "stella", "task", "/tmp/task", 1.0, 1,
             "2026-08-15T09:00:00Z"),
        )
        conn.execute(
            "INSERT OR REPLACE INTO events (event_id,trial_id,seq,type,payload_json)"
            " VALUES (?,?,?,?,?)",
            (f"{trial}:0", trial, 0, "step_usage", json.dumps({"type": "step_usage"})),
        )
        conn.execute(
            "INSERT OR REPLACE INTO tool_calls (id,trial_id,seq,tool) VALUES (?,?,?,?)",
            (f"{trial}:0", trial, 0, "bash"),
        )
        conn.execute(
            "INSERT OR REPLACE INTO verifier_tests (id,trial_id,name,status)"
            " VALUES (?,?,?,?)",
            (f"{trial}:0", trial, "test_thing", "passed"),
        )
        conn.execute(
            "INSERT OR REPLACE INTO artifacts (id,trial_id,kind,rel_path,bytes,sha256)"
            " VALUES (?,?,?,?,?,?)",
            (f"{trial}:r", trial, "result.json", "result.json", 12, "ab" * 32),
        )
        conn.execute(
            "INSERT OR REPLACE INTO step_grades (step_grade_id,trial_id,turn_instance,step,"
            "call_index,event_seq_start,event_seq_end,direction,direction_source,"
            "grader_version,graded_at) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            (f"{trial}:1:1:0:v1", trial, 1, 1, 0, 0, 0, "productive", "deterministic",
             "v1-deterministic", "2026-08-15T10:00:00Z"),
        )
        conn.execute(
            "INSERT OR REPLACE INTO turn_grades (turn_grade_id,trial_id,turn_instance,"
            "grader_version,graded_at) VALUES (?,?,?,?,?)",
            (f"{trial}:1:v1", trial, 1, "v1-deterministic", "2026-08-15T10:00:00Z"),
        )
        conn.execute(
            "INSERT OR REPLACE INTO execution_grades (execution_grade_id,trial_id,run_id,"
            "grader_version,graded_at) VALUES (?,?,?,?,?)",
            (f"{trial}:v1", trial, run_id, "v1-deterministic", "2026-08-15T10:00:00Z"),
        )
    conn.commit()
    return conn


def durable_store(tmp_path, name="durable.db"):
    """A scratch stand-in for the Postgres side: the same `schema.sql`, empty."""
    return connect(str(tmp_path / name))


def counts(conn):
    return {
        table: conn.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
        for table in ALL_TABLES
    }


def test_applying_the_mirror_twice_duplicates_nothing(tmp_path):
    """The property #4614 asks for: every primary key is an ingester-supplied
    id, so `ON CONFLICT DO NOTHING` makes a second apply a no-op."""
    local = local_store(tmp_path)
    durable = durable_store(tmp_path)
    sql = emit(local, (), SOURCE)

    durable.executescript(sql)
    after_first = counts(durable)
    durable.executescript(sql)

    assert counts(durable) == after_first
    assert after_first == {table: 1 for table in ALL_TABLES}


def test_children_land_after_their_parents(tmp_path):
    """`connect` turns foreign keys on, so an ordering that puts a child first
    fails the apply rather than passing quietly."""
    local = local_store(tmp_path)
    durable = durable_store(tmp_path)
    assert durable.execute("PRAGMA foreign_keys").fetchone()[0] == 1
    durable.executescript(emit(local, (), SOURCE))
    orphans = durable.execute(
        "SELECT COUNT(*) FROM events WHERE trial_id NOT IN (SELECT trial_id FROM trials)"
    ).fetchone()[0]
    assert orphans == 0


def test_the_durable_side_records_where_the_copy_came_from(tmp_path):
    local = local_store(tmp_path)
    durable = durable_store(tmp_path)
    durable.executescript(emit(local, (), SOURCE))
    assert durable.execute(
        "SELECT migrated, migration_source FROM runs"
    ).fetchone() == (1, SOURCE)


def test_a_run_already_on_the_durable_side_keeps_its_row_and_still_gets_stamped(tmp_path):
    """`ON CONFLICT DO NOTHING` means the durable copy is the record: a mirror
    never overwrites it. The provenance stamp is an UPDATE for exactly that
    reason, so it lands on a row the INSERT declined."""
    local = local_store(tmp_path)
    durable = durable_store(tmp_path)
    durable.execute(
        "INSERT INTO runs (run_id,tag,kind,model,ingested_at,notes)"
        " VALUES ('tag','tag','scored','z-ai/glm-5.2','2026-08-01T00:00:00Z','the durable one')"
    )
    durable.commit()

    durable.executescript(emit(local, (), SOURCE))
    notes, migrated, source = durable.execute(
        "SELECT notes, migrated, migration_source FROM runs"
    ).fetchone()
    assert notes == "the durable one"
    assert (migrated, source) == (1, SOURCE)


def test_only_the_named_runs_travel_children_included(tmp_path):
    local = local_store(tmp_path, runs=("keep", "leave"))
    durable = durable_store(tmp_path)
    durable.executescript(emit(local, ("keep",), SOURCE))
    assert [row[0] for row in durable.execute("SELECT run_id FROM runs")] == ["keep"]
    assert [row[0] for row in durable.execute("SELECT DISTINCT trial_id FROM events")] == [
        "keep:stella:task__abcdefg"
    ]


def test_the_manifest_and_the_verification_queries_agree_on_the_same_store(tmp_path):
    """The verify half: what the shipper compares before it marks anything."""
    local = local_store(tmp_path)
    durable = durable_store(tmp_path)
    durable.executescript(emit(local, (), SOURCE))

    expected = manifest(local, (), SOURCE)["counts"]
    observed = {}
    for statement in verification_sql(()).strip().split(";"):
        if statement.strip():
            table, rows = durable.execute(statement).fetchone()
            observed[table] = rows
    assert observed == expected


def test_the_emitted_sql_uses_nothing_postgres_cannot_read(tmp_path):
    """`schema.sql`'s own portability rules, held by the rows as well as the
    DDL: a mirror that only applies on SQLite is not a mirror."""
    local = local_store(tmp_path)
    sql = emit(local, (), SOURCE).upper()
    for banned in ("AUTOINCREMENT", "JSONB", " TRUE", " FALSE", "INSERT OR REPLACE"):
        assert banned not in sql, banned


def test_a_quote_in_a_text_column_survives_the_round_trip(tmp_path):
    local = local_store(tmp_path)
    local.execute("UPDATE runs SET notes = ? WHERE run_id = 'tag'", ("it's a run",))
    local.commit()
    durable = durable_store(tmp_path)
    durable.executescript(emit(local, (), SOURCE))
    assert durable.execute("SELECT notes FROM runs").fetchone()[0] == "it's a run"


def test_a_value_with_no_portable_spelling_stops_the_mirror(tmp_path):
    """Coercing here would change the data on the durable side while looking
    exactly like a clean mirror, and the durable copy is the one that outlives
    the machine it can be checked against.

    Infinity rather than NaN, because SQLite stores a NaN as NULL and so cannot
    hand one to the emitter — an infinity it stores and returns intact.
    """
    local = local_store(tmp_path)
    local.execute("UPDATE trials SET cost_usd_norm = ?", (float("inf"),))
    local.commit()
    with pytest.raises(UnmirrorableValue):
        emit(local, (), SOURCE)


def test_a_nul_byte_stops_the_mirror_rather_than_being_dropped(tmp_path):
    """PostgreSQL rejects a NUL in a text column, and dropping it here would
    put different bytes on the durable side than the local store holds."""
    local = local_store(tmp_path)
    local.execute("UPDATE runs SET notes = ?", ("before\x00after",))
    local.commit()
    with pytest.raises(UnmirrorableValue):
        emit(local, (), SOURCE)


def test_two_emissions_of_an_unchanged_store_differ_only_in_their_stamp(tmp_path):
    """Ordered by primary key so a reviewer can diff two mirrors and see the
    rows that changed rather than a reshuffle."""
    local = local_store(tmp_path)
    body = [
        line
        for line in emit(local, (), SOURCE).splitlines()
        if not line.startswith("-- generated:")
    ]
    again = [
        line
        for line in emit(local, (), SOURCE).splitlines()
        if not line.startswith("-- generated:")
    ]
    assert body == again


def test_marking_the_local_rows_is_a_separate_step_from_emitting(tmp_path):
    """Verify before mark: emitting must not touch the local store, because
    the durable side has not been checked at the point the SQL is produced."""
    local = local_store(tmp_path)
    emit(local, (), SOURCE)
    assert local.execute("SELECT migrated, migration_source FROM runs").fetchone() == (
        0,
        None,
    )
    mark_migrated(local, (), SOURCE)
    assert local.execute("SELECT migrated, migration_source FROM runs").fetchone() == (
        1,
        SOURCE,
    )


def test_a_new_grader_versions_rows_travel_beside_the_old_ones(tmp_path):
    """The grade tables are versioned rather than overwritten, and the mirror
    has to carry that: two `grader_version` row sets for one trial are two
    rows, not a collision."""
    local = local_store(tmp_path)
    trial = "tag:stella:task__abcdefg"
    local.execute(
        "INSERT INTO execution_grades (execution_grade_id,trial_id,run_id,"
        "grader_version,graded_at) VALUES (?,?,?,?,?)",
        (f"{trial}:v2", trial, "tag", "v2-experimental", "2026-08-16T10:00:00Z"),
    )
    local.commit()
    durable = durable_store(tmp_path)
    durable.executescript(emit(local, (), SOURCE))
    assert [
        row[0]
        for row in durable.execute(
            "SELECT grader_version FROM execution_grades ORDER BY grader_version"
        )
    ] == ["v1-deterministic", "v2-experimental"]


def test_every_table_in_the_schema_is_carried(tmp_path):
    """A table added to `schema.sql` and not to `ALL_TABLES` would mirror
    silently short, which is the shape a count of rows cannot catch."""
    local = local_store(tmp_path)
    in_schema = {
        row[0]
        for row in local.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table'"
            " AND name NOT LIKE 'sqlite_%'"
        )
    }
    assert in_schema == set(ALL_TABLES)


def test_rows_of_reads_the_columns_from_the_store(tmp_path):
    """The emitter names columns off `PRAGMA table_info`, so a store carrying
    `ingest.py`'s migrated columns emits them without a second column list to
    keep in step."""
    local = local_store(tmp_path)
    assert len(rows_of(local, "runs", ())[0]) == len(
        [row[1] for row in local.execute("PRAGMA table_info(runs)")]
    )


def test_the_mirror_applies_to_a_store_that_predates_the_migration_columns(tmp_path):
    """A durable side written before `migrated`/`migration_source` existed gets
    them from `connect`'s migration, and the stamp then lands."""
    path = str(tmp_path / "old.db")
    old = sqlite3.connect(path)
    old.execute(
        "CREATE TABLE runs (run_id TEXT PRIMARY KEY, tag TEXT NOT NULL,"
        " kind TEXT NOT NULL, void_reason TEXT, model TEXT NOT NULL,"
        " api_surface TEXT, dataset_digest TEXT, sut_commit TEXT,"
        " binary_sha256 TEXT, taskset_sha256 TEXT, task_count INTEGER,"
        " prereg_json TEXT, preflight_text TEXT, started_at TEXT,"
        " finished_at TEXT, ingested_at TEXT NOT NULL, notes TEXT)"
    )
    old.commit()
    old.close()

    local = local_store(tmp_path)
    durable = connect(path)  # runs ingest.py's migrate()
    durable.executescript(emit(local, (), SOURCE))
    assert durable.execute(
        "SELECT migrated, migration_source FROM runs"
    ).fetchone() == (1, SOURCE)
