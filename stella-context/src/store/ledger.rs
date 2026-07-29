//! The lifecycle ledger — append-only, immutable, hashed records.
//!
//! This is the storage half of adaptive-context spec §6.3. It is deliberately
//! **untyped at this layer**: a row carries a kind string, a canonical JSON
//! body, and the ADR 0004 hash over that body. The typed record model lives in
//! `stella-core::context_record`, which `stella-context` does not depend on and
//! must not start depending on — the context plane owns storage, not the record
//! taxonomy, and inverting that would drag the whole domain model into the
//! database crate.
//!
//! ## What "append-only" means here
//!
//! Not a convention. `context.db`'s v8 migration (`migrate_v8`) installs
//! `BEFORE UPDATE` and
//! `BEFORE DELETE` triggers that abort, so the guarantee holds against every
//! writer including a future one that has forgotten this module exists. The
//! only way to change what a record says is to append a new revision naming the
//! old one in `supersedes`.
//!
//! ## Why appending the same record twice is a no-op, not an error
//!
//! Observation extraction replays. The reflection log is append-only and re-read
//! in full every turn; a cursor bounds the work but cannot make it exactly-once
//! across a crash between "record written" and "cursor advanced". So a repeat
//! append of a byte-identical record succeeds silently, and only a repeat with a
//! *different* hash under the same id is an error — that one means two different
//! records claimed one identity, which is a genuine defect and must not be
//! swallowed.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::ContextError;

/// One record to append. Borrowed throughout: the caller already owns the
/// canonical bytes, and a ledger append should not cost a round of clones.
#[derive(Debug, Clone, Copy)]
pub struct LedgerAppend<'a> {
    /// Identity of **this revision**. Callers derive it deterministically from
    /// content so a replay produces the same id.
    pub record_id: &'a str,
    /// Durable identity across revisions. Equals `record_id` for a first
    /// revision.
    pub lineage_id: &'a str,
    /// `ContextRecordKind::as_str()` — `observation`, `record_proposal`,
    /// `promotion_event`, …
    pub record_kind: &'a str,
    /// The ADR 0004 canonical hash (`sha256:<64 hex>`) over `body`.
    pub record_hash: &'a str,
    /// The record schema version the body was written against.
    pub schema_version: &'a str,
    /// The record's canonical JSON.
    pub body: &'a str,
    /// When the thing the record describes was observed (RFC 3339 UTC).
    pub observed_at: &'a str,
    /// The `record_id` this revision replaces, if any.
    pub supersedes: Option<&'a str>,
}

/// One record read back out of the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerRecord {
    /// Identity of this revision.
    pub record_id: String,
    /// Durable identity across revisions.
    pub lineage_id: String,
    /// The record kind's canonical `snake_case` spelling.
    pub record_kind: String,
    /// The ADR 0004 canonical hash over `body`.
    pub record_hash: String,
    /// The record schema version `body` was written against.
    pub schema_version: String,
    /// The record's canonical JSON.
    pub body: String,
    /// When the described thing was observed (RFC 3339 UTC).
    pub observed_at: String,
    /// When the ledger accepted the record (RFC 3339 UTC).
    pub recorded_at: String,
    /// The `record_id` this revision replaces, if any.
    pub supersedes: Option<String>,
}

/// What an append did — reported rather than inferred, so a caller can tell a
/// genuine new record from a replay without re-querying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    /// The record was new and is now in the ledger.
    Appended,
    /// A byte-identical record was already present. Nothing was written.
    AlreadyPresent,
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<LedgerRecord> {
    Ok(LedgerRecord {
        record_id: row.get("record_id")?,
        lineage_id: row.get("lineage_id")?,
        record_kind: row.get("record_kind")?,
        record_hash: row.get("record_hash")?,
        schema_version: row.get("schema_version")?,
        body: row.get("body")?,
        observed_at: row.get("observed_at")?,
        recorded_at: row.get("recorded_at")?,
        supersedes: row.get("supersedes")?,
    })
}

const SELECT_COLUMNS: &str = "record_id, lineage_id, record_kind, record_hash, \
     schema_version, body, observed_at, recorded_at, supersedes";

