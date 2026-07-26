//! Episode and memory records — the two non-graph record tables the context
//! plane owns. Both are mirrored to `Memory`/`Episode` nodes by the write-back
//! path, which is what makes them retrievable.
//!
//! Split out of the store module unchanged (#712 deliverable 1).

use rusqlite::{Connection, params};

use crate::error::ContextError;

/// Insert or update an episode (idempotent by `public_id`). Returns rowid.
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_episode(
    conn: &Connection,
    public_id: &str,
    summary: &str,
    files_touched: &serde_json::Value,
    outcome: &str,
    salience: f64,
    started_at: &str,
    ended_at: &str,
    now: &str,
) -> Result<i64, ContextError> {
    let files = serde_json::to_string(files_touched)?;
    let id: i64 = conn.query_row(
        "INSERT INTO episode (public_id, summary, files_touched, outcome, salience, started_at, ended_at, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(public_id) DO UPDATE SET
             summary = excluded.summary,
             files_touched = excluded.files_touched,
             outcome = excluded.outcome,
             salience = excluded.salience
         RETURNING id",
        params![public_id, summary, files, outcome, salience, started_at, ended_at, now],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// Insert or update a memory record (idempotent by `public_id`). Returns rowid.
pub(crate) fn insert_memory(
    conn: &Connection,
    public_id: &str,
    kind: &str,
    content: &str,
    salience: f64,
    now: &str,
) -> Result<i64, ContextError> {
    let id: i64 = conn.query_row(
        "INSERT INTO memory (public_id, kind, content, salience, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(public_id) DO UPDATE SET
             kind = excluded.kind,
             content = excluded.content,
             salience = excluded.salience
         RETURNING id",
        params![public_id, kind, content, salience, now],
        |r| r.get(0),
    )?;
    Ok(id)
}
