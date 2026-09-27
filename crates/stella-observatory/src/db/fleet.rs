// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The fleet ledger projection behind `/api/fleet`. It sits beside `db.rs`
//! because that file is near the 1500-line cap, the same way `db/recall.rs`
//! does. Each run carries a `name`, which is the title of the first task the
//! run recorded. `stella fleet` titles a task with the short session name it
//! draws from the task's prompt, so the dashboard can list a run by that name
//! and keep the run id for a tooltip.

use rusqlite::Connection;
use serde_json::{Value, json};

use super::{DbError, collect_rows};

/// The 50 newest fan-out runs, each with its name and its counts.
///
/// `name` is `null` for a run that recorded no task. The ledger has no name
/// column, so the name comes from `tasks`. The first task by `rowid` is the
/// first one the plan recorded, which is the task the operator typed first.
pub(super) fn fleet_runs(conn: &Connection) -> Result<Vec<Value>, DbError> {
    collect_rows(
        conn,
        "SELECT r.id, r.root_task_count, r.created_at_ms,
                (SELECT count(*) FROM attempts a WHERE a.run_id = r.id),
                (SELECT count(*) FROM attempts a
                  WHERE a.run_id = r.id AND a.success = 1),
                (SELECT count(*) FROM commits c WHERE c.run_id = r.id),
                (SELECT t.title FROM tasks t
                  WHERE t.run_id = r.id ORDER BY t.rowid LIMIT 1)
         FROM runs r ORDER BY r.created_at_ms DESC LIMIT 50",
        |r| {
            Ok(json!({
                "run_id": r.get::<_, String>(0)?,
                "name": r.get::<_, Option<String>>(6)?,
                "tasks": r.get::<_, i64>(1)?,
                "created_at_ms": r.get::<_, i64>(2)?,
                "attempts": r.get::<_, i64>(3)?,
                "succeeded": r.get::<_, i64>(4)?,
                "commits": r.get::<_, i64>(5)?,
            }))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four ledger tables this projection reads, in the shape
    /// `stella-fleet`'s first migration creates them.
    fn ledger() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory ledger");
        conn.execute_batch(
            "CREATE TABLE runs (id TEXT PRIMARY KEY, root_task_count INTEGER NOT NULL,
                                created_at_ms INTEGER NOT NULL);
             CREATE TABLE tasks (run_id TEXT NOT NULL, task_id TEXT NOT NULL,
                                 title TEXT NOT NULL, isolation TEXT NOT NULL,
                                 PRIMARY KEY (run_id, task_id));
             CREATE TABLE attempts (id INTEGER PRIMARY KEY AUTOINCREMENT,
                                    run_id TEXT NOT NULL, task_id TEXT NOT NULL,
                                    success INTEGER);
             CREATE TABLE commits (id INTEGER PRIMARY KEY AUTOINCREMENT,
                                   run_id TEXT NOT NULL, task_id TEXT NOT NULL);",
        )
        .expect("ledger schema");
        conn
    }

    /// A run is named by the first task it recorded, even when a later task
    /// sorts first by id. A run with no task has a `null` name, so the page
    /// falls back to the run id.
    #[test]
    fn a_run_is_named_by_its_first_recorded_task() {
        let conn = ledger();
        conn.execute_batch(
            "INSERT INTO runs VALUES ('run-a', 2, 2000), ('run-b', 0, 1000);
             INSERT INTO tasks VALUES
                ('run-a', 't2', 'Fix conflicts on PR 123', 'isolated'),
                ('run-a', 't1', 'Update the changelog', 'isolated');
             INSERT INTO attempts (run_id, task_id, success) VALUES
                ('run-a', 't2', 1), ('run-a', 't1', 0);
             INSERT INTO commits (run_id, task_id) VALUES ('run-a', 't2');",
        )
        .expect("rows");

        let runs = fleet_runs(&conn).expect("fleet runs");

        assert_eq!(runs.len(), 2, "{runs:?}");
        assert_eq!(runs[0]["run_id"], "run-a");
        assert_eq!(runs[0]["name"], "Fix conflicts on PR 123");
        assert_eq!(runs[0]["attempts"], 2);
        assert_eq!(runs[0]["succeeded"], 1);
        assert_eq!(runs[0]["commits"], 1);
        assert_eq!(runs[1]["run_id"], "run-b");
        assert_eq!(runs[1]["name"], Value::Null);
    }
}
