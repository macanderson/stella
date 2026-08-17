// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The v17 → v18 schema migration: `tool_calls` stops being an end-of-turn
//! fold and becomes a LIVE projection.
//!
//! It sits beside the two later steps that keep reshaping the same column —
//! [`super::abandoned_state`] (the `'abandoned'` state, v23 → v24) and
//! [`super::error_class`] (v24 → v25) — rather than joining the additive
//! group, because this one is where `state` and its read paths were born and
//! the others only read as amendments to it.

use super::column_exists;
use crate::Result;

/// v17 → v18: `tool_calls` grows the lifecycle column the live projection
/// needs, plus the two indexes that projection reads through.
///
/// Every existing row describes a call that already finished, so `state`
/// backfills from `ok` and no row is left in a state the CHECK constraint
/// would reject. The column is added *without* the CHECK: SQLite's
/// `ADD COLUMN` cannot carry one that references the added column on an
/// existing table, so the constraint lives on the fresh-file DDL only and is
/// upheld here by the writer. That asymmetry is deliberate and cheap —
/// [`crate::Store`] is the only writer, and the alternative is a full table
/// rebuild (lang_altertable §7) to gain a constraint on a column this
/// migration is itself the sole populator of.
///
/// The unique index is partial (`WHERE call_id != ''`) so a legacy file
/// holding several rows with the empty pre-`call_id` default still upgrades
/// instead of failing to build the index — which would abort the migration
/// and leave the workspace unable to open its store at all. Any *real*
/// duplicate ids are collapsed first, keeping the earliest position, because
/// that is the row `materialize_tool_calls` would itself have kept.
pub(super) fn migrate_v17_to_v18(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "tool_calls", "state")? {
        tx.execute_batch("ALTER TABLE tool_calls ADD COLUMN state TEXT NOT NULL DEFAULT 'ok';")?;
        // Backfill from the boolean this column supersedes. Rows written
        // before v18 are all terminal — the only writer was the end-of-turn
        // fold — so none of them is 'running'.
        tx.execute_batch(
            "UPDATE tool_calls SET state = CASE WHEN ok = 1 THEN 'ok' ELSE 'error' END;",
        )?;
    }
    tx.execute_batch(
        "DELETE FROM tool_calls WHERE call_id != '' AND rowid NOT IN (
             SELECT min(rowid) FROM tool_calls WHERE call_id != ''
             GROUP BY execution_id, call_id
         );
         CREATE INDEX IF NOT EXISTS tool_calls_by_state
           ON tool_calls(state, execution_id, seq);
         CREATE UNIQUE INDEX IF NOT EXISTS tool_calls_by_call_id
           ON tool_calls(execution_id, call_id) WHERE call_id != '';
         CREATE INDEX IF NOT EXISTS executions_unfinished
           ON executions(id) WHERE finished_at IS NULL;",
    )?;
    Ok(())
}
