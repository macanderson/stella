//! Bounded candidate generation — the readers `recall` ranks over.
//!
//! Recall used to load every live node *with its body* and every vector, rank
//! the whole corpus, and only then apply a budget that keeps a handful. The
//! bodies were the expensive part: a workspace accumulates memories forever, so
//! the bytes crossing the SQLite boundary per turn grew with the workspace's
//! lifetime while the answer stayed five frames long.
//!
//! These readers separate **ranking** from **serving**. Ranking runs over
//! [`NodeMeta`] — an id, a label, a hash, and a byte count, with no content —
//! and is `LIMIT`-bounded at the SQL boundary. Serving reads full rows for the
//! survivors only, through [`nodes_by_ids`]. What a recall costs is therefore
//! set by the frame count it asks for, not by how long the workspace has been
//! alive (#712 deliverable 2).
//!
//! Every reader here shares one bitemporal predicate, [`NODE_AS_OF`], so a
//! point-in-time query is answered from a single instant across every signal
//! (#712 deliverable 7).

use rusqlite::{Connection, params};

use crate::error::ContextError;

use super::node::{NodeRow, map_node_row};

/// The bitemporal liveness predicate every candidate reader shares. Bound
/// parameter `?1` is the `as_of` cutoff: `NULL` reads currently-believed rows,
/// a timestamp reads the rows believed at that instant.
///
/// Written branch-free, as one string with one repeated parameter, on purpose.
/// The defect this replaces was a cutoff that reached `neighbors` and nothing
/// else, so a point-in-time recall returned today's content wearing yesterday's
/// edges. Two SQL variants per reader is how that happens: each reader is one
/// place the cutoff can be forgotten. One predicate, textually shared, cannot
/// be honored by four readers out of five.
///
/// Expects the `node` table aliased as `n`.
pub(crate) const NODE_AS_OF: &str = "(?1 IS NULL OR n.recorded_at <= ?1) \
     AND (n.superseded_at IS NULL OR (?1 IS NOT NULL AND n.superseded_at > ?1))";

/// Everything the ranking passes read about a node, and nothing they don't.
///
/// No `content`. Fusion ranks ids, dedup compares hashes, MMR folds vectors,
/// and the budget packs byte counts — none of them needs a body, and reading
/// one per corpus row was the plane's real per-turn cost. The body is fetched
/// for packed survivors only.
#[derive(Debug, Clone)]
pub(crate) struct NodeMeta {
    /// The SQLite rowid, which is what every ranked list carries.
    pub id: i64,
    /// The stable `nod_…` identity, so a drop report names frames a caller can
    /// ask for again without the store minting one.
    pub public_id: String,
    /// The human citation label (`L-C4`), for the same reason.
    pub display_name: String,
    /// sha256 of the content — the dedup key.
    pub content_hash: String,
    /// Whether the content is empty or ASCII-whitespace-only. Computed in SQL
    /// so dedup can keep blank nodes distinct (they all share `sha256("")`
    /// while being distinct identities) without reading their bodies.
    ///
    /// SQLite has no Unicode-aware `trim`, so content consisting solely of
    /// non-ASCII space characters reads as non-blank here where Rust's
    /// `str::trim` would call it blank. That only changes which dedup bucket
    /// such a node lands in, and no writer in the tree produces one.
    pub blank: bool,
    /// What this node would cost a budget, computed as the protocol's
    /// canonical `ceil(utf8_bytes / 4)` without moving the bytes. The packer
    /// spends against this, and [`crate::retrieval::frame_from_node`] declares
    /// the same number over the same content, so the two cannot disagree.
    pub token_cost: u32,
}

/// The metadata projection, shared by every reader below so a column added to
/// one cannot be missing from another. `n.content` never appears in it.
const META_COLUMNS: &str = "n.id, n.public_id, n.display_name, n.content_hash, \
     (n.content = '' OR trim(n.content, char(32)||char(9)||char(10)||char(13)||char(11)||char(12)) = '') AS blank, \
     (length(CAST(n.content AS BLOB)) + 3) / 4 AS token_cost";

