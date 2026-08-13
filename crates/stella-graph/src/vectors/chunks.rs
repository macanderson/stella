// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Chunk vectors: the pieces a file is made of, embedded one at a time.
//!
//! # The measurement
//!
//! [`super`] embeds one vector per **file**. Asked *"where is the prompt cache
//! engaged on outbound requests"* against this workspace, that index returned
//! ten files scoring 0.656 down to 0.604 — a **0.05 spread across ten
//! results**, which is a tie wearing a ranking's clothes — and the file that
//! actually engages the cache was not among them. Its *test file* was, at rank
//! ten.
//!
//! Neither half of that is a tuning problem. Averaging a 900-line multi-topic
//! file into one 1024-dimension vector destroys the distinctions a query is
//! asking about; `render_file_text` already concedes it by truncating at
//! `MAX_CONTENT_CHARS` because "a whole large file dilutes its own vector
//! besides". This module embeds the parts instead: **27,150 symbols against
//! 1,431 files**, a 19x sharper index over text the indexer has already read.
//!
//! A sharper index is also what makes a *relevance threshold* possible at all.
//! A cutoff over scores spanning 0.05 admits everything or nothing; there is
//! no line to draw until the vectors spread.
//!
//! # A chunk is not a symbol
//!
//! It is anything with a file, a name, a kind and a line span — a function, a
//! markdown section (this crate's `markdown` module), and later a table
//! definition. They share one table because they share one **vector space**:
//! the same embedder under the same fingerprint, so a doc section's score and
//! a function's score are genuinely comparable rather than two scales glued
//! together. That is the difference between this and the strategy ladder in
//! `search`, where fusing scores would make the answer's `via:` line a lie.
//!
//! # Why the key is a content hash
//!
//! `store::index_one` replaces a changed file's symbols wholesale — one
//! `DELETE ... WHERE file_id = ?` and fresh inserts — so `code_graph_symbols.id`
//! is not stable across a re-parse. Keying a vector on it (with the
//! `ON DELETE CASCADE` such a key implies) would discard every vector in a
//! file whenever any line of that file changed. Hashing the chunk's own
//! rendered text instead means an untouched function in an edited file is
//! **re-stamped, not re-bought**, which is the whole point of an incremental
//! pass. See the DDL comment in `store.rs` for the full argument.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, Transaction, params};
use stella_embed::rank::{Candidate, Scored};

use crate::error::GraphError;
use crate::store;

#[cfg(test)]
mod tests;

/// How many *files* one pass will chunk-embed. Files rather than chunks
/// because a file's chunks are written as one unit: the sweep that deletes
/// vanished chunks keys on the file hash, so a file split across two passes
/// would have its first half swept away by its second.
pub const MAX_FILES_PER_CHUNK_PASS: usize = 64;

/// How much of a chunk's body reaches the embedder.
///
/// Smaller than the whole-file cap on purpose: a chunk is already
/// the unit of meaning, so the cap is a guard against one pathological
/// generated function rather than the summarisation device it has to be for a
/// whole file. Roughly 1,000 tokens — past any hand-written function.
const MAX_CHUNK_CHARS: usize = 4_000;

/// One chunk that needs attention on this pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChunk {
    /// The chunk's name — a symbol name, or a markdown section's breadcrumb.
    pub name: String,
    /// [`crate::SymbolKind::tag`], carried as text so the vector table does
    /// not have to know this crate's enum.
    pub kind: String,
    /// 1-based, inclusive, as everywhere else in this crate.
    pub start_line: u32,
    pub end_line: u32,
    /// sha256 of [`render_chunk_text`]'s output — the vector's identity.
    pub chunk_sha256: String,
    /// The text to embed, or `None` when a vector for this exact
    /// `(file, chunk_sha256, fingerprint)` already exists and the row only
    /// needs re-stamping with the file's current hash. **`None` is the
    /// incremental win**: it is a chunk that survived an edit to its file.
    pub text: Option<String>,
}

