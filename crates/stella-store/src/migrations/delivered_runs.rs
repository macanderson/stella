// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! v37 → v38: `executions` grows `delivery` — whether the run shipped
//! anything, recorded apart from how it ended (#2808).
//!
//! `outcome` is one final label carrying two independent facts. How a run
//! ended — completed, aborted, cancelled, failed, error — says nothing about
//! whether it delivered, and a cancelled run can have pushed and merged a pull
//! request before the cancel landed. Execution 99 did exactly that: recorded
//! `cancelled`, and the truth was a $5.29 run that shipped a merged PR. Every
//! analysis filtering on `outcome = 'completed'` scores that as zero.
//!
//! # Nullable, no default, no backfill
//!
//! `NULL` means **nothing observed this run's delivery** — not "shipped
//! nothing". The two must stay apart, or the column answers the question the
//! `outcome` overload already answers badly: a reader must be able to tell
//! "ended early and shipped nothing" from "ended early and nobody looked".
//! Only a door that can see an attempt's commits writes here, so most rows are
//! `NULL` and will stay that way.
//!
//! There is no backfill because nothing recorded the fact. `pull_requests` is
//! keyed by URL and linked to a session, never to an execution; the fleet's
//! commit ledger lives in a different database (`fleet.db`) with no execution
//! id on it. Picking whichever run of a session was open when a PR appeared
//! would be a guess dressed as a record.
//!
//! Not to be confused with `execution_reflection.delivered`, which is the
//! model's own self-report about its turn. This column is an observation.

use crate::Result;
use crate::migrations::{column_exists, table_exists};

/// Add `executions.delivery`, column-guarded so a file that already has it is
/// untouched.
pub(super) fn migrate_v37_to_v38(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if table_exists(tx, "executions")? && !column_exists(tx, "executions", "delivery")? {
        tx.execute_batch("ALTER TABLE executions ADD COLUMN delivery TEXT;")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    fn v37_file() -> Connection {
        let conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE executions (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               kind TEXT NOT NULL,
               prompt TEXT NOT NULL,
               provider TEXT NOT NULL,
               model TEXT NOT NULL,
               outcome TEXT,
               session_id TEXT
             );
             INSERT INTO executions (kind, prompt, provider, model, outcome, session_id)
               VALUES ('fleet', 'fix the router', 'zai', 'glm-5.2', 'cancelled', 'ses-1');",
        )
        .expect("seed a v37 file");
        conn
    }

    /// A row written before the column existed reads as **unobserved**, never
    /// as "shipped nothing". Nothing looked at that run's delivery, and a
    /// default would convert an absence of evidence into a record of failure —
    /// which is the reading `outcome` already forces and the reason this column
    /// exists.
    #[test]
    fn a_run_from_before_the_column_reads_as_unobserved() {
        let mut conn = v37_file();
        apply_migration(&mut conn, migrate_v37_to_v38, 38).expect("migrate");
        let delivery: Option<String> = conn
            .query_row("SELECT delivery FROM executions WHERE id = 1", [], |r| {
                r.get(0)
            })
            .expect("read back");
        assert_eq!(delivery, None);
    }

    /// Running it twice is a no-op — the column guard, not the error handler,
    /// is what makes that true.
    #[test]
    fn the_migration_is_idempotent() {
        let mut conn = v37_file();
        apply_migration(&mut conn, migrate_v37_to_v38, 38).expect("first");
        apply_migration(&mut conn, migrate_v37_to_v38, 38).expect("second");
    }

    /// A file with no `executions` table at all still climbs the ladder — the
    /// rebuild path can present exactly that shape.
    #[test]
    fn a_file_with_no_executions_table_still_migrates() {
        let mut conn = Connection::open_in_memory().expect("db");
        apply_migration(&mut conn, migrate_v37_to_v38, 38).expect("migrate");
    }
}
