// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Which spelling of a store column this file uses.
//!
//! An observer opens every store `SQLITE_OPEN_READ_ONLY` and migrates none of
//! them, so a workspace that has not run a turn since an upgrade is still on
//! the old shape. `db.rs`'s [`collect_rows_degrading`] handles a column that
//! is simply *missing*; this module handles the other case — a column that is
//! present under a different name — which degrading to `NULL` would answer
//! wrongly.
//!
//! Its own file rather than more lines in `db.rs`, which sits within fifty
//! lines of the 1500-line ceiling (AGENTS.md § "God files — plan around them,
//! never into them").
//!
//! [`collect_rows_degrading`]: crate::db

use rusqlite::Connection;

/// What this store calls `telemetry`'s event-stream seq column.
///
/// `stream_seq` since store schema v37; `step` before it, where the name said
/// "the engine's step" and the column held a seq the whole time (#4924).
/// Both spellings are live at once for as long as un-migrated stores exist,
/// and blanking a turn's whole Steps table over a column name would be the
/// worst answer available.
///
/// Answers with the current name when the table cannot be read at all, so a
/// missing `telemetry` degrades through the caller rather than here.
pub(crate) fn telemetry_seq_column(conn: &Connection) -> &'static str {
    let legacy = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('telemetry') WHERE name = 'step'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0);
    if legacy > 0 { "step" } else { "stream_seq" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(column: &str) -> Connection {
        let conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(&format!(
            "CREATE TABLE telemetry (execution_id INTEGER NOT NULL, {column} INTEGER NOT NULL);"
        ))
        .expect("seed");
        conn
    }

    #[test]
    fn a_migrated_store_reads_the_current_name() {
        assert_eq!(
            telemetry_seq_column(&store_with("stream_seq")),
            "stream_seq"
        );
    }

    /// The case this module exists for: a store nobody has run a turn against
    /// since the rename still answers, rather than losing its Steps table.
    #[test]
    fn a_store_older_than_the_rename_reads_the_old_name() {
        assert_eq!(telemetry_seq_column(&store_with("step")), "step");
    }

    /// No `telemetry` table at all is the caller's problem, not this one's —
    /// it answers with the current name and lets the read degrade there.
    #[test]
    fn a_store_with_no_telemetry_table_answers_the_current_name() {
        let conn = Connection::open_in_memory().expect("db");
        assert_eq!(telemetry_seq_column(&conn), "stream_seq");
    }
}
