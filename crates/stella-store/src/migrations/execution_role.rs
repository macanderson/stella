// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The v26 → v27 schema migration: `executions.kind` stops carrying the four
//! **role** values written by standalone system model calls, and the role
//! moves to its own column (#3395).
//!
//! The sibling of [`super::pipeline_variant`] and the same shape of defect,
//! from the third direction. #3388 took the *wrapper* out of the door column;
//! this takes the *role* out of it.
//!
//! Split into its own file for the same reason that one was: `migrations.rs`
//! is at its ceiling, and a step carrying a real backfill plus its tests
//! deserves the room.

use super::{column_exists, table_exists};
use crate::Result;

/// The four `kind` values that were really roles, plus the ingest role that
/// joined them later.
///
/// Named exactly, never matched by a pattern. These were read off the write
/// sites in the current tree (`stella-cli`'s `accounted_call`,
/// `command_deck::authoring`, `command_deck::skills`, `domains`,
/// `ingest_cmd::extract`). A value written by an older build that nothing in
/// this tree writes was not in that list, and a broad rewrite would corrupt
/// it — the same argument #3388's backfill makes, and for the same reason.
pub(crate) const ROLE_KINDS: [&str; 5] = [
    "domain_inference",
    "agent_author",
    "reflection",
    "skill_author",
    "ingest_extraction",
];

/// The door value meaning "no door — this row is not a turn the user asked
/// for".
///
/// # Why a sentinel rather than NULL
///
/// #3395 prefers `kind = NULL` for a row belonging to no door. `kind` is
/// `TEXT NOT NULL` (see [`crate::ddl`]), so that spelling is unavailable
/// without rebuilding the table, and a rebuild of the busiest table in the
/// store is a large amount of risk bought for a representational preference.
///
/// The sentinel is safe here for the reason #3388 names: it must be
/// distinguishable from "unrecorded". Because the column is `NOT NULL`, there
/// is no unrecorded state to be confused with — every row has always had a
/// value — so `'system'` says exactly one thing, and it says it explicitly.
/// A door query excludes it by name; `role` then says which system call it
/// was.
pub(crate) const SYSTEM_NON_DOOR: &str = "system";

/// v26 → v27: add `executions.role`, and move the five role values out of
/// `kind`.
///
/// # The defect
///
/// `executions.kind` is the **door** — which command the user ran.
/// `accounted_call::complete_standalone` opened a row per standalone system
/// model call with `kind` set to the *role* of that call, so the column was
/// two-meaning: a door for turns, a role for system calls. Anything grouping
/// by door counted those rows as doors somebody typed, and nobody ran
/// `stella reflection`.
///
/// It matters now because #3381 adds per-variant comparison over this table.
/// A `GROUP BY kind` silently including role rows is a confounded
/// measurement — which is the failure #3388 exists to prevent, arriving from
/// the other side.
///
/// # Why the column and the rewrite are one step
///
/// Exactly as in #3388: rewriting `kind` alone would destroy the record of
/// which system call the row was, and adding `role` alone would leave two
/// columns answering one question and disagreeing. Neither half is safe
/// alone.
///
/// # What a NULL `role` means afterwards
///
/// This row is a turn, run through a door. That is the ordinary case and a
/// fact rather than an absence, so it must not read as "we forgot" — the
/// same reading `pipeline_variant`'s NULL carries.
pub(super) fn migrate_v26_to_v27(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !table_exists(tx, "executions")? {
        return Ok(());
    }
    if !column_exists(tx, "executions", "role")? {
        tx.execute_batch("ALTER TABLE executions ADD COLUMN role TEXT;")?;
    }
    // Idempotent by construction: after the first pass no row still has a
    // role value in `kind`, so a re-run matches nothing.
    for role in ROLE_KINDS {
        tx.execute(
            "UPDATE executions SET kind = ?1, role = ?2 WHERE kind = ?2",
            rusqlite::params![SYSTEM_NON_DOOR, role],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    fn v26_executions(conn: &mut Connection) {
        conn.execute_batch(
            "CREATE TABLE executions (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               kind TEXT NOT NULL,
               prompt TEXT NOT NULL,
               provider TEXT NOT NULL,
               model TEXT NOT NULL,
               pipeline_variant TEXT);
             INSERT INTO executions (kind, prompt, provider, model) VALUES
               ('reflection', 'p', 'zai', 'glm-5.2'),
               ('skill_author', 'p', 'zai', 'glm-5.2'),
               ('domain_inference', 'p', 'zai', 'glm-5.2'),
               ('agent_author', 'p', 'zai', 'glm-5.2'),
               ('ingest_extraction', 'p', 'zai', 'glm-5.2'),
               ('run', 'p', 'zai', 'glm-5.2'),
               ('deck', 'p', 'zai', 'glm-5.2');",
        )
        .expect("seed");
    }

    fn rows(conn: &Connection) -> Vec<(String, Option<String>)> {
        let mut stmt = conn
            .prepare("SELECT kind, role FROM executions ORDER BY id")
            .expect("prepare");
        let mapped = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query");
        mapped.map(|r| r.expect("row")).collect()
    }

    /// The witness for #3395: after this step, no role value survives in the
    /// door column, and every door query can be written without knowing the
    /// list of roles. Fails before the migration exists, where `kind` is the
    /// only record and conflates the two.
    #[test]
    fn v27_moves_the_role_out_of_the_door_column() {
        let mut conn = Connection::open_in_memory().expect("db");
        v26_executions(&mut conn);
        apply_migration(&mut conn, migrate_v26_to_v27, 27).expect("migrate");

        assert_eq!(
            rows(&conn),
            vec![
                ("system".to_string(), Some("reflection".to_string())),
                ("system".to_string(), Some("skill_author".to_string())),
                ("system".to_string(), Some("domain_inference".to_string())),
                ("system".to_string(), Some("agent_author".to_string())),
                ("system".to_string(), Some("ingest_extraction".to_string())),
                ("run".to_string(), None),
                ("deck".to_string(), None),
            ]
        );
    }

    /// The measurement this exists to protect: grouping by door no longer
    /// counts a system call as a door somebody typed.
    #[test]
    fn grouping_by_door_excludes_standalone_system_calls() {
        let mut conn = Connection::open_in_memory().expect("db");
        v26_executions(&mut conn);
        apply_migration(&mut conn, migrate_v26_to_v27, 27).expect("migrate");

        let doors: Vec<String> = rows(&conn)
            .into_iter()
            .filter(|(kind, _)| kind != SYSTEM_NON_DOOR)
            .map(|(kind, _)| kind)
            .collect();
        assert_eq!(doors, vec!["run".to_string(), "deck".to_string()]);
    }

    #[test]
    fn the_backfill_is_idempotent() {
        let mut conn = Connection::open_in_memory().expect("db");
        v26_executions(&mut conn);
        apply_migration(&mut conn, migrate_v26_to_v27, 27).expect("migrate");
        let after_first = rows(&conn);
        apply_migration(&mut conn, migrate_v26_to_v27, 27).expect("second pass");
        assert_eq!(rows(&conn), after_first);
    }

    /// A value this tree does not write is left exactly as it was.
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
        apply_migration(&mut conn, migrate_v26_to_v27, 27).expect("migrate");

        assert_eq!(
            rows(&conn),
            vec![("some-older-build-value".to_string(), None)]
        );
    }

    /// A file with no `executions` table at all is not an error.
    #[test]
    fn a_file_without_the_table_is_a_no_op() {
        let mut conn = Connection::open_in_memory().expect("db");
        apply_migration(&mut conn, migrate_v26_to_v27, 27).expect("migrate");
    }
}
