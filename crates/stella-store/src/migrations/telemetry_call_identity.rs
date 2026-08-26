// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! v36 → v37: `telemetry.step` becomes `stream_seq`, and the table grows the
//! engine-side call identity it never recorded (#4924).
//!
//! # The rename
//!
//! The column was called `step` and held the event-stream `seq`. That value
//! is the right one — the seq is the execution-global call identity, while
//! the engine's `step` restarts on every `run_turn` and several calls can
//! share one, so `UNIQUE (execution_id, step)` under the engine's meaning
//! would collide and a collision here double-counts money. Only the name was
//! wrong, and it was wrong in the most expensive way a name can be: AGENTS.md
//! § Glossary exists because `step`, `turn_instance` and `call_seq` look
//! alike and are not, and this column was that hazard written into the
//! schema. `stella-cli`'s writer had a comment saying so at the assignment
//! and no reader ever saw it.
//!
//! `stream_seq` rather than `call_seq_global`, which the issue also floated:
//! `call_seq` is a real and *different* identifier, added to this same table
//! below, and two columns a prefix apart would be the same trap one turn of
//! the screw tighter.
//!
//! # The three new columns
//!
//! `step_receipt`'s primary key is `(execution_id, turn_instance, step,
//! call_seq)`. A telemetry row carried only the first of those, so a cost
//! could not be joined to the receipt of the call that produced it — the
//! reader had to go back to `stella-events.jsonl` and re-derive it. Since
//! #4793 the `step_usage` event carries `turn_instance` and `call_seq`
//! alongside its `step`, so the durable row can simply record what the event
//! already said.
//!
//! `engine_step` is `step_receipt.step` under a different spelling, because
//! this table's `step` meant a seq for thirty-six versions and reusing the
//! word immediately would be the same collision the rename exists to end.
//!
//! # Nullable, no default, no backfill
//!
//! NULL means **this row cannot say**. Every row written before v37 is in
//! that state, and so is a `usage_incomplete` receipt whose call died before
//! the engine could name a turn. Zero is not available as the absent value:
//! turn instances and call seqs are both 0-based, so a default of 0 would
//! assert that every legacy row was the worker call of turn 0. That is the
//! same argument `AgentEvent::StepUsage::call_seq`'s own doc makes for the
//! field being `Option` rather than a bare `u64`, and it has to hold on both
//! sides of the wire or the durable row claims more than the event did.
//!
//! There is no backfill, because nothing in the store recorded the fact. The
//! seq orders calls within an execution and says nothing about which turn
//! they rode; pairing a telemetry row with whichever receipt sorts alongside
//! it would be a guess dressed as a record — and the one case it would get
//! wrong is the auxiliary call sharing a step with the worker, which is
//! exactly the case `call_seq` was added to disambiguate.

use crate::Result;
use crate::migrations::{column_exists, table_exists};

