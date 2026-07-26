//! Edge access — fact assertion, supersession, and the two point-in-time
//! readers (`neighbors` for adjacency, `edges_as_of` for the audit query).
//!
//! Split out of the store module unchanged (#712 deliverable 1).

use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{Connection, params};

use crate::error::ContextError;

use super::sha256_hex;

/// 1-hop neighbors of `seeds` over currently-believed fact edges, as
/// `(neighbor_id, edge_weight)`. `as_of` (transaction time) pins which beliefs
/// are visible: `None` = currently believed (`superseded_at IS NULL`).
pub(crate) fn neighbors(
    conn: &Connection,
    seeds: &[i64],
    as_of: Option<&str>,
) -> Result<Vec<(i64, f64)>, ContextError> {
    let mut out = Vec::new();
    // Undirected 1-hop: a seed on either endpoint pulls in the other.
    let sql = match as_of {
        None => {
            "SELECT CASE WHEN src_id = ?1 THEN dst_id ELSE src_id END AS other, weight
             FROM edge
             WHERE (src_id = ?1 OR dst_id = ?1) AND superseded_at IS NULL"
        }
        Some(_) => {
            "SELECT CASE WHEN src_id = ?1 THEN dst_id ELSE src_id END AS other, weight
             FROM edge
             WHERE (src_id = ?1 OR dst_id = ?1)
               AND recorded_at <= ?2
               AND (superseded_at IS NULL OR superseded_at > ?2)"
        }
    };
    let map = |r: &rusqlite::Row<'_>| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?));
    let mut stmt = conn.prepare(sql)?;
    for &seed in seeds {
        // Each arm's `MappedRows` is a distinct closure type, so consume it
        // fully inside the arm rather than binding it across the `match`.
        match as_of {
            None => {
                for r in stmt.query_map(params![seed], map)? {
                    out.push(r?);
                }
            }
            Some(t) => {
                for r in stmt.query_map(params![seed, t], map)? {
                    out.push(r?);
                }
            }
        }
    }
    Ok(out)
}

/// A fact edge read back for point-in-time queries. Endpoints are node rowids
/// the caller resolves to human labels (`L-C4`).
#[derive(Debug, Clone)]
pub(crate) struct EdgeView {
    pub rel: String,
    pub src_id: i64,
    pub dst_id: i64,
    pub recorded_at: String,
    pub superseded_at: Option<String>,
}

/// Sequence disambiguating two facts asserted within the same clock second,
/// which would otherwise hash to the same edge `public_id`.
///
/// It is **process-local**, so two stella processes writing the same workspace
/// db concurrently can still mint the same `edg_…` id; `edge.public_id` carries
/// no UNIQUE constraint, so those rows coexist silently. Nothing reads an edge
/// by public id today, which is why this is tolerable rather than a bug — a
/// reader would need the id made collision-proof first (see the audit note on
/// `insert_edge`).
static EDGE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Insert a fact edge. `supersedes` links to the edge this one replaced (the
/// `SUPERSEDES` relation of), or `None` for a
/// fresh assertion. Returns the new edge's rowid.
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_edge(
    conn: &Connection,
    rel: &str,
    src_id: i64,
    dst_id: i64,
    weight: f64,
    properties: &serde_json::Value,
    valid_from: Option<&str>,
    valid_to: Option<&str>,
    now: &str,
    supersedes: Option<i64>,
) -> Result<i64, ContextError> {
    let seq = EDGE_SEQ.fetch_add(1, Ordering::Relaxed);
    let public_id = format!(
        "edg_{}",
        &sha256_hex(&format!("{rel}:{src_id}:{dst_id}:{now}:{seq}"))[..24]
    );
    let props = serde_json::to_string(properties)?;
    let id: i64 = conn.query_row(
        "INSERT INTO edge (public_id, rel, src_id, dst_id, weight, properties, valid_from, valid_to, recorded_at, supersedes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         RETURNING id",
        params![public_id, rel, src_id, dst_id, weight, props, valid_from, valid_to, now, supersedes],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// Close an edge's intervals: set `superseded_at` (transaction time) and, if
/// not already ended, `valid_to` (world time). **Never deletes** (`L-C3`) — the
/// row survives so "what did we believe at T1" still answers.
pub(crate) fn close_edge(
    conn: &Connection,
    edge_id: i64,
    superseded_at: &str,
    valid_to: &str,
) -> Result<(), ContextError> {
    conn.execute(
        "UPDATE edge SET superseded_at = ?2, valid_to = COALESCE(valid_to, ?3) WHERE id = ?1",
        params![edge_id, superseded_at, valid_to],
    )?;
    Ok(())
}

/// Fact edges as believed at a transaction-time instant. `as_of = None` means
/// "currently believed" (`superseded_at IS NULL`); `Some(t)` reconstructs the
/// belief set at `t` — the bi-temporal audit query (`L-C3`).
pub(crate) fn edges_as_of(
    conn: &Connection,
    as_of: Option<&str>,
) -> Result<Vec<EdgeView>, ContextError> {
    let map = |r: &rusqlite::Row<'_>| {
        Ok(EdgeView {
            rel: r.get(0)?,
            src_id: r.get(1)?,
            dst_id: r.get(2)?,
            recorded_at: r.get(3)?,
            superseded_at: r.get(4)?,
        })
    };
    let mut out = Vec::new();
    match as_of {
        None => {
            let mut stmt = conn.prepare(
                "SELECT rel, src_id, dst_id, recorded_at, superseded_at
                 FROM edge WHERE superseded_at IS NULL",
            )?;
            for r in stmt.query_map([], map)? {
                out.push(r?);
            }
        }
        Some(t) => {
            let mut stmt = conn.prepare(
                "SELECT rel, src_id, dst_id, recorded_at, superseded_at
                 FROM edge
                 WHERE recorded_at <= ?1 AND (superseded_at IS NULL OR superseded_at > ?1)",
            )?;
            for r in stmt.query_map(params![t], map)? {
                out.push(r?);
            }
        }
    }
    Ok(out)
}
