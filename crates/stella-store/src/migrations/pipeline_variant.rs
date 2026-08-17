// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The v25 → v26 schema migration: `executions.kind` becomes the door and
//! only the door, and which wrapper ran moves to its own column (#3388).
//!
//! Split out of [`super`] for the same reason `error_class` was: that module
//! sits close under the 1500-line ratchet, and a step that carries a real
//! backfill plus its tests deserves the room.

use super::{column_exists, table_exists};
use crate::Result;

/// v25 → v26: add `executions.pipeline_variant`, and rewrite the two `kind`
/// values that named a wrapper instead of a door.
///
/// # The defect
///
/// `kind` is documented and read as the **door** — which command the user
/// ran. Two write sites disagreed: `stella run`'s pipeline path wrote
/// `'pipeline'`, and the Command Deck wrote `'deck-pipeline'` when the
/// pipeline was on. So a deck turn recorded a different door depending on
/// whether a wrapper ran, and one fact (which wrapper) was hiding inside
/// another (which door).
///
/// # Why the column and the rewrite are one step
///
/// Renaming alone would **destroy information**: after it, nothing would
/// record that those runs went through the pipeline at all. Adding the column
/// alone would leave two columns answering one question and disagreeing.
/// Neither half is safe on its own, so they are one migration.
///
/// # The backfill, and why it is exhaustive by mapping
///
/// Every `'pipeline'` row becomes `kind='run', pipeline_variant='classic'`,
/// and every `'deck-pipeline'` row becomes `kind='deck',
/// pipeline_variant='classic'`. `'classic'` is the name the staged pipeline
/// carries as a wrapper variant, and it is the only wrapper those rows could
/// have run under — there was no second one.
///
/// The `WHERE` clauses name the exact legacy values rather than anything
/// cleverer, because the set of values was read off the write sites in the
/// current tree. A value written by an older build that nothing in this tree
/// writes would not have been in that list, and a broad rewrite would corrupt
/// it. Naming the two we know keeps an unknown third untouched.
///
/// # What a NULL means afterwards
///
/// No wrapper ran. That is a fact, not an absence — the raw turn loop is the
/// ordinary case, and it must not read as "we forgot to record it". Rows that
/// predate this column and were not one of the two rewritten kinds are
/// genuinely unwrapped, so NULL is correct for them too.
pub(super) fn migrate_v25_to_v26(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !table_exists(tx, "executions")? {
        return Ok(());
    }
    if !column_exists(tx, "executions", "pipeline_variant")? {
        tx.execute_batch("ALTER TABLE executions ADD COLUMN pipeline_variant TEXT;")?;
    }
    // Idempotent by construction: after the first pass no row matches either
    // WHERE, so a re-run is a no-op rather than a second rewrite.
    tx.execute_batch(
        "UPDATE executions SET kind = 'run', pipeline_variant = 'classic' \
           WHERE kind = 'pipeline';
         UPDATE executions SET kind = 'deck', pipeline_variant = 'classic' \
           WHERE kind = 'deck-pipeline';",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    fn v25_executions(conn: &mut Connection) {
        conn.execute_batch(
            "CREATE TABLE executions (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               kind TEXT NOT NULL,
               prompt TEXT NOT NULL,
               provider TEXT NOT NULL,
               model TEXT NOT NULL,
               started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               finished_at TEXT,
               outcome TEXT,
               cost_usd REAL NOT NULL DEFAULT 0,
               session_id TEXT,
               usage_complete INTEGER NOT NULL DEFAULT 0,
               usage_status TEXT NOT NULL DEFAULT 'pending',
               journal_era INTEGER NOT NULL DEFAULT 0);
             INSERT INTO executions (kind, prompt, provider, model) VALUES
               ('pipeline', 'p', 'zai', 'glm-5.2'),
               ('deck-pipeline', 'p', 'zai', 'glm-5.2'),
               ('deck', 'p', 'zai', 'glm-5.2'),
               ('goal', 'p', 'zai', 'glm-5.2');",
        )
        .expect("seed");
    }

    fn rows(conn: &Connection) -> Vec<(String, Option<String>)> {
        let mut stmt = conn
            .prepare("SELECT kind, pipeline_variant FROM executions ORDER BY id")
            .expect("prepare");
        let mapped = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query");
        mapped.map(|r| r.expect("row")).collect()
    }

    /// The witness for #3388: a wrapped run and a wrapped deck turn keep the
    /// fact that a wrapper ran, while their `kind` becomes the door they
    /// actually came in by. Fails before this migration exists, where
    /// `kind` is the only record and it conflates the two.
    #[test]
    fn v26_moves_the_wrapper_out_of_the_door_column() {
        let mut conn = Connection::open_in_memory().expect("db");
        v25_executions(&mut conn);
        apply_migration(&mut conn, migrate_v25_to_v26, 26).expect("migrate");

        assert_eq!(
            rows(&conn),
            vec![
                ("run".to_string(), Some("classic".to_string())),
                ("deck".to_string(), Some("classic".to_string())),
                ("deck".to_string(), None),
                ("goal".to_string(), None),
            ]
        );
    }

    /// A deck turn that ran a wrapper and one that did not now record the
    /// SAME door and differ only in the variant — which is the whole point:
    /// before this, `GROUP BY kind` split one door into two.
    #[test]
    fn a_wrapped_and_an_unwrapped_deck_turn_share_one_door() {
        let mut conn = Connection::open_in_memory().expect("db");
        v25_executions(&mut conn);
        apply_migration(&mut conn, migrate_v25_to_v26, 26).expect("migrate");

        let decks: Vec<_> = rows(&conn).into_iter().filter(|r| r.0 == "deck").collect();
        assert_eq!(decks.len(), 2, "both deck turns count as the deck door");
        assert_eq!(decks[0].1.as_deref(), Some("classic"));
        assert_eq!(decks[1].1, None);
    }

    /// Re-running the ladder over an already-migrated file changes nothing.
    #[test]
    fn the_backfill_is_idempotent() {
        let mut conn = Connection::open_in_memory().expect("db");
        v25_executions(&mut conn);
        apply_migration(&mut conn, migrate_v25_to_v26, 26).expect("migrate");
        let after_first = rows(&conn);
        apply_migration(&mut conn, migrate_v25_to_v26, 26).expect("second pass");
        assert_eq!(rows(&conn), after_first);
    }

    /// A door value this tree does not write is left exactly as it was — the
    /// backfill names the two legacy values it knows rather than guessing at
    /// a general rule.
    #[test]
    fn an_unrecognised_kind_is_untouched() {
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE executions (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               kind TEXT NOT NULL, prompt TEXT NOT NULL,
               provider TEXT NOT NULL, model TEXT NOT NULL);
             INSERT INTO executions (kind, prompt, provider, model)
               VALUES ('some-older-build-value', 'p', 'zai', 'glm-5.2');",
        )
        .expect("seed");
        apply_migration(&mut conn, migrate_v25_to_v26, 26).expect("migrate");

        assert_eq!(
            rows(&conn),
            vec![("some-older-build-value".to_string(), None)]
        );
    }

    /// A file with no executions table at all is a no-op, not an error.
    #[test]
    fn a_file_with_no_executions_table_migrates_cleanly() {
        let mut conn = Connection::open_in_memory().expect("db");
        apply_migration(&mut conn, migrate_v25_to_v26, 26).expect("no data plane is fine");
    }
}
