//! Episode and memory records — the two non-graph record tables the context
//! plane owns. Both are mirrored to `Memory`/`Episode` nodes by the write-back
//! path, which is what makes them retrievable.
//!
//! Split out of the store module unchanged (#712 deliverable 1).

use rusqlite::{Connection, OptionalExtension, params};

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

/// Write a memory **revision** under `lineage_id`, closing whatever revision of
/// that lineage was live. Idempotent by `public_id`. Returns rowid.
///
/// The supersede runs first and excludes this revision's own id, so re-writing
/// identical content is still the plain upsert it always was — it must not
/// close and immediately reopen the row it is updating.
///
/// Nothing is deleted (`L-C3`): a superseded revision keeps its text and its
/// timestamps, which is what lets a lineage's history be read back.
pub(crate) fn insert_memory(
    conn: &Connection,
    public_id: &str,
    lineage_id: &str,
    kind: &str,
    content: &str,
    salience: f64,
    now: &str,
) -> Result<i64, ContextError> {
    conn.execute(
        "UPDATE memory SET superseded_at = ?3
         WHERE lineage_id = ?1 AND public_id <> ?2 AND superseded_at IS NULL",
        params![lineage_id, public_id, now],
    )?;
    let id: i64 = conn.query_row(
        "INSERT INTO memory (public_id, lineage_id, kind, content, salience, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(public_id) DO UPDATE SET
             lineage_id = excluded.lineage_id,
             kind = excluded.kind,
             content = excluded.content,
             salience = excluded.salience,
             superseded_at = NULL
         RETURNING id",
        params![public_id, lineage_id, kind, content, salience, now],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// One revision of a memory lineage — what the memory said, and when it
/// stopped saying it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRevision {
    /// This revision's own `mem_…` id.
    pub id: String,
    /// The text this revision carried.
    pub content: String,
    /// When it was written.
    pub recorded_at: String,
    /// When a later revision replaced it; `None` for the current one.
    pub superseded_at: Option<String>,
}

impl MemoryRevision {
    /// Whether this is the lineage's current revision.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.superseded_at.is_none()
    }
}

/// Every revision of `lineage_id`, newest first.
///
/// The read behind "what did this memory used to say". A lineage has exactly
/// one revision with a null `superseded_at`; the rest are its history.
pub(crate) fn memory_revisions(
    conn: &Connection,
    lineage_id: &str,
) -> Result<Vec<MemoryRevision>, ContextError> {
    let mut stmt = conn.prepare(
        "SELECT public_id, content, recorded_at, superseded_at
         FROM memory WHERE lineage_id = ?1
         ORDER BY recorded_at DESC, id DESC",
    )?;
    let mut out = Vec::new();
    for row in stmt.query_map(params![lineage_id], |r| {
        Ok(MemoryRevision {
            id: r.get(0)?,
            content: r.get(1)?,
            recorded_at: r.get(2)?,
            superseded_at: r.get(3)?,
        })
    })? {
        out.push(row?);
    }
    Ok(out)
}

/// The `kind` of a lineage's current revision, if it has one.
///
/// An edit reads this so a revision inherits its lineage's classification
/// instead of defaulting to one — silently turning a reflection into a note
/// would change which surfaces treat it as mined-and-regenerable.
pub(crate) fn memory_kind(
    conn: &Connection,
    lineage_id: &str,
) -> Result<Option<String>, ContextError> {
    let kind = conn
        .query_row(
            "SELECT kind FROM memory WHERE lineage_id = ?1 AND superseded_at IS NULL
             ORDER BY recorded_at DESC, id DESC LIMIT 1",
            params![lineage_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(kind)
}