/// One file's chunks, all of them, as a single unit of work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChunkFile {
    /// Workspace-relative, forward-slash path.
    pub path: String,
    /// The file's current content hash — what the stored rows get stamped
    /// with, and what the sweep compares against.
    pub file_sha256: String,
    /// Every chunk the file currently has, in `start_line` order. Chunks
    /// whose `text` is `None` need no embedding.
    pub chunks: Vec<PendingChunk>,
}

impl PendingChunkFile {
    /// How many of this file's chunks actually need an embedder round trip.
    #[must_use]
    pub fn to_embed(&self) -> usize {
        self.chunks.iter().filter(|c| c.text.is_some()).count()
    }
}

/// What one [`pending_chunks`] scan found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingChunkScan {
    /// Files with work to do, ordered by path, at most `limit` of them.
    pub files: Vec<PendingChunkFile>,
    /// Indexed files stepped over because their content could not be read.
    /// Counted over the rows this scan walked, matching
    /// [`super::PendingScan::unreadable`]'s contract exactly (#3016).
    pub unreadable: usize,
}

/// A chunk on its way back to storage.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkVector {
    pub chunk_sha256: String,
    pub name: String,
    pub kind: String,
    pub start_line: u32,
    pub end_line: u32,
    /// `None` re-stamps the existing row rather than replacing its vector —
    /// the caller passes `None` back for every [`PendingChunk`] that arrived
    /// with `text: None`.
    pub vector: Option<Vec<f32>>,
}

/// One ranked chunk, with everything needed to cite it.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredChunk {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub start_line: u32,
    pub end_line: u32,
    /// Cosine similarity against the query, in `[-1.0, 1.0]`.
    pub score: f32,
}

/// Turn one chunk into the text an embedder sees.
///
/// Path and name first, for the reason [`super::render_file_text`] leads with
/// the path: a model reading
/// `crates/stella-model/src/anthropic.rs :: apply_cache_control (function)`
/// has most of what it needs before a line of body. Deliberately plain — no
/// headings a tokenizer spends budget on.
///
/// Pure: no I/O, no clock, no environment. The same inputs render the same
/// bytes forever, which is what makes the hash of this output a valid identity
/// for the vector computed from it.
#[must_use]
pub fn render_chunk_text(rel_path: &str, name: &str, kind: &str, body: &str) -> String {
    let mut out = String::with_capacity(MAX_CHUNK_CHARS + 128);
    out.push_str(rel_path);
    out.push_str(" :: ");
    out.push_str(name);
    out.push_str(" (");
    out.push_str(kind);
    out.push_str(")\n\n");
    // `char_indices` rather than a byte slice: truncating a UTF-8 sequence
    // would make the render depend on where the cut landed.
    match body.char_indices().nth(MAX_CHUNK_CHARS) {
        Some((byte, _)) => out.push_str(&body[..byte]),
        None => out.push_str(body),
    }
    out
}