fn map_meta(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeMeta> {
    Ok(NodeMeta {
        id: row.get("id")?,
        public_id: row.get("public_id")?,
        display_name: row.get("display_name")?,
        content_hash: row.get("content_hash")?,
        blank: row.get::<_, i64>("blank")? != 0,
        token_cost: row.get::<_, i64>("token_cost")?.max(0) as u32,
    })
}

fn collect_meta(
    stmt: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> Result<Vec<NodeMeta>, ContextError> {
    let mut out = Vec::new();
    for row in stmt.query_map(params, map_meta)? {
        out.push(row?);
    }
    Ok(out)
}

/// The `limit` most recently recorded nodes, newest first.
///
/// The recency signal is what used to make candidate generation unbounded: it
/// contributes *every* live node to the fusion at any relevance. Bounding it
/// here is safe because recency enters the fusion damped
/// (`retrieval::RECENCY_WEIGHT`) — at rank 200 a node banks `0.15/261`, sixty
/// times less than a single mid-ranked semantic hit, so a node that recency
/// alone would have surfaced from below the limit could not have won a slot.
/// A node with real relevance is still reachable: it is in the vector list, the
/// graph expansion, or the domain-overlap list, none of which is recency-bounded.
pub(crate) fn recent_node_meta(
    conn: &Connection,
    as_of: Option<&str>,
    limit: usize,
) -> Result<Vec<NodeMeta>, ContextError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {META_COLUMNS} FROM node n
         WHERE {NODE_AS_OF}
         ORDER BY n.recorded_at DESC, n.id DESC
         LIMIT ?2"
    ))?;
    collect_meta(&mut stmt, params![as_of, limit as i64])
}

/// Metadata for a specific set of node ids — how the ids a *vector* or *graph*
/// signal produced acquire the fields fusion and packing need.
///
/// The id list is already bounded by its producer, so this reads at most that
/// many rows. Ids that are not live at `as_of` simply do not come back, which
/// is what makes the cutoff reach the vector and graph signals too.
pub(crate) fn node_meta_for_ids(
    conn: &Connection,
    ids: &[i64],
    as_of: Option<&str>,
) -> Result<Vec<NodeMeta>, ContextError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // Numbered explicitly: `?1` is `as_of`, the ids bind from `?2` on. See
    // `domain_ranked_ids` for why a bare `?` alongside a numbered one is a trap.
    let placeholders = (0..ids.len())
        .map(|i| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT {META_COLUMNS} FROM node n
         WHERE {NODE_AS_OF} AND n.id IN ({placeholders})"
    ))?;
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(ids.len() + 1);
    binds.push(Box::new(as_of.map(str::to_string)));
    for id in ids {
        binds.push(Box::new(*id));
    }
    collect_meta(&mut stmt, rusqlite::params_from_iter(binds.iter()))
}

/// Term-matching candidates for the lexical fallback (`L-C6`), scored by the
/// fraction of query terms present in the label or body, best first.
///
/// The matching runs *in SQLite* rather than over rows pulled into Rust. The
/// fallback was already capped at a handful of frames, but it reached that cap
/// by reading every node's body and scanning it here — the exact cost the
/// bounded readers exist to remove, on the path taken when the vector index is
/// cold, which is when a workspace is largest.
///
/// `terms` come from user query text. They are bound as parameters, never
/// formatted into the SQL, and `retrieval::query_terms` yields alphanumeric
/// runs only — so no `LIKE` metacharacter can appear and no `ESCAPE` clause is
/// needed. The term count is capped by the caller.
pub(crate) fn lexical_node_meta(
    conn: &Connection,
    terms: &[String],
    as_of: Option<&str>,
    limit: usize,
) -> Result<Vec<(NodeMeta, f32)>, ContextError> {
    if terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    // One `LIKE` per term over `display_name || ' ' || content`, summed into a
    // hit count. Identical arithmetic to the Rust scan it replaces.
    let hits = (0..terms.len())
        .map(|i| {
            format!(
                "(CASE WHEN lower(n.display_name || ' ' || n.content) LIKE ?{} THEN 1 ELSE 0 END)",
                i + 3
            )
        })
        .collect::<Vec<_>>()
        .join(" + ");
    // Ties break on node id: term-fraction scores collide heavily (there are
    // only `terms.len() + 1` possible values), so without the tiebreak the
    // LIMIT would keep a *different set* of frames from run to run — not merely
    // a different order, which is the one thing the fallback must not do.
    let mut stmt = conn.prepare(&format!(
        "SELECT {META_COLUMNS}, ({hits}) AS hits FROM node n
         WHERE {NODE_AS_OF} AND ({hits}) > 0
         ORDER BY hits DESC, n.id ASC
         LIMIT ?2"
    ))?;
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(terms.len() + 2);
    binds.push(Box::new(as_of.map(str::to_string)));
    binds.push(Box::new(limit as i64));
    for term in terms {
        binds.push(Box::new(format!("%{}%", term.to_lowercase())));
    }
    let total = terms.len() as f32;
    let mut out = Vec::new();
    for row in stmt.query_map(rusqlite::params_from_iter(binds.iter()), |r| {
        Ok((map_meta(r)?, r.get::<_, i64>("hits")? as f32 / total))
    })? {
        out.push(row?);
    }
    Ok(out)
}