/// Rename `telemetry.step` to `stream_seq` and add the three join columns,
/// each guarded so a file that already carries it is untouched.
///
/// `ALTER TABLE … RENAME COLUMN` also rewrites the `telemetry_by_model` index
/// and the `UNIQUE (execution_id, step)` constraint that name the column, so
/// the ladder does not have to rebuild the table to keep either.
pub(super) fn migrate_v36_to_v37(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !table_exists(tx, "telemetry")? {
        return Ok(());
    }
    if column_exists(tx, "telemetry", "step")? && !column_exists(tx, "telemetry", "stream_seq")? {
        tx.execute_batch("ALTER TABLE telemetry RENAME COLUMN step TO stream_seq;")?;
    }
    for column in ["turn_instance", "engine_step", "call_seq"] {
        if !column_exists(tx, "telemetry", column)? {
            tx.execute_batch(&format!(
                "ALTER TABLE telemetry ADD COLUMN {column} INTEGER;"
            ))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    /// A v36 file: the column still called `step`, the unique key and the
    /// covering index both naming it, and one row of real telemetry in it.
    fn v36_file() -> Connection {
        let conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE telemetry (
               execution_id INTEGER NOT NULL,
               step INTEGER NOT NULL,
               provider TEXT NOT NULL,
               model TEXT NOT NULL,
               cost_usd REAL NOT NULL,
               UNIQUE (execution_id, step)
             );
             CREATE INDEX telemetry_by_model
               ON telemetry(provider, model, execution_id, step);
             INSERT INTO telemetry (execution_id, step, provider, model, cost_usd)
               VALUES (7, 42, 'zai', 'glm-5.2', 0.25);",
        )
        .expect("seed a v36 file");
        conn
    }

    /// The seq survives the rename with its value intact. It was never the
    /// data that was wrong.
    #[test]
    fn the_seq_keeps_its_value_under_its_real_name() {
        let mut conn = v36_file();
        apply_migration(&mut conn, migrate_v36_to_v37, 37).expect("migrate");
        let seq: i64 = conn
            .query_row(
                "SELECT stream_seq FROM telemetry WHERE execution_id = 7",
                [],
                |r| r.get(0),
            )
            .expect("read back");
        assert_eq!(seq, 42);
    }

    /// `usage_stats` sums per execution, so a duplicate row double-counts
    /// real money — which makes `UNIQUE (execution_id, step)` the thing that
    /// stops it. The rename must carry the key over rather than drop it.
    #[test]
    fn the_uniqueness_that_stops_double_counting_survives_the_rename() {
        let mut conn = v36_file();
        apply_migration(&mut conn, migrate_v36_to_v37, 37).expect("migrate");
        let duplicate = conn.execute(
            "INSERT INTO telemetry (execution_id, stream_seq, provider, model, cost_usd) \
             VALUES (7, 42, 'zai', 'glm-5.2', 0.25)",
            [],
        );
        assert!(
            duplicate.is_err(),
            "a second row on the same (execution_id, stream_seq) must still be refused"
        );
    }

    /// And so does `drift_samples`'/`usage_stats`' covering access path,
    /// which named the old column in its own definition.
    #[test]
    fn the_covering_index_follows_the_column() {
        let mut conn = v36_file();
        apply_migration(&mut conn, migrate_v36_to_v37, 37).expect("migrate");
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'telemetry_by_model'",
                [],
                |r| r.get(0),
            )
            .expect("the index still exists");
        assert!(
            sql.contains("stream_seq"),
            "the index must name the renamed column, got: {sql}"
        );
    }

    /// Every pre-v37 row reads as unjoinable, which is the only reading the
    /// store can make: nothing recorded which turn the call rode.
    #[test]
    fn an_existing_row_cannot_say_which_receipt_it_belongs_to() {
        let mut conn = v36_file();
        apply_migration(&mut conn, migrate_v36_to_v37, 37).expect("migrate");
        let identity: (Option<i64>, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT turn_instance, engine_step, call_seq FROM telemetry WHERE execution_id = 7",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("read back");
        assert_eq!(
            identity,
            (None, None, None),
            "absence must be NULL, never 0 — turn 0 and call_seq 0 are real values"
        );
    }

    /// Running it twice is a no-op — the column guards, not the error
    /// handler, are what make that true.
    #[test]
    fn the_migration_is_idempotent() {
        let mut conn = v36_file();
        apply_migration(&mut conn, migrate_v36_to_v37, 37).expect("first");
        apply_migration(&mut conn, migrate_v36_to_v37, 37).expect("second");
    }

    /// A file with no `telemetry` table at all still climbs the ladder — the
    /// rebuild path can present exactly that shape.
    #[test]
    fn a_file_with_no_telemetry_table_still_migrates() {
        let mut conn = Connection::open_in_memory().expect("db");
        apply_migration(&mut conn, migrate_v36_to_v37, 37).expect("migrate");
    }
}
