// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Semantic file vectors: the render that turns a file into text, the storage
//! that keeps its vector beside the node it describes, and the ranking pass.
//!
//! # Why this exists
//!
//! `graph_query` could answer "where is `run_turn` defined" and could not
//! answer "which file strips secrets before logging", so the agent answered
//! the second question by *guessing names* — a run of greps for
//! `redact|scrub|sanitize|mask` until one hit. Measured on Terminal-Bench 2.1:
//! one task spent 13 of its 58 tool calls grepping a single file for spelling
//! variants of a concept. That is a semantic question being asked of a
//! substring index.
//!
//! # The shape
//!
//! - **Two passes, one mechanism.** This module answers "which files have no
//!   vector" and stores the ones a caller comes back with; *when* that happens
//!   is the caller's decision, and there are two callers. `stella init` embeds
//!   the tree it just indexed, up front, so the first tool call of a run can
//!   already be answered — the pass that matters for a single-turn run, where
//!   there is no second session to amortise an index over. The per-query pass
//!   then tops up **incrementally**, [`MAX_FILES_PER_PASS`] files at a time,
//!   for everything written after init: the agent's own edits, a file added
//!   later, a workspace that never ran init at all. Neither pass is a second
//!   way for a vector to exist — both render through [`render_file_text`] and
//!   both write the same `(file_id, fingerprint)` row — and the cost is paid
//!   once per `(file content, embedder)` pair either way.
//! - **One render, used everywhere.** [`render_file_text`] is the only way a
//!   file becomes embeddable text. A mismatch between what was indexed and
//!   what a later pass would index degrades ranking silently, with no error to
//!   catch it, so there is exactly one function and its output is what the
//!   stored `content_sha256` refers to.
//! - **A file is one ranking unit, and no longer the only one.** This module
//!   held that ranking symbols directly "would multiply the corpus by twenty
//!   for an answer the caller then has to re-aggregate to a file anyway".
//!   Measurement refuted it: one vector per file returned a **0.05 score
//!   spread across ten results** on a real query, with the answer absent
//!   entirely (#3089). Multiplying the corpus by twenty is exactly what buys
//!   the resolution, and [`chunks`] does it. A file vector still answers
//!   "what is this module about", which no symbol can, so both rungs stay.
//! - **The fingerprint is the invalidation.** A vector is keyed
//!   `(file_id, fingerprint)` and carries the content hash it was computed
//!   from, so changing the embedder or the file content makes the old vector
//!   invisible and re-embeds on next touch — never a mixed vector space.

use std::path::Path;

use rusqlite::{Connection, Transaction, params};
use stella_embed::rank::{Candidate, Scored};

use crate::error::GraphError;
use crate::store;

/// The most files one semantic query will embed before answering. The cap is
/// the difference between "the first query is a little slow" and "the first
/// query indexes a monorepo": a caller that finds work still pending says so
/// in its answer, and the next query picks up where this one stopped.
pub const MAX_FILES_PER_PASS: usize = 200;

/// How much of a file's content reaches the embedder. Every hosted model has
/// an input limit (8k tokens is the tightest in common use) and a whole large
/// file dilutes its own vector besides. The head of the file plus the symbol
/// names is where a file says what it is about.
const MAX_CONTENT_CHARS: usize = 6_000;

/// How many symbol names are named in the render. Enough to characterise a
/// module, bounded so a generated file with two thousand symbols cannot crowd
/// out its own content.
const MAX_SYMBOL_NAMES: usize = 60;

/// A file that has no current vector for the active fingerprint, together with
/// the exact text to embed and the hash that text must be stored under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEmbed {
    /// Workspace-relative, forward-slash path.
    pub path: String,
    /// sha256 of the file's raw content, as the index recorded it. Stored with
    /// the resulting vector so a later content change invalidates it.
    pub content_sha256: String,
    /// The rendered text to embed — [`render_file_text`]'s output.
    pub text: String,
}

/// What one [`pending`] scan found: the files it can offer, and how many it
/// had to step over to fill the window.
///
/// The count is not decoration. It is the only place anything knows that a
/// file the index holds could not be turned into text, and a caller that
/// reports "N files left unembedded" without it states the cap as the reason
/// when the reason was a file it could not read (#3016).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingScan {
    /// Files ready to embed, most recently changed first (`path` breaking
    /// ties), at most `limit` of them.
    pub files: Vec<PendingEmbed>,
    /// Indexed files stepped over because their content could not be read —
    /// deleted since the index pass, unreadable by this process, or not UTF-8.
    /// Counted over the rows this scan actually walked, so a scan that stopped
    /// at `limit` reports what it saw on the way, not a repository total.
    pub unreadable: usize,
}

