// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Copying what is still readable out of a corrupt store.
//!
//! `stella doctor --repair` used to salvage with `VACUUM INTO` alone, and
//! `VACUUM INTO` walks the b-tree — so it aborts the whole copy on the first
//! unreadable page it steps on and writes nothing. On the 1 GB store measured
//! for #5282 that meant **zero rows recovered** from a file whose damage was
//! six pages across four b-trees; `sqlite3 .recover`, run by hand, returned
//! 376,129 of 376,140 events and every row of every other table.
//!
//! # What runs, and in what order
//!
//! 1. **`VACUUM INTO`.** Kept as the first attempt, and it is the one to want:
//!    it produces an exact, defragmented copy including indexes and triggers.
//!    On an intact file, or damage confined to a free page, it succeeds and
//!    nothing below runs.
//! 2. **A per-table scan**, when the vacuum aborts. Each table is read on its
//!    own, so a damaged page in one b-tree costs that table's unreadable region
//!    and nothing else — the case the vacuum turned into total loss.
//!
//! # Why not `.recover`
//!
//! `.recover` is a feature of the **`sqlite3` shell**, not of the library, and
//! `rusqlite` exposes neither it nor the `dbdata` extension it is built on.
//! Three ways to get it, and why this is the one:
//!
//! - **Compile the extension in.** The most faithful — it rebuilds from
//!   surviving cell records and so reads past damage this scan cannot. It is
//!   also a C build dependency on every platform Stella ships, taken for a
//!   path that runs when a database has already broken. Worth revisiting if
//!   the scan proves insufficient in the field; not worth it first (#5354).
//! - **Shell out to a system `sqlite3`.** An external runtime dependency the
//!   crate does not otherwise take, absent on plenty of machines, and its
//!   absence would be discovered at the worst possible moment. `doctor` still
//!   prints the `by hand:` line, so a user who *has* the shell keeps the
//!   better tool; this is what runs when they do not.
//! - **A cell-record walk of our own.** Reimplementing SQLite's page format to
//!   read files SQLite itself declines to read. A large, subtle piece of work
//!   whose bugs are silently-wrong rows, which is worse than fewer rows.
//!
//! So the scan is the weaker option, chosen for costing no dependency: it
//! recovers less than `.recover` and much more than nothing, and it cannot
//! invent a row, because every row it writes came back from SQLite's own
//! decoder.
//!
//! # Safety posture, unchanged
//!
//! The source is opened immutably and never written. A failed salvage reports
//! and does not fail the quarantine. Nothing the user had is deleted — the only
//! file removed on failure is this module's own partial output.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use rusqlite::types::Value;

use super::restrict_to_owner;
use crate::{Result, StoreError, open_private_sqlite, open_private_sqlite_read_only};

/// Rows read per statement while a table is copying cleanly.
///
/// Large enough that an intact table costs few round trips, small enough that
/// one damaged page forfeits little before the scan drops to isolating rows.
const BATCH: usize = 512;

/// How far past a failure the scan probes before giving the table up.
///
/// Each step seeks to a different depth of the table's b-tree, so a probe that
/// lands beyond the damaged pages resumes the scan. Geometric because the size
/// of a damaged region is unknown and the cost of overshooting is only the rows
/// between the last good rowid and where it lands — which the next table pass
/// cannot recover either way.
const PROBES: [i64; 8] = [1, 8, 64, 512, 4_096, 32_768, 262_144, 2_097_152];

/// What one table gave up.
struct TableOutcome {
    name: String,
    copied: u64,
    /// `true` when the scan stopped before the end of the table.
    truncated: bool,
}