/// Append one record. Idempotent by `record_id` for identical content; a
/// conflicting hash under an existing id is an error.
pub(crate) fn append(
    conn: &Connection,
    record: LedgerAppend<'_>,
    now: &str,
) -> Result<AppendOutcome, ContextError> {
    let changed = conn.execute(
        "INSERT INTO context_records
             (record_id, lineage_id, record_kind, record_hash, schema_version,
              body, observed_at, recorded_at, supersedes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(record_id) DO NOTHING",
        params![
            record.record_id,
            record.lineage_id,
            record.record_kind,
            record.record_hash,
            record.schema_version,
            record.body,
            record.observed_at,
            now,
            record.supersedes,
        ],
    )?;
    if changed == 1 {
        return Ok(AppendOutcome::Appended);
    }
    // The id was taken. A replay of the same record is expected and fine; two
    // different records under one id is a defect and must be loud.
    let existing: Option<String> = conn
        .query_row(
            "SELECT record_hash FROM context_records WHERE record_id = ?1",
            params![record.record_id],
            |r| r.get(0),
        )
        .optional()?;
    match existing {
        Some(hash) if hash == record.record_hash => Ok(AppendOutcome::AlreadyPresent),
        Some(hash) => Err(ContextError::InvalidInput(format!(
            "ledger record `{}` already exists with hash {hash}, but a different \
             record claiming the same id was appended (hash {}) — two records \
             cannot share one identity",
            record.record_id, record.record_hash
        ))),
        // The row vanished between the insert and the read, which the
        // append-only triggers make impossible. Treat as appended rather than
        // inventing a failure mode.
        None => Ok(AppendOutcome::Appended),
    }
}

/// One record by id.
pub(crate) fn record_by_id(
    conn: &Connection,
    record_id: &str,
) -> Result<Option<LedgerRecord>, ContextError> {
    Ok(conn
        .query_row(
            &format!("SELECT {SELECT_COLUMNS} FROM context_records WHERE record_id = ?1"),
            params![record_id],
            row_to_record,
        )
        .optional()?)
}

/// Every record of one kind, oldest first. Ordered by `observed_at` then
/// `record_id` so the tiebreak is explicit rather than rowid-shaped — spec §5.3
/// requires every ordering tie to have a documented resolution, and a ledger
/// read whose order depends on insertion history is not replayable.
pub(crate) fn records_of_kind(
    conn: &Connection,
    record_kind: &str,
    limit: usize,
) -> Result<Vec<LedgerRecord>, ContextError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM context_records
         WHERE record_kind = ?1
         ORDER BY observed_at ASC, record_id ASC
         LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![record_kind, limit as i64], row_to_record)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every record of one kind in **append order** — the order the ledger accepted
/// them.
///
/// Distinct from [`records_of_kind`], and the distinction is load-bearing.
/// `observed_at` is the wall clock of the *described event*, at second
/// granularity, and `record_id` is a content hash — so two decisions made in the
/// same second sort by hash, which is to say arbitrarily. A replay that folds
/// "last write wins" over that order gets the wrong answer roughly half the time,
/// and does so deterministically, which is worse than flaky.
///
/// Append order is the authority for "what happened last" in an append-only log.
/// `rowid` is SQLite's own monotonic insertion counter and the table is never
/// updated or deleted from, so it cannot be reused or reordered.
pub(crate) fn records_of_kind_in_append_order(
    conn: &Connection,
    record_kind: &str,
    limit: usize,
) -> Result<Vec<LedgerRecord>, ContextError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM context_records
         WHERE record_kind = ?1
         ORDER BY rowid ASC
         LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![record_kind, limit as i64], row_to_record)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The **newest** `limit` records of one kind, returned oldest-first
/// (`observed_at ASC`) — the recency-window counterpart of [`records_of_kind`].
///
/// A `limit`-bounded read of an append-only log that grows without bound: the
/// plain [`records_of_kind`] returns the OLDEST `limit`, so a "current state"
/// fold over it freezes at the first `limit` events once the log grows past the
/// bound (new records become invisible forever). A recency fold must instead
/// read the newest window. Selection is by `observed_at DESC` (tiebroken by
/// `record_id`, the same documented resolution [`records_of_kind`] uses); the
/// result is reversed so callers still fold in ascending order.
pub(crate) fn records_of_kind_newest(
    conn: &Connection,
    record_kind: &str,
    limit: usize,
) -> Result<Vec<LedgerRecord>, ContextError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM context_records
         WHERE record_kind = ?1
         ORDER BY observed_at DESC, record_id DESC
         LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![record_kind, limit as i64], row_to_record)?;
    let mut out: Vec<LedgerRecord> = rows.collect::<rusqlite::Result<_>>()?;
    out.reverse();
    Ok(out)
}