/// A vector ready to be written back, as produced by an [`stella_embed::Embedder`].
#[derive(Debug, Clone, PartialEq)]
pub struct FileVector {
    /// Workspace-relative, forward-slash path.
    pub path: String,
    /// The content hash the vector was computed from.
    pub content_sha256: String,
    /// The vector.
    pub vector: Vec<f32>,
}

/// Turn a file into the text an embedder sees.
///
/// Path first, then the symbol names it defines, then the content: a model
/// reading `crates/stella-store/src/telemetry.rs` plus `drain`, `flush`,
/// `Sink` has most of what it needs before a single line of body. Deliberately
/// plain — no headings a tokenizer spends budget on, no ordering that depends
/// on anything but the caller's, which is the index's `start_line` order.
///
/// Pure: no I/O, no clock, no environment. The same inputs render the same
/// bytes forever, which is what makes the stored `content_sha256` a valid
/// cache key for this text.
pub fn render_file_text(rel_path: &str, symbol_names: &[String], content: &str) -> String {
    let mut out = String::with_capacity(MAX_CONTENT_CHARS + 512);
    out.push_str("file: ");
    out.push_str(rel_path);
    out.push('\n');

    if !symbol_names.is_empty() {
        out.push_str("symbols: ");
        for (index, name) in symbol_names.iter().take(MAX_SYMBOL_NAMES).enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str(name);
        }
        out.push('\n');
    }

    out.push('\n');
    // `char_indices` rather than a byte slice: truncating a UTF-8 sequence
    // would make the render depend on where the cut landed.
    match content.char_indices().nth(MAX_CONTENT_CHARS) {
        Some((byte, _)) => out.push_str(&content[..byte]),
        None => out.push_str(content),
    }
    out
}