/// Salvage `source` into a file named beside `original`.
///
/// Returns the salvaged path, and a report string whenever the outcome is worth
/// a sentence — a scan that ran, or the reason nothing could be read. Both may
/// be present: a partial salvage is a file *and* a caveat.
pub(super) fn salvage(
    source: &Path,
    original: &Path,
    stamp: &str,
    unique_sibling: impl Fn(&Path, &str) -> Result<PathBuf>,
) -> (Option<PathBuf>, Option<String>) {
    let target = match unique_sibling(original, &format!("salvaged-{stamp}")) {
        Ok(target) => target,
        Err(error) => return (None, Some(error.to_string())),
    };
    let Some(target_str) = target.to_str() else {
        return (
            None,
            Some(format!(
                "salvage target {} is not valid UTF-8",
                target.display()
            )),
        );
    };

    let vacuum = open_private_sqlite_read_only(source).and_then(|conn| {
        // `VACUUM INTO` takes an expression, so the path binds as a parameter —
        // no quoting of a path that may contain apostrophes.
        conn.execute("VACUUM INTO ?1", [target_str])
            .map_err(|e| StoreError::Other(format!("VACUUM INTO failed: {e}")))
    });
    let vacuum_error = match vacuum {
        Ok(_) => return finish(target, None),
        Err(error) => error.to_string(),
    };

    // The vacuum leaves a partial file behind; it is this function's own output
    // path, never anything the user had.
    let _ = std::fs::remove_file(&target);

    match scan_tables(source, &target) {
        Ok(outcomes) if outcomes.iter().any(|t| t.copied > 0) => {
            finish(target, Some(report(&vacuum_error, &outcomes)))
        }
        Ok(_) => {
            let _ = std::fs::remove_file(&target);
            (
                None,
                Some(format!(
                    "{vacuum_error}; a per-table scan read no rows either"
                )),
            )
        }
        Err(error) => {
            let _ = std::fs::remove_file(&target);
            (None, Some(format!("{vacuum_error}; and then {error}")))
        }
    }
}

/// Apply the store's own file mode to a salvaged copy, or drop it.
///
/// The copy holds the same transcripts the original did. A mode that cannot be
/// set is a leak, not a cosmetic problem.
fn finish(target: PathBuf, note: Option<String>) -> (Option<PathBuf>, Option<String>) {
    if let Err(error) = restrict_to_owner(&target) {
        let _ = std::fs::remove_file(&target);
        return (None, Some(error.to_string()));
    }
    (Some(target), note)
}

/// One line naming what the scan got, and what it did not.
fn report(vacuum_error: &str, outcomes: &[TableOutcome]) -> String {
    let total: u64 = outcomes.iter().map(|t| t.copied).sum();
    let mut partial: Vec<&str> = outcomes
        .iter()
        .filter(|t| t.truncated)
        .map(|t| t.name.as_str())
        .collect();
    partial.sort_unstable();
    let tail = if partial.is_empty() {
        String::new()
    } else {
        format!("; unreadable past a point in: {}", partial.join(", "))
    };
    format!(
        "{vacuum_error}; recovered {total} row(s) across {} table(s) by scanning instead{tail}",
        outcomes.len()
    )
}

/// Copy every table `source` will still describe, row by readable row.
fn scan_tables(source: &Path, target: &Path) -> Result<Vec<TableOutcome>> {
    let src = open_private_sqlite_read_only(source)?;
    // The schema is the one read that has to work: without it there is nothing
    // to create and nothing to select from. It lives in the b-tree rooted at
    // page 1, which is why a file whose damage starts later still yields it.
    let schema = read_schema(&src)
        .map_err(|e| StoreError::Other(format!("the schema itself is unreadable: {e}")))?;

    let dst = open_private_sqlite(target)?;
    let mut outcomes = Vec::new();
    for (kind, name, sql) in &schema {
        if kind != "table" {
            continue;
        }
        // A table this connection refuses to create is one this scan cannot
        // fill; skip it rather than abandoning the tables after it.
        if dst.execute_batch(sql).is_err() {
            continue;
        }
        outcomes.push(copy_table(&src, &dst, name));
    }
    // Indexes, views and triggers last: they are derived, so a failure here
    // costs speed on the salvaged copy and never a row.
    for (kind, _, sql) in &schema {
        if kind != "table" {
            let _ = dst.execute_batch(sql);
        }
    }
    Ok(outcomes)
}

