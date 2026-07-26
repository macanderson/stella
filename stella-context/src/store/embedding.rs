//! Embedding access — the vector codec, the fingerprinted index reads, and
//! the byte-compat skip that keeps re-indexing cheap (`L-C2`).
//!
//! Split out of the store module unchanged (#712 deliverable 1).

use rusqlite::{Connection, params};

use crate::error::ContextError;

/// Encode a vector as a little-endian f32 BLOB.
pub(crate) fn vector_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Decode a little-endian f32 BLOB back to a vector. A length that isn't a
/// multiple of 4 is corruption, reported loudly rather than truncated.
pub(crate) fn blob_to_vector(blob: &[u8]) -> Result<Vec<f32>, ContextError> {
    if !blob.len().is_multiple_of(4) {
        return Err(ContextError::Corruption(format!(
            "embedding blob length {} is not a multiple of 4",
            blob.len()
        )));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

/// (node_id, vector) for every node with an embedding under `fingerprint` that
/// is live at `as_of`. The join on `content_hash` is what enforces "never mix
/// fingerprints" structurally — a vector under any other fingerprint is simply
/// not selected.
///
/// The cutoff is applied to the **node**, never to the embedding's own
/// `recorded_at`. A vector is a derived index over content, not a record with
/// its own history (ADR 0010, decision point 6): asking what was believed at T
/// must not depend on when the index happened to catch up, and a warm that ran
/// this morning must not make yesterday's content invisible to a query about
/// yesterday.
pub(crate) fn vectors_for_fingerprint(
    conn: &Connection,
    fingerprint: &str,
    as_of: Option<&str>,
) -> Result<Vec<(i64, Vec<f32>)>, ContextError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT n.id, e.vector
         FROM embedding e
         JOIN node n ON n.content_hash = e.content_hash
         WHERE e.fingerprint = ?2 AND {}",
        super::candidate::NODE_AS_OF
    ))?;
    let rows = stmt.query_map(params![as_of, fingerprint], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (id, blob) = r?;
        out.push((id, blob_to_vector(&blob)?));
    }
    Ok(out)
}

/// Whether a vector already exists for `(content_hash, fingerprint)` — the
/// byte-compat skip that makes re-indexing cheap (`L-C2`).
pub(crate) fn embedding_exists(
    conn: &Connection,
    content_hash: &str,
    fingerprint: &str,
) -> Result<bool, ContextError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM embedding WHERE content_hash = ?1 AND fingerprint = ?2",
        params![content_hash, fingerprint],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Insert a vector (idempotent under the composite primary key). Returns
/// whether a new row was written (`false` = it already existed = reused).
pub(crate) fn store_embedding(
    conn: &Connection,
    content_hash: &str,
    fingerprint: &str,
    vector: &[f32],
    now: &str,
) -> Result<bool, ContextError> {
    let changed = conn.execute(
        "INSERT INTO embedding (content_hash, fingerprint, dims, vector, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(content_hash, fingerprint) DO NOTHING",
        params![
            content_hash,
            fingerprint,
            vector.len() as i64,
            vector_to_blob(vector),
            now
        ],
    )?;
    Ok(changed > 0)
}

/// Live nodes lacking a vector under `fingerprint`, as `(content_hash, content)`.
/// Deduplicated by content hash so identical content is embedded once.
pub(crate) fn nodes_missing_embedding(
    conn: &Connection,
    fingerprint: &str,
) -> Result<Vec<(String, String)>, ContextError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT n.content_hash, n.content
         FROM node n
         WHERE n.superseded_at IS NULL
           AND n.content <> ''
           AND NOT EXISTS (
               SELECT 1 FROM embedding e
               WHERE e.content_hash = n.content_hash AND e.fingerprint = ?1
           )",
    )?;
    let rows = stmt.query_map(params![fingerprint], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}