/// Node ids ranked by how many of `scope`'s domains they carry, best first,
/// bounded by `limit`.
///
/// Replaces a Rust-side pass over every live node's domain tags. Ties break on
/// node id for the same reason they always do here: overlap counts collide
/// constantly (most tagged nodes carry exactly one domain), and the drained
/// order is what RRF converts into a rank.
pub(crate) fn domain_ranked_ids(
    conn: &Connection,
    scope: &[String],
    as_of: Option<&str>,
    limit: usize,
) -> Result<Vec<i64>, ContextError> {
    if scope.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    // Numbered explicitly, never bare `?`. SQLite assigns a bare marker "one
    // greater than the largest index assigned so far", so mixing the two forms
    // makes a bare marker silently collide with `?2` — which is `LIMIT` here.
    // That is a wrong-arity error at prepare time if you are lucky and a
    // wrong-value bind if you are not.
    let placeholders = (0..scope.len())
        .map(|i| format!("?{}", i + 3))
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT n.id, COUNT(*) AS overlap
         FROM node_domains nd
         JOIN domain d ON d.id = nd.domain_id
         JOIN node n ON n.id = nd.node_id
         WHERE {NODE_AS_OF} AND d.name IN ({placeholders})
         GROUP BY n.id
         ORDER BY overlap DESC, n.id ASC
         LIMIT ?2"
    ))?;
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(scope.len() + 2);
    binds.push(Box::new(as_of.map(str::to_string)));
    binds.push(Box::new(limit as i64));
    for name in scope {
        binds.push(Box::new(name.clone()));
    }
    let mut out = Vec::new();
    for row in stmt.query_map(rusqlite::params_from_iter(binds.iter()), |r| {
        r.get::<_, i64>(0)
    })? {
        out.push(row?);
    }
    Ok(out)
}

/// Full rows — bodies included — for exactly the nodes that survived packing.
///
/// This is the only read on the recall path that moves content, and the only
/// one whose cost is set by the query's `max_frames` rather than by the size of
/// the store.
///
/// Counts the rows and bytes it moves so a benchmark can pin what a recall
/// *costs* rather than how long it happens to take on the machine running the
/// suite. Wall clock cannot distinguish "bounded" from "fast today"; this can.
/// Test-only: no counter exists in a release build.
#[cfg(test)]
pub(crate) static CONTENT_ROWS_READ: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
/// Bytes of node content crossing the SQLite boundary. See [`CONTENT_ROWS_READ`].
#[cfg(test)]
pub(crate) static CONTENT_BYTES_READ: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub(crate) fn nodes_by_ids(conn: &Connection, ids: &[i64]) -> Result<Vec<NodeRow>, ContextError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT id, public_id, kind, display_name, content, content_hash, uri, valid_from, recorded_at
         FROM node WHERE id IN ({placeholders})"
    ))?;
    let mut out = Vec::new();
    for row in stmt.query_map(rusqlite::params_from_iter(ids.iter()), map_node_row)? {
        let row = row?;
        #[cfg(test)]
        {
            use std::sync::atomic::Ordering;
            CONTENT_ROWS_READ.fetch_add(1, Ordering::Relaxed);
            CONTENT_BYTES_READ.fetch_add(row.content.len(), Ordering::Relaxed);
        }
        out.push(row);
    }
    Ok(out)
}

/// Domain tags for a bounded set of nodes, sorted per node for stable citation
/// display.
///
/// The unbounded form scanned every live node's tags on every recall — one
/// grouped scan, but a scan whose size grew with the store. Only the packed
/// survivors' tags are ever shown, so only theirs are read.
pub(crate) fn domains_for_nodes(
    conn: &Connection,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, Vec<String>>, ContextError> {
    let mut out: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT nd.node_id, d.name FROM node_domains nd
         JOIN domain d ON d.id = nd.domain_id
         WHERE nd.node_id IN ({placeholders})
         ORDER BY nd.node_id, d.name"
    ))?;
    for row in stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })? {
        let (id, name) = row?;
        out.entry(id).or_default().push(name);
    }
    Ok(out)
}
