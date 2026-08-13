// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The v23 → v24 schema migration: `tool_calls` learns *which kind* of error
//! a failed call was (#3145).
//!
//! Split out of [`super`] only because that module sits close under the
//! 1500-line ratchet and a step with a test deserves the room; the migration
//! itself is the ordinary column-guarded `ADD COLUMN` shape every additive
//! step in the ladder uses.

use super::{column_exists, table_exists};
use crate::Result;

/// v23 → v24: `tool_calls` grows `error_class` — the machine-readable class
/// of a failed call, projected from `ToolOutput::Error.class` (#3145).
///
/// # Why a column and not a decode at read time
///
/// The class rides on the `ToolOutput` in the `events` journal, so a reader
/// *could* re-derive it. But the whole point of this projection is that "what
/// is `bash`'s error rate, excluding model misuse?" is an index seek rather
/// than a JSON walk of history — a class that only exists inside the payload
/// leaves that query exactly as expensive as it was before the class existed.
///
/// # Why the default is the empty string
///
/// `''` is the store's spelling of `ErrorClass` = `None`: **unclassified**,
/// which is precisely what every pre-existing row is and what every
/// still-unaudited call site keeps writing. It is deliberately not a class of
/// its own — an unaudited site and a site audited as `internal` are different
/// facts, and collapsing them would make the ceiling this column exists to
/// enable count our unfinished audit as our defects. Non-empty only ever
/// holds a token `ErrorClass::parse` recognizes.
///
/// No backfill: the class of a call that failed before this build existed is
/// not recoverable from anywhere — the message prose is the only record, and
/// classifying by string match is the exact practice this column replaces.
pub(super) fn migrate_v23_to_v24(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if table_exists(tx, "tool_calls")? && !column_exists(tx, "tool_calls", "error_class")? {
        tx.execute_batch(
            "ALTER TABLE tool_calls ADD COLUMN error_class TEXT NOT NULL DEFAULT '';",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    /// A v23 file grows the column, and every row already in it reads as
    /// unclassified — never as a class, which would invent evidence.
    #[test]
    fn v24_migration_adds_an_unclassified_error_class_to_existing_rows() {
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE tool_calls (
               execution_id INTEGER NOT NULL, seq INTEGER NOT NULL,
               call_id TEXT NOT NULL DEFAULT '', name TEXT NOT NULL,
               surface TEXT NOT NULL DEFAULT 'native',
               args_json TEXT NOT NULL DEFAULT '{}',
               args_digest TEXT NOT NULL DEFAULT '',
               reason TEXT NOT NULL DEFAULT '',
               ok INTEGER NOT NULL DEFAULT 1,
               state TEXT NOT NULL DEFAULT 'ok',
               error TEXT NOT NULL DEFAULT '',
               bytes_out INTEGER NOT NULL DEFAULT 0,
               duration_ms INTEGER NOT NULL DEFAULT 0,
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               UNIQUE (execution_id, seq));
             INSERT INTO tool_calls (execution_id, seq, name, ok, state, error)
               VALUES (1, 0, 'bash', 0, 'error', 'command failed');",
        )
        .expect("v23 shape");

        apply_migration(&mut conn, migrate_v23_to_v24, 24).expect("migrate");

        let class: String = conn
            .query_row("SELECT error_class FROM tool_calls", [], |r| r.get(0))
            .expect("the column exists and the row survived");
        assert_eq!(
            class, "",
            "a call that failed before the class existed is unclassified, \
             not classified as anything"
        );

        // Idempotent: the column guard makes a second pass a no-op rather
        // than a duplicate-column error.
        apply_migration(&mut conn, migrate_v23_to_v24, 24).expect("second pass");
    }

    /// The step must survive a file whose executions all predate the
    /// data plane — `tool_calls` arrived at v7.
    #[test]
    fn v24_migration_is_a_no_op_without_the_tool_calls_table() {
        let mut conn = Connection::open_in_memory().expect("db");
        apply_migration(&mut conn, migrate_v23_to_v24, 24).expect("no data plane is fine");
    }
}