/// The 1-based, inclusive line range `start..=end` of `content`.
///
/// Total: a span running past the end of the file yields what there is, and an
/// inverted span yields the empty string. A stale span cannot be an error here
/// — the index and the file on disk are read at different instants by
/// construction.
#[must_use]
pub fn line_span(content: &str, start_line: u32, end_line: u32) -> String {
    if start_line == 0 || end_line < start_line {
        return String::new();
    }
    content
        .lines()
        .skip(start_line as usize - 1)
        .take((end_line - start_line) as usize + 1)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The one predicate for "this file still needs a chunk pass", shared by
/// [`pending_chunks`] and [`pending_chunk_file_count`] so the page of work and
/// the count of it can never disagree. `?1` is the fingerprint.
///
/// **It is a coverage marker, not a count comparison** (#3128). The obvious
/// spelling — raw symbol count against stored chunk-vector rows — is wrong in
/// one direction and permanently so: rows are keyed
/// `(file_id, chunk_sha256, fingerprint)`, and `chunk_sha256` hashes the
/// *rendered* chunk, so two different symbols whose rendered text is
/// byte-identical collapse to one row. Two trait accessors that both read
/// `&self.id`, or two `fn execute() { todo!() }` stubs across fixtures in one
/// test file, are enough. The symbol count then exceeds the row count forever,
/// and the file is re-selected on every pass no matter how correctly and how
/// often it has been embedded.
///
/// Asking instead whether the file carries **any** chunk vector stamped with
/// its current content hash is exact here, because a chunk pass is per-file
/// atomic: `embed_and_store_chunk_file` gathers every vector the file needs
/// before it writes any of them, and re-stamps `file_sha256` on the rows it
/// skipped. So a file is either untouched at this content hash or completely
/// covered at it — there is no partial state for the marker to misread. Edited
/// files still re-enter: their new `content_sha256` matches no stored row.
///
/// The `EXISTS` on symbols is load-bearing rather than tidiness. Without it a
/// file with no symbols at all — an empty source file, a markdown document with
/// no headings — matches "carries no chunk vector" trivially and would be
/// selected on every pass forever, which is the very bug this predicate exists
/// to remove, merely relocated.
const NEEDS_CHUNK_PASS: &str = "EXISTS (SELECT 1 FROM code_graph_symbols s WHERE s.file_id = f.id) \
     AND NOT EXISTS (SELECT 1 FROM code_graph_chunk_vectors v \
                     WHERE v.file_id = f.id AND v.fingerprint = ?1 \
                       AND v.file_sha256 = f.content_sha256)";

/// Up to `limit` files whose chunks are not fully embedded under
/// `fingerprint`, with each chunk rendered and hashed.
///
/// "Not fully embedded" is `NEEDS_CHUNK_PASS` — a coverage marker read
/// straight from the index, which is what keeps a scan over an up-to-date
/// workspace from re-reading every file on disk to discover there is nothing
/// to do.
///
/// Ordered by path and resumed through a path cursor, filling **past**
/// unreadable files rather than spending the window on them — the same
/// discipline [`super::pending`] uses, and for the same reason (#3016).
pub fn pending_chunks(
    conn: &Connection,
    root: &Path,
    fingerprint: &str,
    limit: usize,
) -> Result<PendingChunkScan, GraphError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT f.id, f.path, f.content_sha256 \
         FROM code_graph_files f \
         WHERE f.path > ?2 AND {NEEDS_CHUNK_PASS} \
         ORDER BY f.path \
         LIMIT ?3"
    ))?;

    let mut scan = PendingChunkScan::default();
    // The empty string sorts before every non-empty path under SQLite's
    // BINARY collation, so the first round starts at the beginning.
    let mut cursor = String::new();
    while scan.files.len() < limit {
        let want = limit - scan.files.len();
        let rows: Vec<(i64, String, String)> = stmt
            .query_map(params![fingerprint, cursor, want as i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<_, _>>()?;
        let Some((_, last, _)) = rows.last() else {
            break;
        };
        cursor = last.clone();

        for (file_id, path, file_sha256) in rows {
            let Ok(content) = std::fs::read_to_string(root.join(&path)) else {
                scan.unreadable += 1;
                continue;
            };
            let already = embedded_hashes(conn, file_id, fingerprint)?;
            let chunks: Vec<PendingChunk> = store::symbols_in_file(conn, &path)?
                .into_iter()
                .map(|def| {
                    let kind = def.kind.tag().to_string();
                    let body = line_span(&content, def.start_line, def.end_line);
                    let text = render_chunk_text(&path, &def.name, &kind, &body);
                    let chunk_sha256 = store::sha256_hex(text.as_bytes());
                    let needed = !already.contains(&chunk_sha256);
                    PendingChunk {
                        name: def.name,
                        kind,
                        start_line: def.start_line,
                        end_line: def.end_line,
                        chunk_sha256,
                        text: needed.then_some(text),
                    }
                })
                .collect();
            if chunks.is_empty() {
                continue;
            }
            scan.files.push(PendingChunkFile {
                path,
                file_sha256,
                chunks,
            });
        }
    }
    Ok(scan)
}

/// Every chunk hash already stored for `file_id` under `fingerprint`.
fn embedded_hashes(
    conn: &Connection,
    file_id: i64,
    fingerprint: &str,
) -> Result<HashSet<String>, GraphError> {
    let mut stmt = conn.prepare_cached(
        "SELECT chunk_sha256 FROM code_graph_chunk_vectors \
         WHERE file_id = ?1 AND fingerprint = ?2",
    )?;
    let rows = stmt.query_map(params![file_id, fingerprint], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Write one file's chunks under `fingerprint`, then sweep what no longer
/// exists. Returns how many rows were written or re-stamped.
///
/// **One file per call, one transaction, and the sweep is why.** After the
/// upserts, every row for this `(file, fingerprint)` still carrying an older
/// `file_sha256` is a chunk the file no longer has — a deleted function, a
/// renamed heading — and is removed. Splitting a file across two calls would
/// let the first half's rows be swept by the second, which is exactly why
/// [`pending_chunks`] caps on files rather than on chunks.
///
/// A path the index has since dropped is skipped rather than resurrected,
/// matching [`super::store_vectors`].
pub fn store_chunk_vectors(
    conn: &mut Connection,
    fingerprint: &str,
    path: &str,
    file_sha256: &str,
    chunks: &[ChunkVector],
) -> Result<usize, GraphError> {
    let tx = conn.transaction()?;
    let written = write_chunks(&tx, fingerprint, path, file_sha256, chunks)?;
    tx.commit()?;
    Ok(written)
}

fn write_chunks(
    tx: &Transaction<'_>,
    fingerprint: &str,
    path: &str,
    file_sha256: &str,
    chunks: &[ChunkVector],
) -> Result<usize, GraphError> {
    let file_id: Option<i64> = tx
        .prepare_cached("SELECT id FROM code_graph_files WHERE path = ?1")?
        .query_row(params![path], |row| row.get(0))
        .ok();
    let Some(file_id) = file_id else {
        return Ok(0);
    };

    let now = store::now_unix();
    let mut written = 0usize;
    {
        let mut insert = tx.prepare_cached(
            "INSERT INTO code_graph_chunk_vectors\
               (file_id, chunk_sha256, fingerprint, file_sha256, name, kind, \
                start_line, end_line, dims, vector, embedded_at) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT(file_id, chunk_sha256, fingerprint) DO UPDATE SET \
               file_sha256 = excluded.file_sha256, \
               name        = excluded.name, \
               kind        = excluded.kind, \
               start_line  = excluded.start_line, \
               end_line    = excluded.end_line",
        )?;
        // Re-stamping a surviving chunk: the row keeps its vector and takes
        // the new file hash and line span. The span is the half that moves —
        // an edit above a function shifts it without changing a byte of it.
        let mut restamp = tx.prepare_cached(
            "UPDATE code_graph_chunk_vectors \
             SET file_sha256 = ?4, name = ?5, kind = ?6, start_line = ?7, end_line = ?8 \
             WHERE file_id = ?1 AND chunk_sha256 = ?2 AND fingerprint = ?3",
        )?;

        for chunk in chunks {
            let touched = match &chunk.vector {
                Some(vector) => insert.execute(params![
                    file_id,
                    chunk.chunk_sha256,
                    fingerprint,
                    file_sha256,
                    chunk.name,
                    chunk.kind,
                    chunk.start_line,
                    chunk.end_line,
                    vector.len() as i64,
                    super::encode(vector),
                    now,
                ])?,
                None => restamp.execute(params![
                    file_id,
                    chunk.chunk_sha256,
                    fingerprint,
                    file_sha256,
                    chunk.name,
                    chunk.kind,
                    chunk.start_line,
                    chunk.end_line,
                ])?,
            };
            written += touched;
        }
    }

    tx.execute(
        "DELETE FROM code_graph_chunk_vectors \
         WHERE file_id = ?1 AND fingerprint = ?2 AND file_sha256 <> ?3",
        params![file_id, fingerprint, file_sha256],
    )?;
    Ok(written)
}

/// Rank every stored chunk under `fingerprint` against `query`.
///
/// The ranking itself is [`stella_embed::rank::top_k`] — pure, total and
/// property-tested there — so this function only gets the vectors out of
/// SQLite and hands them over, the invariant-2 split [`super::rank`] already
/// makes.
///
/// Candidates are keyed `path#line#name` rather than by rowid: the key is
/// `top_k`'s tie-break, so it has to be both unique per chunk and *stable
/// across rebuilds of the database*, which a rowid is not.
pub fn rank_chunks(
    conn: &Connection,
    fingerprint: &str,
    query: &[f32],
    floor: f32,
    limit: usize,
) -> Result<Vec<ScoredChunk>, GraphError> {
    let mut stmt = conn.prepare(
        "SELECT f.path, v.name, v.kind, v.start_line, v.end_line, v.vector \
         FROM code_graph_chunk_vectors v \
         JOIN code_graph_files f ON f.id = v.file_id \
         WHERE v.fingerprint = ?1 \
         ORDER BY f.path, v.start_line, v.name",
    )?;
    let rows: Vec<(ScoredChunk, Vec<f32>)> = stmt
        .query_map(params![fingerprint], |row| {
            let blob: Vec<u8> = row.get(5)?;
            Ok((
                ScoredChunk {
                    path: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    start_line: row.get(3)?,
                    end_line: row.get(4)?,
                    score: 0.0,
                },
                super::decode(&blob),
            ))
        })?
        .collect::<Result<_, _>>()?;

    let keys: Vec<String> = rows.iter().map(|(chunk, _)| candidate_key(chunk)).collect();
    let candidates: Vec<Candidate<'_>> = keys
        .iter()
        .zip(&rows)
        .map(|(key, (_, vector))| Candidate {
            key: key.as_str(),
            vector: vector.as_slice(),
        })
        .collect();

    let scored: Vec<Scored> = stella_embed::rank::top_k(query, &candidates, floor, limit);
    let by_key: std::collections::HashMap<&str, &ScoredChunk> = keys
        .iter()
        .zip(&rows)
        .map(|(key, (chunk, _))| (key.as_str(), chunk))
        .collect();

    Ok(scored
        .into_iter()
        .filter_map(|hit| {
            by_key.get(hit.key.as_str()).map(|chunk| ScoredChunk {
                score: hit.score,
                ..(*chunk).clone()
            })
        })
        .collect())
}

/// The rank key for one chunk: unique within a database, and stable across
/// rebuilds of it. Zero-padded so the ordering the key imposes on a tie is the
/// document order a reader expects rather than a lexical one.
fn candidate_key(chunk: &ScoredChunk) -> String {
    format!("{}#{:08}#{}", chunk.path, chunk.start_line, chunk.name)
}

/// How many chunks carry a vector under `fingerprint`.
pub fn chunk_count(conn: &Connection, fingerprint: &str) -> Result<usize, GraphError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM code_graph_chunk_vectors WHERE fingerprint = ?1",
        params![fingerprint],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as usize)
}

/// How many indexed files still need a chunk pass under `fingerprint` — the
/// same `NEEDS_CHUNK_PASS` predicate [`pending_chunks`] scans, as a count
/// rather than a page of rendered work. An eager pass reports this as
/// `remaining` without paying for a render-and-hash pass over files it is
/// about to discard the content of.
///
/// Because the predicate is exact, **zero here means done**: a caller may treat
/// it as the completeness oracle it reads like, which
/// `stella_tools::search::coverage_note` does (#3124).
pub fn pending_chunk_file_count(conn: &Connection, fingerprint: &str) -> Result<usize, GraphError> {
    let count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM code_graph_files f WHERE {NEEDS_CHUNK_PASS}"),
        params![fingerprint],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as usize)
}