/// Up to `limit` files with no usable vector under `fingerprint`, rendered and
/// ready to embed.
///
/// "No usable vector" is either no row at all or a row whose `content_sha256`
/// no longer matches the file's — the same `(content, fingerprint)` skip the
/// parser uses, applied to embedding.
///
/// # Ordering: most recently changed first
///
/// Ordered by `(indexed_at DESC, path ASC)` — **not** by path alone, which is
/// what it used to be and which had a defect this cap makes severe. A pass
/// embeds at most `limit` files; under path ordering those are the
/// alphabetically first pending files, forever. A merge that touched
/// `src/zone.rs` in a workspace with thousands of pending files near the front
/// of the alphabet therefore left that file unembedded across an unbounded
/// number of queries — semantically invisible, with nothing reporting it as
/// missing beyond a `PARTIAL INDEX` note that named the cap rather than the
/// file.
///
/// `indexed_at` is written by `upsert_file` and the byte-compat skip means it
/// moves only when a file's **content** actually changed. So this ordering is
/// "what changed most recently, first" — which is exactly the set a commit,
/// merge, or checkout just produced, derived from state the index already
/// keeps rather than from a priority queue that could disagree with it. There
/// is no queue to drain, invalidate, or leak.
///
/// The `path ASC` tiebreak is load-bearing, not decoration: a fresh
/// `stella init` stamps a whole tree within the same second, so without it the
/// order inside a timestamp would be SQLite's choice and a capped pass could
/// revisit files it already did instead of resuming past them.
///
/// A file the index knows about but that has since vanished from disk is
/// skipped, not an error: the next `index_all` prunes its row. The window
/// **fills past those skips** rather than spending itself on them, walking a
/// path cursor until it has `limit` embeddable files or the pending set runs
/// out. Truncating in SQL and filtering afterwards is what it used to do, and
/// a run of unreadable files ahead of the readable ones then returned nothing
/// while thousands still pended — which the eager `stella init` pass read as
/// "done" and stopped on (#3016). The cursor keeps that fix inside the one
/// function both passes share; the path ordering, and so the resume, is
/// unchanged.
pub fn pending(
    conn: &Connection,
    root: &Path,
    fingerprint: &str,
    limit: usize,
) -> Result<PendingScan, GraphError> {
    let mut stmt = conn.prepare(
        "SELECT f.path, f.content_sha256, f.indexed_at \
         FROM code_graph_files f \
         LEFT JOIN code_graph_vectors v \
           ON v.file_id = f.id AND v.fingerprint = ?1 \
         WHERE (v.file_id IS NULL OR v.content_sha256 <> f.content_sha256) \
           AND (f.indexed_at < ?2 OR (f.indexed_at = ?2 AND f.path > ?3)) \
         ORDER BY f.indexed_at DESC, f.path ASC \
         LIMIT ?4",
    )?;

    let mut scan = PendingScan::default();
    // The cursor is the (indexed_at, path) of the last row handed out, and it
    // starts past the end of the ordering in both components: `i64::MAX` is
    // above every timestamp the indexer can write, and the empty string sorts
    // before every non-empty path under SQLite's BINARY collation — the same
    // collation `ORDER BY f.path` uses. So the first round starts at the
    // newest pending file and walks backwards through time.
    let mut cursor_indexed_at = i64::MAX;
    let mut cursor_path = String::new();
    while scan.files.len() < limit {
        let want = limit - scan.files.len();
        let rows: Vec<(String, String, i64)> = stmt
            .query_map(
                params![fingerprint, cursor_indexed_at, cursor_path, want as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?
            .collect::<Result<_, _>>()?;
        // No rows past the cursor: the pending set is exhausted, and whatever
        // is still unembedded is what this scan stepped over.
        let Some((last_path, _, last_indexed_at)) = rows.last() else {
            break;
        };
        cursor_path = last_path.clone();
        cursor_indexed_at = *last_indexed_at;

        for (path, content_sha256, _) in rows {
            let Ok(content) = std::fs::read_to_string(root.join(&path)) else {
                scan.unreadable += 1;
                continue;
            };
            let names: Vec<String> = store::symbols_in_file(conn, &path)?
                .into_iter()
                .map(|def| def.name)
                .collect();
            scan.files.push(PendingEmbed {
                path: path.clone(),
                content_sha256,
                text: render_file_text(&path, &names, &content),
            });
        }
    }
    Ok(scan)
}

/// Write vectors back under `fingerprint`, replacing whatever was there.
///
/// One transaction for the whole batch, matching the indexer's `L-L1`
/// contract: a process killed mid-write has committed nothing and the next
/// pass finds the same files pending. A row whose path is no longer indexed is
/// skipped rather than resurrecting a file the graph has dropped.
pub fn store_vectors(
    conn: &mut Connection,
    fingerprint: &str,
    rows: &[FileVector],
) -> Result<usize, GraphError> {
    let tx = conn.transaction()?;
    let written = write_vectors(&tx, fingerprint, rows)?;
    tx.commit()?;
    Ok(written)
}

fn write_vectors(
    tx: &Transaction<'_>,
    fingerprint: &str,
    rows: &[FileVector],
) -> Result<usize, GraphError> {
    let mut lookup = tx.prepare_cached("SELECT id FROM code_graph_files WHERE path = ?1")?;
    let mut upsert = tx.prepare_cached(
        "INSERT INTO code_graph_vectors\
           (file_id, fingerprint, content_sha256, dims, vector, embedded_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(file_id, fingerprint) DO UPDATE SET \
           content_sha256 = excluded.content_sha256, \
           dims           = excluded.dims, \
           vector         = excluded.vector, \
           embedded_at    = excluded.embedded_at",
    )?;

    let now = store::now_unix();
    let mut written = 0usize;
    for row in rows {
        let file_id: Option<i64> = lookup.query_row(params![row.path], |r| r.get(0)).ok();
        let Some(file_id) = file_id else {
            continue;
        };
        upsert.execute(params![
            file_id,
            fingerprint,
            row.content_sha256,
            row.vector.len() as i64,
            encode(&row.vector),
            now,
        ])?;
        written += 1;
    }
    Ok(written)
}

/// Every stored vector under `fingerprint`, as `(path, vector)`.
///
/// The whole set, brute-force: a workspace's file count is in the thousands
/// and a dot product over a few hundred dimensions is nanoseconds, so an ANN
/// structure here would be complexity bought against a cost nobody is paying.
/// `stella-context` has one for its far larger corpus; this is a different
/// scale and gets a different answer.
pub fn stored(conn: &Connection, fingerprint: &str) -> Result<Vec<(String, Vec<f32>)>, GraphError> {
    let mut stmt = conn.prepare(
        "SELECT f.path, v.vector \
         FROM code_graph_vectors v JOIN code_graph_files f ON f.id = v.file_id \
         WHERE v.fingerprint = ?1 \
         ORDER BY f.path",
    )?;
    let rows = stmt.query_map(params![fingerprint], |row| {
        let path: String = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        Ok((path, decode(&blob)))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Rank the stored vectors under `fingerprint` against `query`.
///
/// The ranking itself is [`stella_embed::rank::top_k`] — pure, total, and
/// property-tested there. This function's only job is to get the vectors out
/// of SQLite and hand them over, which is exactly the invariant-2 split: the
/// decision is testable without a database, and the I/O has no decision in it.
pub fn rank(
    conn: &Connection,
    fingerprint: &str,
    query: &[f32],
    floor: f32,
    limit: usize,
) -> Result<Vec<Scored>, GraphError> {
    let stored = stored(conn, fingerprint)?;
    let candidates: Vec<Candidate<'_>> = stored
        .iter()
        .map(|(path, vector)| Candidate {
            key: path.as_str(),
            vector: vector.as_slice(),
        })
        .collect();
    Ok(stella_embed::rank::top_k(query, &candidates, floor, limit))
}

/// How many files carry a vector under `fingerprint`.
pub fn count(conn: &Connection, fingerprint: &str) -> Result<usize, GraphError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM code_graph_vectors WHERE fingerprint = ?1",
        params![fingerprint],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as usize)
}

/// Vectors held under a fingerprint that is not the active one (#3652).
///
/// A vector is keyed `(file_id, fingerprint)` precisely so two embedders
/// never share a vector space, which means switching model does not
/// invalidate the old vectors — it makes them **invisible**. Invisible is not
/// gone: nothing deletes them, nothing counts them, and a 1536-dimension f32
/// vector is about 6 KiB per file, so a workspace that has tried three models
/// silently carries two full corpora it can never read.
///
/// Reported rather than swept, and that is a deliberate decision rather than
/// an unfinished one. Sweeping automatically would destroy the exact property
/// the coexisting key buys: a user moving between two models — comparing
/// them, or switching back after a bad result — would pay a full re-embed in
/// each direction. So this answers the question and leaves the spending
/// decision with whoever is paying for it.
///
/// Ordered by count descending, then fingerprint, so a report is stable.
pub fn retired_fingerprints(
    conn: &Connection,
    active: &str,
) -> Result<Vec<(String, usize)>, GraphError> {
    // Both tables, unioned: chunk vectors outnumber file vectors by roughly
    // the symbol-per-file ratio, so a report that counted only the file rung
    // would understate the reclaimable space by an order of magnitude.
    let mut stmt = conn.prepare(
        "SELECT fingerprint, SUM(n) FROM ( \
           SELECT fingerprint, COUNT(*) AS n FROM code_graph_vectors \
             WHERE fingerprint <> ?1 GROUP BY fingerprint \
           UNION ALL \
           SELECT fingerprint, COUNT(*) AS n FROM code_graph_chunk_vectors \
             WHERE fingerprint <> ?1 GROUP BY fingerprint \
         ) GROUP BY fingerprint \
         ORDER BY SUM(n) DESC, fingerprint",
    )?;
    let rows = stmt
        .query_map(params![active], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as usize,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Delete every vector held under `fingerprint`, returning how many rows went.
///
/// One transaction across both tables: a half-swept fingerprint is a state
/// nothing else in this module knows how to describe, and the whole point of
/// the operation is reclaiming space, which a partial result does not do.
///
/// Refuses to touch the active fingerprint. Sweeping the vectors currently in
/// use is never what a caller means by "reclaim retired space", and doing it
/// would silently bill them for a full re-embed on the next query.
pub fn prune_fingerprint(
    conn: &mut Connection,
    fingerprint: &str,
    active: &str,
) -> Result<usize, GraphError> {
    if fingerprint == active {
        return Ok(0);
    }
    let tx = conn.transaction()?;
    let mut removed = tx.execute(
        "DELETE FROM code_graph_vectors WHERE fingerprint = ?1",
        params![fingerprint],
    )?;
    removed += tx.execute(
        "DELETE FROM code_graph_chunk_vectors WHERE fingerprint = ?1",
        params![fingerprint],
    )?;
    tx.commit()?;
    Ok(removed)
}

/// Little-endian f32, fixed so a `codegraph.db` copied between machines ranks
/// identically. SQLite's own float storage would cost a row per dimension.
fn encode(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// The inverse of [`encode`]. A trailing partial float is dropped rather than
/// producing a garbage dimension: a truncated blob can only come from a
/// damaged store, and the ranker already treats a wrong-width vector as
/// incomparable (score `0.0`) instead of comparing across vector spaces.
fn decode(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

pub mod chunks;

#[cfg(test)]
mod tests;
