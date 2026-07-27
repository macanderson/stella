//! The queries recall runs on every turn.
//!
//! Kept apart from [`crate::store`] because they answer to a different
//! constraint. The store's other loaders serve one caller each and read whole
//! rows; these run on the first-token path of every turn against a corpus that
//! only grows, so each one exists to keep some part of that cost proportional to
//! the QUERY rather than to lifetime memory size:
//!
//! - [`live_node_metas`] ranks the corpus without moving a single content body.
//! - [`score_nodes_by_vector`] scores embeddings off their stored BLOBs without
//!   decoding one.
//! - [`nodes_by_ids`], [`vectors_for_ids`], and [`domains_for_nodes`] fetch the
//!   bodies, vectors, and tags of the candidates that survived the cut — and
//!   nothing else.
//! - [`scan_lexical`] streams the weak-coverage fallback's scan instead of
//!   materializing the corpus to search it.
//!
//! The pipeline that composes them is [`crate::retrieval`].

use rusqlite::{Connection, params};

use crate::error::ContextError;
use crate::retrieval::RecallTier;
use crate::store::{NodeRow, blob_to_vector, map_node_row};

/// The bitemporal liveness predicate every reader here shares. Bound parameter
/// `?1` is the `as_of` cutoff: `NULL` reads currently-believed rows, a timestamp
/// reads the rows believed at that instant.
///
/// Written branch-free, as one string with one repeated parameter, on purpose.
/// The defect this replaces was a cutoff that reached `neighbors` and nothing
/// else, so a point-in-time recall returned today's node content wearing
/// yesterday's edges. Two SQL variants per reader is how that happens: each
/// reader becomes one more place the cutoff can be forgotten. One predicate,
/// shared textually, cannot be honored by four readers out of five
/// (#712 deliverable 7).
///
/// Expects the `node` table aliased as `n`. Applied to the **node**, never to an
/// embedding's own `recorded_at`: a vector is a derived index over content, not
/// a record with its own history (ADR 0010, decision point 6), so when the warm
/// task happened to catch up must not decide what was believed.
pub(crate) const NODE_AS_OF: &str = "(?1 IS NULL OR n.recorded_at <= ?1) \
     AND (n.superseded_at IS NULL OR (?1 IS NOT NULL AND n.superseded_at > ?1))";

/// A live node with everything ranking needs and **no content body**.
///
/// Every signal recall fuses over the whole corpus — recency, domain overlap,
/// hash dedup, and the `L-C5` drop report — reads only identity, time, hash,
/// and *size*. Only the ≤`MMR_CANDIDATE_MULTIPLE × max_frames` candidates that
/// survive the cut need the bytes, and those are fetched by id
/// ([`nodes_by_ids`]). Loading `content` for the whole corpus meant a 5-frame
/// recall pulled every body a workspace had ever written across the SQLite
/// boundary and heap-allocated it as an owned `String`, on every turn.
///
/// The two derived columns exist so the body never has to be inspected:
/// `content_bytes` reproduces `budget_tokens` exactly (it is
/// `ceil(bytes / BYTES_PER_BUDGET_TOKEN)` over the same UTF-8 byte length), and
/// `content_blank` reproduces the `content.trim().is_empty()` test the dedup key
/// asks. See [`live_node_metas`] for the one documented edge of `content_blank`.
#[derive(Debug, Clone)]
pub struct NodeMeta {
    /// Internal rowid — the key every ranking signal and lookup map uses.
    pub id: i64,
    /// The stable `nod_…` identity a frame (or a drop report) carries.
    pub public_id: String,
    /// The human citation label (`L-C4`).
    pub display_name: String,
    /// sha256 of `content` — the hash-dedup key, and the digest a frame
    /// declares.
    pub content_hash: String,
    /// UTF-8 **byte** length of `content`, computed in SQL. The basis of the
    /// node's declared `token_cost` without moving the body.
    pub content_bytes: usize,
    /// Whether `content` is empty or whitespace-only — the dedup key's
    /// `Distinct` vs `Content` decision.
    pub content_blank: bool,
    /// Transaction time the row was first written; recall's recency sort key.
    pub recorded_at: String,
    /// Which precedence band this node competes in once the budget binds. Read
    /// here rather than joined later because it is consumed by
    /// [`crate::retrieval::pack_to_budget`], which runs over exactly this
    /// struct and never touches the body.
    pub recall_tier: RecallTier,
}