/// The **newest** `limit` records of one kind in **append order** — the
/// recency-window counterpart of [`records_of_kind_in_append_order`], carrying
/// the same load-bearing `rowid` ordering (a last-write-wins fold needs append
/// order, not `observed_at`). Selects by `rowid DESC LIMIT` then reverses, so
/// the result is the newest `limit` records in ascending append order.
pub(crate) fn records_of_kind_newest_in_append_order(
    conn: &Connection,
    record_kind: &str,
    limit: usize,
) -> Result<Vec<LedgerRecord>, ContextError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM context_records
         WHERE record_kind = ?1
         ORDER BY rowid DESC
         LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![record_kind, limit as i64], row_to_record)?;
    let mut out: Vec<LedgerRecord> = rows.collect::<rusqlite::Result<_>>()?;
    out.reverse();
    Ok(out)
}

/// Every revision in one lineage, oldest first — the audit trail for a single
/// logical record.
pub(crate) fn records_for_lineage(
    conn: &Connection,
    lineage_id: &str,
) -> Result<Vec<LedgerRecord>, ContextError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM context_records
         WHERE lineage_id = ?1
         ORDER BY recorded_at ASC, record_id ASC"
    ))?;
    let rows = stmt.query_map(params![lineage_id], row_to_record)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// How many records of each kind the ledger holds — the reported number ADR
/// 0010 asks for, so the two-regime debt stays visible rather than silent.
pub(crate) fn record_counts(conn: &Connection) -> Result<Vec<(String, u64)>, ContextError> {
    let mut stmt = conn.prepare(
        "SELECT record_kind, COUNT(*) FROM context_records
         GROUP BY record_kind ORDER BY record_kind ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
    })?;
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .collect())
}

/// Read an extraction cursor. `None` means this source has never been consumed.
pub(crate) fn extraction_cursor(
    conn: &Connection,
    source: &str,
) -> Result<Option<String>, ContextError> {
    Ok(conn
        .query_row(
            "SELECT position FROM context_extraction_cursor WHERE source = ?1",
            params![source],
            |r| r.get(0),
        )
        .optional()?)
}