/// `(type, name, sql)` for everything the file still declares.
fn read_schema(src: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
    let mut stmt = src.prepare(
        "SELECT type, name, sql FROM sqlite_master \
         WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    rows.collect()
}

/// Copy one table, resuming past damage where a forward probe can find a way.
fn copy_table(src: &Connection, dst: &Connection, table: &str) -> TableOutcome {
    let mut outcome = TableOutcome {
        name: table.to_string(),
        copied: 0,
        truncated: false,
    };
    let Ok(columns) = column_count(src, table) else {
        outcome.truncated = true;
        return outcome;
    };
    if columns == 0 {
        return outcome;
    }
    let insert = format!(
        "INSERT OR IGNORE INTO \"{}\" VALUES ({})",
        table.replace('"', "\"\""),
        vec!["?"; columns].join(", ")
    );

    // `rowid` is how the scan resumes, so a WITHOUT ROWID table gets one
    // unresumable pass: what it yields before erroring is still more than the
    // vacuum's nothing.
    let Ok(mut after) = first_rowid_floor(src, table) else {
        outcome.truncated = !copy_whole_table(src, dst, table, &insert, &mut outcome.copied);
        return outcome;
    };

    let mut batch = BATCH;
    loop {
        match read_batch(src, table, columns, after, batch) {
            Ok(rows) if rows.is_empty() => return outcome,
            Ok(rows) => {
                for (rowid, values) in rows {
                    if dst
                        .execute(&insert, rusqlite::params_from_iter(values))
                        .is_ok()
                    {
                        outcome.copied += 1;
                    }
                    after = rowid;
                }
                // A batch that read cleanly means the scan is past the damage.
                batch = BATCH;
            }
            // Narrow the window before giving up on it: one bad page inside a
            // 512-row read must not forfeit the 511 readable rows around it.
            Err(_) if batch > 1 => batch = 1,
            Err(_) => {
                let Some(resumed) = probe_past(src, table, columns, after) else {
                    outcome.truncated = true;
                    return outcome;
                };
                after = resumed;
                batch = BATCH;
            }
        }
    }
}

/// The rowid to start before, or an error when the table has no rowid.
fn first_rowid_floor(src: &Connection, table: &str) -> rusqlite::Result<i64> {
    // Asking for the column rather than a row: an empty table is fine, a table
    // without rowids is what this has to detect, and neither needs data read.
    src.prepare(&format!(
        "SELECT rowid FROM \"{}\" LIMIT 0",
        table.replace('"', "\"\"")
    ))
    .map(|_| i64::MIN)
}

/// Read up to `limit` rows after `after`, newest rowid last.
#[allow(
    clippy::type_complexity,
    reason = "one call site; naming it would not \
     make the shape clearer than `(rowid, row values)`"
)]
fn read_batch(
    src: &Connection,
    table: &str,
    columns: usize,
    after: i64,
    limit: usize,
) -> rusqlite::Result<Vec<(i64, Vec<Value>)>> {
    let mut stmt = src.prepare(&format!(
        "SELECT rowid, * FROM \"{}\" WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
        table.replace('"', "\"\"")
    ))?;
    let rows = stmt.query_map(rusqlite::params![after, limit as i64], |row| {
        let rowid: i64 = row.get(0)?;
        let mut values = Vec::with_capacity(columns);
        for i in 0..columns {
            values.push(row.get::<_, Value>(i + 1)?);
        }
        Ok((rowid, values))
    })?;
    rows.collect()
}

/// Look for a rowid past the damage, returning the floor to resume from.
fn probe_past(src: &Connection, table: &str, columns: usize, after: i64) -> Option<i64> {
    for step in PROBES {
        let floor = after.saturating_add(step);
        if read_batch(src, table, columns, floor, 1).is_ok() {
            return Some(floor);
        }
    }
    None
}

/// One unresumable pass, for a table with no rowid to seek by.
///
/// Returns whether it reached the end.
fn copy_whole_table(
    src: &Connection,
    dst: &Connection,
    table: &str,
    insert: &str,
    copied: &mut u64,
) -> bool {
    let quoted = table.replace('"', "\"\"");
    let Ok(mut stmt) = src.prepare(&format!("SELECT * FROM \"{quoted}\"")) else {
        return false;
    };
    let Ok(mut rows) = stmt.query([]) else {
        return false;
    };
    loop {
        match rows.next() {
            Ok(None) => return true,
            Ok(Some(row)) => {
                let mut values = Vec::new();
                let mut i = 0;
                while let Ok(value) = row.get::<_, Value>(i) {
                    values.push(value);
                    i += 1;
                }
                if dst
                    .execute(insert, rusqlite::params_from_iter(values))
                    .is_ok()
                {
                    *copied += 1;
                }
            }
            Err(_) => return false,
        }
    }
}

/// How many columns the table declares.
fn column_count(src: &Connection, table: &str) -> rusqlite::Result<usize> {
    let mut stmt = src.prepare("SELECT COUNT(*) FROM pragma_table_info(?1)")?;
    stmt.query_row([table], |row| row.get::<_, i64>(0))
        .map(|n| n.max(0) as usize)
}