/// Record body bytes crossing the SQLite boundary (test builds only). Counted
/// here rather than in `store::map_node_row` so the tally covers exactly the
/// loaders recall can reach — `nodes_by_ids` and `scan_lexical` are the only two
/// that move a body — and not the store's unrelated single-row readers.
#[inline]
fn count_content_bytes(len: usize) {
    #[cfg(test)]
    crate::cost_counters::add_content_bytes(len);
    #[cfg(not(test))]
    let _ = len;
}

/// Every live node's ranking metadata, **without its content body**. Recall's
/// whole-corpus pass ([`crate::retrieval`] steps 3–4) runs on this; only the
/// candidates that survive the cut are re-read with [`nodes_by_ids`].
///
/// Both derived columns are computed in SQL so no body crosses the boundary:
///
/// - `length(CAST(content AS BLOB))` is the UTF-8 **byte** count. The bare
///   `length()` of a TEXT value counts *characters*, which would under-report a
///   non-ASCII body and hand the drop report a `token_cost` that disagrees with
///   `budget_tokens`; the `CAST` is what makes the two identical.
/// - `trim(content, <ascii ws>) = ''` stands in for Rust's
///   `content.trim().is_empty()`. **One documented edge:** SQLite's `trim` takes
///   a literal character set, so content consisting *only* of non-ASCII
///   whitespace (NBSP, U+2028, …) now reads as non-blank and therefore dedups by
///   content hash instead of keeping its own identity. Two such nodes with
///   byte-identical content collapse to one candidate. Empty content — the case
///   the dedup exemption actually exists for, since code-graph and taxonomy
///   nodes carry no text — is unaffected.
pub(crate) fn live_node_metas(
    conn: &Connection,
    as_of: Option<&str>,
) -> Result<Vec<NodeMeta>, ContextError> {
    // char(32,9,10,13,11,12) = space, tab, LF, CR, VT, FF — Rust's
    // `char::is_whitespace` over the ASCII range.
    let mut stmt = conn.prepare(&format!(
        "SELECT id, public_id, display_name, content_hash, recorded_at, recall_tier,
                length(CAST(content AS BLOB)) AS content_bytes,
                trim(content, char(32) || char(9) || char(10) || char(13) || char(11) || char(12)) = ''
                    AS content_blank
         FROM node n WHERE {NODE_AS_OF}"
    ))?;
    let rows = stmt.query_map(params![as_of], |row| {
        Ok(NodeMeta {
            id: row.get("id")?,
            public_id: row.get("public_id")?,
            display_name: row.get("display_name")?,
            content_hash: row.get("content_hash")?,
            content_bytes: row.get::<_, i64>("content_bytes")?.max(0) as usize,
            content_blank: row.get::<_, i64>("content_blank")? != 0,
            recorded_at: row.get("recorded_at")?,
            recall_tier: RecallTier::from_i64(row.get("recall_tier")?),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Stream every live node's label and body past `score`, keeping only the ids
/// it scores — the lexical fallback's scan (`L-C6`) with no corpus-sized
/// retention.
///
/// `score` sees each body **borrowed out of the row buffer**, one row at a time,
/// so the fallback matches on exactly the same bytes it always did while the
/// peak live heap stays one row instead of every body in the workspace. It ran
/// over a fully-materialized `Vec<NodeRow>` before, which is what made a
/// weak-coverage turn the most expensive kind.
pub(crate) fn scan_lexical(
    conn: &Connection,
    excluded: &std::collections::HashSet<i64>,
    as_of: Option<&str>,
    mut score: impl FnMut(&str, &str) -> Option<f32>,
) -> Result<Vec<(i64, f32)>, ContextError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, display_name, content FROM node n WHERE {NODE_AS_OF}"
    ))?;
    let mut rows = stmt.query(params![as_of])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        if excluded.contains(&id) {
            continue;
        }
        let display_name = row.get_ref(1)?.as_str().map_err(rusqlite::Error::from)?;
        let content = row.get_ref(2)?.as_str().map_err(rusqlite::Error::from)?;
        count_content_bytes(content.len());
        if let Some(s) = score(display_name, content) {
            out.push((id, s));
        }
    }
    Ok(out)
}

/// Full node rows for `ids` — the bodies recall fetches *after* the candidate
/// cut, so the only content it moves is content a frame can actually carry.
/// Returns them keyed by rowid; ids with no live row are simply absent.
pub(crate) fn nodes_by_ids(
    conn: &Connection,
    ids: &[i64],
    as_of: Option<&str>,
) -> Result<std::collections::HashMap<i64, NodeRow>, ContextError> {
    let mut out = std::collections::HashMap::with_capacity(ids.len());
    if ids.is_empty() {
        return Ok(out);
    }
    // Numbered explicitly, never bare `?`: SQLite assigns a bare marker "one
    // greater than the largest index assigned so far", so mixing the two forms
    // silently collides with `?1`.
    let placeholders = (0..ids.len())
        .map(|i| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT n.id, n.public_id, n.kind, n.display_name, n.content, n.content_hash, n.uri,
                n.valid_from, n.recorded_at, n.recall_tier
         FROM node n WHERE {NODE_AS_OF} AND n.id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut bound: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(ids.len() + 1);
    bound.push(&as_of);
    for id in ids {
        bound.push(id);
    }
    let rows = stmt.query_map(bound.as_slice(), map_node_row)?;
    for r in rows {
        let node = r?;
        count_content_bytes(node.content.len());
        out.insert(node.id, node);
    }
    Ok(out)
}

/// Cosine-score every live node with an embedding under `fingerprint` against
/// `query_vec`, returning `(node_id, cosine)` — **without materializing a single
/// decoded vector**.
///
/// This replaces a loader that returned `Vec<(i64, Vec<f32>)>` for the whole
/// corpus. Recall's only use for those vectors was one cosine each, plus the
/// ≤20 candidate vectors the MMR pass folds; the other N−20 were decoded into a
/// fresh heap `Vec<f32>` (256 f32 = 1 KiB apiece), scored once, and dropped.
/// Scoring straight off the borrowed BLOB retires N decode allocations and N KiB
/// of live heap per turn, and leaves the retained set at 12 bytes per node.
/// [`vectors_for_ids`] fetches the candidates' vectors afterwards.
///
/// `excluded` (the domain-scope anti-set) is applied here rather than by
/// retaining and re-filtering, so an out-of-scope node is never even scored.
pub(crate) fn score_nodes_by_vector(
    conn: &Connection,
    fingerprint: &str,
    query_vec: &[f32],
    excluded: &std::collections::HashSet<i64>,
    as_of: Option<&str>,
    cosine: impl Fn(&[f32], &[u8]) -> f32,
) -> Result<Vec<(i64, f32)>, ContextError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT n.id, e.vector
         FROM embedding e
         JOIN node n ON n.content_hash = e.content_hash
         WHERE e.fingerprint = ?2 AND {NODE_AS_OF}"
    ))?;
    let mut rows = stmt.query(params![as_of, fingerprint])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        if excluded.contains(&id) {
            continue;
        }
        // Borrowed straight out of the statement's row buffer — no owned Vec.
        let blob = row.get_ref(1)?.as_blob().map_err(rusqlite::Error::from)?;
        if !blob.len().is_multiple_of(4) {
            return Err(ContextError::Corruption(format!(
                "embedding blob length {} is not a multiple of 4",
                blob.len()
            )));
        }
        out.push((id, cosine(query_vec, blob)));
    }
    Ok(out)
}