/// Advance an extraction cursor. Unlike `context_records` this table IS mutable
/// — it is bookkeeping about progress, not a record of what was believed, and a
/// cursor that could only move by appending would need its own compaction.
pub(crate) fn set_extraction_cursor(
    conn: &Connection,
    source: &str,
    position: &str,
    now: &str,
) -> Result<(), ContextError> {
    conn.execute(
        "INSERT INTO context_extraction_cursor (source, position, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(source) DO UPDATE SET
             position = excluded.position,
             updated_at = excluded.updated_at",
        params![source, position, now],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ContextStore;

    fn store() -> (tempfile::TempDir, ContextStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ContextStore::open(dir.path().join("context.db")).expect("open");
        (dir, store)
    }

    fn append<'a>(record_id: &'a str, body: &'a str, hash: &'a str) -> LedgerAppend<'a> {
        LedgerAppend {
            record_id,
            lineage_id: record_id,
            record_kind: "observation",
            record_hash: hash,
            schema_version: "1.0-draft",
            body,
            observed_at: "2026-07-26T12:00:00Z",
            supersedes: None,
        }
    }

    #[test]
    fn a_record_round_trips() {
        let (_dir, store) = store();
        assert_eq!(
            store
                .append_record(append("obs_1", r#"{"a":1}"#, "sha256:aa"))
                .expect("append"),
            AppendOutcome::Appended
        );
        let read = store.record_by_id("obs_1").expect("read").expect("present");
        assert_eq!(read.body, r#"{"a":1}"#);
        assert_eq!(read.record_hash, "sha256:aa");
        assert_eq!(read.lineage_id, "obs_1");
        assert!(read.supersedes.is_none());
        assert!(!read.recorded_at.is_empty(), "recorded_at is stamped");
    }

    #[test]
    fn newest_in_append_order_returns_the_last_n_not_the_first() {
        // #818: a "current state" fold must read the newest window, not the
        // oldest — the oldest-N read froze such folds at the first N events once
        // the append-only log grew past the bound. Append five, read the newest
        // three: they must be the LAST three appended, in ascending append
        // order. A revert to `records_of_kind_in_append_order` (oldest N) fails.
        let (_dir, store) = store();
        for i in 1..=5 {
            let id = format!("obs_{i}");
            let hash = format!("sha256:{i}");
            store
                .append_record(append(&id, r#"{"a":1}"#, &hash))
                .expect("append");
        }
        let newest = store
            .records_of_kind_newest_in_append_order("observation", 3)
            .expect("read");
        let ids: Vec<&str> = newest.iter().map(|r| r.lineage_id.as_str()).collect();
        assert_eq!(ids, ["obs_3", "obs_4", "obs_5"]);
    }

    #[test]
    fn newest_by_observed_at_selects_the_recent_window_not_the_oldest() {
        // #818: `records_of_kind_newest` selects the most-recent `limit` by
        // `observed_at` (returned oldest-first), where `records_of_kind`
        // returns the oldest `limit`. With distinct timestamps the two reads
        // pick opposite ends, so this pins the recency direction.
        let (_dir, store) = store();
        let ts = [
            "2026-01-01T00:00:01Z",
            "2026-01-01T00:00:02Z",
            "2026-01-01T00:00:03Z",
            "2026-01-01T00:00:04Z",
        ];
        for (i, t) in ts.iter().enumerate() {
            let id = format!("obs_{}", i + 1);
            let hash = format!("sha256:{}", i + 1);
            store
                .append_record(LedgerAppend {
                    record_id: &id,
                    lineage_id: &id,
                    record_kind: "observation",
                    record_hash: &hash,
                    schema_version: "1.0-draft",
                    body: r#"{"a":1}"#,
                    observed_at: t,
                    supersedes: None,
                })
                .expect("append");
        }
        let newest: Vec<String> = store
            .records_of_kind_newest("observation", 2)
            .expect("read")
            .iter()
            .map(|r| r.lineage_id.clone())
            .collect();
        assert_eq!(newest, ["obs_3", "obs_4"], "newest two, oldest-first");
        let oldest: Vec<String> = store
            .records_of_kind("observation", 2)
            .expect("read")
            .iter()
            .map(|r| r.lineage_id.clone())
            .collect();
        assert_eq!(
            oldest,
            ["obs_1", "obs_2"],
            "plain read still takes the oldest"
        );
    }

    /// Replay-idempotence, at the storage layer. Extraction re-reads an
    /// append-only log every turn and cannot be exactly-once across a crash
    /// between the record write and the cursor advance, so a byte-identical
    /// repeat has to be a no-op rather than a duplicate or an error.
    #[test]
    fn appending_an_identical_record_twice_is_a_no_op() {
        let (_dir, store) = store();
        store
            .append_record(append("obs_1", r#"{"a":1}"#, "sha256:aa"))
            .expect("first");
        assert_eq!(
            store
                .append_record(append("obs_1", r#"{"a":1}"#, "sha256:aa"))
                .expect("replay"),
            AppendOutcome::AlreadyPresent
        );
        assert_eq!(
            store
                .records_of_kind("observation", 100)
                .expect("list")
                .len(),
            1
        );
    }

    /// But two *different* records claiming one identity is a real defect and
    /// must be loud — swallowing it would let a hash collision or an id-derivation
    /// bug silently drop records.
    #[test]
    fn a_conflicting_record_under_an_existing_id_is_rejected() {
        let (_dir, store) = store();
        store
            .append_record(append("obs_1", r#"{"a":1}"#, "sha256:aa"))
            .expect("first");
        let err = store
            .append_record(append("obs_1", r#"{"a":2}"#, "sha256:bb"))
            .expect_err("a different record under the same id must be refused");
        assert!(err.to_string().contains("obs_1"), "{err}");
    }

    /// The append-only guarantee is enforced by the database, not by this
    /// module's discipline. A future writer that never read these docs still
    /// cannot rewrite history.
    #[test]
    fn the_ledger_rejects_updates_and_deletes_at_the_database() {
        let (dir, store) = store();
        store
            .append_record(append("obs_1", r#"{"a":1}"#, "sha256:aa"))
            .expect("append");
        drop(store);

        let conn = rusqlite::Connection::open(dir.path().join("context.db")).expect("reopen");
        let update = conn.execute(
            "UPDATE context_records SET body = '{\"a\":99}' WHERE record_id = 'obs_1'",
            [],
        );
        assert!(update.is_err(), "an UPDATE was allowed against the ledger");
        let delete = conn.execute("DELETE FROM context_records WHERE record_id = 'obs_1'", []);
        assert!(delete.is_err(), "a DELETE was allowed against the ledger");

        // And the row is untouched.
        let body: String = conn
            .query_row(
                "SELECT body FROM context_records WHERE record_id = 'obs_1'",
                [],
                |r| r.get(0),
            )
            .expect("still there");
        assert_eq!(body, r#"{"a":1}"#);
    }

    /// A correction is an append naming its predecessor, which the triggers
    /// permit — immutability is not the same as unchangeable.
    #[test]
    fn a_revision_supersedes_its_predecessor_by_appending() {
        let (_dir, store) = store();
        store
            .append_record(append("prp_1", r#"{"v":1}"#, "sha256:aa"))
            .expect("first");
        store
            .append_record(LedgerAppend {
                record_id: "prp_2",
                lineage_id: "prp_1",
                record_kind: "record_proposal",
                record_hash: "sha256:bb",
                schema_version: "1.0-draft",
                body: r#"{"v":2}"#,
                observed_at: "2026-07-26T13:00:00Z",
                supersedes: Some("prp_1"),
            })
            .expect("revision");

        let lineage = store.records_for_lineage("prp_1").expect("lineage");
        assert_eq!(lineage.len(), 2, "both revisions survive: {lineage:#?}");
        assert_eq!(lineage[1].supersedes.as_deref(), Some("prp_1"));
    }

    /// Ordering is explicit rather than rowid-shaped: a ledger read whose order
    /// depends on insertion history is not replayable (spec §5.3).
    #[test]
    fn records_of_a_kind_are_ordered_by_time_then_id() {
        let (_dir, store) = store();
        for (id, at) in [
            ("obs_c", "2026-07-26T10:00:00Z"),
            ("obs_a", "2026-07-26T10:00:00Z"),
            ("obs_b", "2026-07-26T09:00:00Z"),
        ] {
            store
                .append_record(LedgerAppend {
                    record_id: id,
                    lineage_id: id,
                    record_kind: "observation",
                    record_hash: "sha256:aa",
                    schema_version: "1.0-draft",
                    body: "{}",
                    observed_at: at,
                    supersedes: None,
                })
                .expect("append");
        }
        let ids: Vec<String> = store
            .records_of_kind("observation", 100)
            .expect("list")
            .into_iter()
            .map(|r| r.record_id)
            .collect();
        assert_eq!(ids, vec!["obs_b", "obs_a", "obs_c"]);
    }

    #[test]
    fn counts_are_reported_per_kind() {
        let (_dir, store) = store();
        store
            .append_record(append("obs_1", "{}", "sha256:aa"))
            .expect("a");
        store
            .append_record(LedgerAppend {
                record_kind: "record_proposal",
                ..append("prp_1", "{}", "sha256:bb")
            })
            .expect("b");
        assert_eq!(
            store.record_counts().expect("counts"),
            vec![
                ("observation".to_string(), 1),
                ("record_proposal".into(), 1)
            ]
        );
    }

    #[test]
    fn an_extraction_cursor_starts_absent_and_then_advances() {
        let (_dir, store) = store();
        assert_eq!(store.extraction_cursor("reflections").expect("read"), None);
        store
            .set_extraction_cursor("reflections", "42")
            .expect("set");
        assert_eq!(
            store.extraction_cursor("reflections").expect("read"),
            Some("42".into())
        );
        store
            .set_extraction_cursor("reflections", "99")
            .expect("advance");
        assert_eq!(
            store.extraction_cursor("reflections").expect("read"),
            Some("99".into())
        );
    }
}