/// Decoded vectors for `ids` under `fingerprint` — the bounded companion to
/// [`score_nodes_by_vector`], called with the candidate ids only.
pub(crate) fn vectors_for_ids(
    conn: &Connection,
    fingerprint: &str,
    ids: &[i64],
    as_of: Option<&str>,
) -> Result<std::collections::HashMap<i64, Vec<f32>>, ContextError> {
    let mut out = std::collections::HashMap::with_capacity(ids.len());
    if ids.is_empty() {
        return Ok(out);
    }
    let placeholders = (0..ids.len())
        .map(|i| format!("?{}", i + 3))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT n.id, e.vector
         FROM embedding e
         JOIN node n ON n.content_hash = e.content_hash
         WHERE e.fingerprint = ?2 AND {NODE_AS_OF}
           AND n.id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    // Bound positionally with their real types: `params_from_iter` over one
    // homogeneous iterator cannot mix the text fingerprint with integer ids.
    let mut bound: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(ids.len() + 2);
    bound.push(&as_of);
    bound.push(&fingerprint);
    for id in ids {
        bound.push(id);
    }
    let rows = stmt.query_map(bound.as_slice(), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    for r in rows {
        let (id, blob) = r?;
        out.insert(id, blob_to_vector(&blob)?);
    }
    Ok(out)
}

/// Domain names for `ids` only, sorted per node — the bounded form of
/// [`domains_by_node`].
///
/// An unscoped recall needs domains for exactly the frames it mints (they ride
/// provenance so a citation view can show them), which is at most
/// `max_frames × MMR_CANDIDATE_MULTIPLE` nodes. It was calling
/// [`domains_by_node`] instead — a full `node_domains ⋈ domain ⋈ node` scan
/// building a `HashMap` entry per tagged node in the workspace — and then
/// looking up 20 of them. A domain-*scoped* recall still needs the whole map,
/// because the overlap ranking in step 3b scores every node.
pub(crate) fn domains_for_nodes(
    conn: &Connection,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, Vec<String>>, ContextError> {
    let mut out: std::collections::HashMap<i64, Vec<String>> =
        std::collections::HashMap::with_capacity(ids.len());
    if ids.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT nd.node_id, d.name FROM node_domains nd
         JOIN domain d ON d.id = nd.domain_id
         WHERE nd.node_id IN ({placeholders})
         ORDER BY nd.node_id, d.name"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    for r in rows {
        let (id, name) = r?;
        out.entry(id).or_default().push(name);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ContextStore, NodeInput, NodeKind, upsert_node};
    use tempfile::TempDir;

    fn tmp_store() -> (TempDir, ContextStore) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("context.db");
        let store = ContextStore::open(&path).expect("open");
        (dir, store)
    }

    /// The body-free projection is only sound if SQLite's answers match Rust's.
    /// Both derived columns are computed by SQL expressions standing in for Rust
    /// string operations, and a divergence would silently change what recall
    /// dedups and what token cost it declares — the two things `NodeMeta` exists
    /// to answer without reading a body.
    #[test]
    fn live_node_metas_agrees_with_the_bodies_it_omits() {
        let (_dir, store) = tmp_store();
        // Multi-byte content is the case that separates a byte count from a
        // character count; ASCII whitespace is the case the dedup exemption
        // turns on.
        let long = "x".repeat(1000);
        let bodies = [
            ("empty", ""),
            ("spaces", "   "),
            ("ascii-ws", " \t\n "),
            ("ascii", "plain ascii body"),
            ("unicode", "héllo — unicode ✓ ünïcødé"),
            ("long", long.as_str()),
        ];
        {
            let conn = store.conn();
            for (name, content) in bodies {
                upsert_node(
                    &conn,
                    &NodeInput::new(NodeKind::Concept, name).with_content(content),
                    "2026-01-01T00:00:00Z",
                )
                .unwrap();
            }
        }

        let conn = store.conn();
        let metas = live_node_metas(&conn, None).unwrap();
        assert_eq!(metas.len(), bodies.len());
        let ids: Vec<i64> = metas.iter().map(|m| m.id).collect();
        let rows = nodes_by_ids(&conn, &ids, None).unwrap();

        for meta in &metas {
            let row = rows.get(&meta.id).expect("every meta has its row");
            assert_eq!(
                meta.content_bytes,
                row.content.len(),
                "content_bytes for {:?} must be the UTF-8 byte length, not a char \
                 count — `budget_tokens` counts bytes",
                row.display_name
            );
            assert_eq!(
                contextgraph_types::budget_tokens(&row.content),
                meta.content_bytes
                    .div_ceil(contextgraph_types::BYTES_PER_BUDGET_TOKEN) as u32,
                "declared token cost for {:?}",
                row.display_name
            );
            assert_eq!(
                meta.content_blank,
                row.content.trim().is_empty(),
                "dedup blankness for {:?}",
                row.display_name
            );
            assert_eq!(meta.content_hash, row.content_hash);
            assert_eq!(meta.public_id, row.public_id);
            assert_eq!(meta.display_name, row.display_name);
            assert_eq!(meta.recorded_at, row.recorded_at);
        }
    }

    /// The v4 index is what keeps the per-turn similarity scan proportional to
    /// the ACTIVE fingerprint's vectors. `embedding` is `WITHOUT ROWID` keyed
    /// `(content_hash, fingerprint)`, so `WHERE fingerprint = ?` matches no key
    /// prefix and without this index SQLite scans every blob ever written,
    /// including the ones a fingerprint bump stranded.
    #[test]
    fn the_fingerprint_scan_recall_runs_every_turn_is_indexed() {
        let (_dir, store) = tmp_store();
        let conn = store.conn();
        let plan: Vec<String> = conn
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT n.id, e.vector FROM embedding e
                 JOIN node n ON n.content_hash = e.content_hash
                 WHERE e.fingerprint = ?1 AND n.superseded_at IS NULL",
            )
            .unwrap()
            .query_map(params!["fp"], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let plan = plan.join(" | ");
        assert!(
            plan.contains("idx_embedding_fingerprint"),
            "recall's per-turn vector scan must use the fingerprint index; plan was: {plan}"
        );
    }

    /// End-to-end on a REAL on-disk store that predates this change: a v3
    /// `context.db` must migrate to v4 and then answer a recall with the frame
    /// bodies still attached.
    ///
    /// The unit tests above build their stores at the current version, so none of
    /// them exercises the thing an existing workspace actually does on first
    /// open. Two ways this could break in the field and not in a unit test: the
    /// v4 index could fail to apply over a populated `embedding` table, and the
    /// two-phase load could return metadata for a node whose body it then fails
    /// to fetch — leaving a frame with no content, which is the one outcome worse
    /// than a slow recall.
    #[tokio::test]
    async fn a_v3_store_on_disk_migrates_and_still_recalls_with_bodies() {
        use crate::embed::HashEmbedder;
        use crate::writeback::ContextDelta;
        use contextgraph_types::ContextQuery;
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");

        // A v3 store with real content and a real embedding, written by the v3
        // schema and stamped v3 — what an existing workspace has on disk.
        {
            let store = ContextStore::open(&path).unwrap();
            store
                .upsert(
                    ContextDelta::new()
                        .with_node(
                            NodeInput::new(NodeKind::File, "src/store.rs")
                                .with_content("open the sqlite connection in wal mode"),
                        )
                        .with_node(
                            NodeInput::new(NodeKind::Memory, "wal mode note").with_content(
                                "the store opens sqlite in wal mode with foreign keys",
                            ),
                        ),
                )
                .await
                .unwrap();
            let conn = store.conn();
            conn.execute("DROP INDEX IF EXISTS idx_embedding_fingerprint", [])
                .unwrap();
            conn.pragma_update(None, "user_version", 3i64).unwrap();
        }

        // Reopening runs the real migration.
        let store = ContextStore::open_with(
            &path,
            Arc::new(HashEmbedder::default()),
            crate::clock::FixedClock::shared(2_000),
        )
        .unwrap();
        {
            let conn = store.conn();
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                version,
                crate::store::SCHEMA_VERSION,
                "a v3 store must migrate to the current schema on open"
            );
            let indexed: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master \
                     WHERE type = 'index' AND name = 'idx_embedding_fingerprint'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(indexed, 1, "the v4 index must exist over populated rows");
        }
        // Outside the guard: the connection mutex is not reentrant, and
        // `integrity_check` takes it itself.
        store
            .integrity_check()
            .expect("still consistent after migrating");

        let result = store
            .recall(&ContextQuery {
                goal: "how does the store open sqlite".into(),
                query_text: Some("open sqlite in wal mode".into()),
                embedding: None,
                kinds: vec![],
                anchors: vec![],
                max_frames: 5,
                max_tokens: 4000,
                as_of: None,
                representation_preferences: vec![],
            })
            .await
            .unwrap();

        assert!(
            !result.frames.is_empty(),
            "a migrated store must still recall"
        );
        for frame in &result.frames {
            let content = frame.content.as_deref().unwrap_or_default();
            assert!(
                !content.is_empty(),
                "every frame must carry the body the two-phase load fetched: {frame:?}"
            );
            assert_eq!(
                frame.token_cost,
                contextgraph_types::budget_tokens(content),
                "declared cost must match the body actually attached"
            );
        }
    }
}
