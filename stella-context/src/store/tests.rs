//! Store tests — moved verbatim out of the module's inline `mod tests` when
//! #712 split the store along the seams it already had. The assertions are
//! unchanged; only the imports the enclosing module used to supply are new.

use std::sync::atomic::Ordering;

use super::embedding::{blob_to_vector, vector_to_blob};
use super::schema::{
    MIGRATION_V1, MIGRATION_V2, MIGRATION_V3, MIGRATION_V4, MIGRATION_V6, MIGRATION_V7,
    SCHEMA_VERSION,
};
use super::*;
use tempfile::TempDir;

fn tmp_store() -> (TempDir, ContextStore) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("context.db");
    let store = ContextStore::open(&path).expect("open");
    (dir, store)
}

#[test]
fn open_creates_a_consistent_store_at_the_current_schema_version() {
    let (_dir, store) = tmp_store();
    store.integrity_check().expect("integrity");
    let conn = store.conn();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
}

/// `context.db` carries recalled memories and verbatim past prompts, so
/// it must not land at the process umask (`0644` on a default system).
/// The WAL/SHM siblings hold the same bytes and are checked too — a
/// tightened database with a world-readable `-wal` beside it would be the
/// exact false assurance this change exists to remove.
#[cfg(unix)]
#[test]
fn opening_narrows_the_database_and_its_wal_siblings_to_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = ContextStore::open(&path).unwrap();
    // Force a WAL write so `-wal` definitely exists alongside `-shm`.
    upsert_node(
        &store.conn(),
        &NodeInput::new(NodeKind::Concept, "secret-bearing content"),
        "2026-01-01T00:00:00Z",
    )
    .unwrap();

    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let target = PathBuf::from(name);
        if !target.exists() {
            continue;
        }
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode,
            0o600,
            "{} must be owner-only, found {mode:04o}",
            target.display()
        );
    }
}

#[test]
fn reopening_is_idempotent_and_does_not_remigrate() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    {
        let s = ContextStore::open(&path).unwrap();
        let conn = s.conn();
        upsert_node(
            &conn,
            &NodeInput::new(NodeKind::Concept, "keep me"),
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
    }
    let s2 = ContextStore::open(&path).unwrap();
    assert_eq!(s2.node_count().unwrap(), 1, "data survives reopen");
    s2.integrity_check().unwrap();
}

#[test]
fn opening_drops_orphaned_code_graph_tables_from_context_db() {
    // A legacy context.db that still carries the code graph's tables (from
    // the era when the tree-sitter index shared this one file) must have
    // them dropped on open — the graph now lives in codegraph.db, and
    // leaving duplicates here is the "two DBs hold the code graph" defect.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(MIGRATION_V1).unwrap();
        conn.execute_batch(MIGRATION_V2).unwrap();
        // Simulate the orphaned graph tables + a pre-V3 schema version.
        conn.execute_batch(
            "CREATE TABLE code_graph_files (id INTEGER PRIMARY KEY, path TEXT);\
                 CREATE TABLE code_graph_symbols (id INTEGER PRIMARY KEY, file_id INTEGER);\
                 CREATE TABLE code_graph_imports (id INTEGER PRIMARY KEY, from_file_id INTEGER);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 2i64).unwrap();
    }
    // Reopen through the store: the V3 migration must evict the orphans.
    let store = ContextStore::open(&path).unwrap();
    let conn = store.conn();
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name LIKE 'code_graph_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0, "orphaned code_graph_* tables must be dropped");
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
}

#[test]
fn upsert_node_updates_content_on_touch_keeping_identity() {
    let (_dir, store) = tmp_store();
    let conn = store.conn();
    let a = upsert_node(
        &conn,
        &NodeInput::new(NodeKind::File, "src/lib.rs")
            .with_uri("file:///repo/src/lib.rs")
            .with_content("v1"),
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    let b = upsert_node(
        &conn,
        &NodeInput::new(NodeKind::File, "src/lib.rs")
            .with_uri("file:///repo/src/lib.rs")
            .with_content("v2"),
        "2026-01-02T00:00:00Z",
    )
    .unwrap();
    assert_eq!(a, b, "same identity → same rowid");
    let node = node_by_id(&conn, a).unwrap().unwrap();
    assert_eq!(node.content, "v2");
}

#[test]
fn memory_nodes_and_public_id_lookup_serve_the_inspection_surface() {
    let (_dir, store) = tmp_store();
    {
        let conn = store.conn();
        upsert_node(
            &conn,
            &NodeInput::new(NodeKind::Memory, "prefer rg over grep")
                .with_content("prefer rg over grep in this workspace"),
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        upsert_node(
            &conn,
            &NodeInput::new(NodeKind::Memory, "tests live next to code")
                .with_content("tests live next to code, not in tests/"),
            "2026-01-02T00:00:00Z",
        )
        .unwrap();
        // A non-memory node must never surface in the memory listing.
        upsert_node(
            &conn,
            &NodeInput::new(NodeKind::Concept, "budgeting").with_content("c"),
            "2026-01-03T00:00:00Z",
        )
        .unwrap();
    }
    let memories = store.memory_nodes().unwrap();
    assert_eq!(memories.len(), 2, "only Memory-kind nodes");
    assert_eq!(
        memories[0].display_name, "tests live next to code",
        "newest first"
    );
    assert!(memories.iter().all(|m| m.kind == NodeKind::Memory));

    let looked_up = store
        .node_by_public_id(&memories[0].public_id)
        .unwrap()
        .expect("public id resolves");
    assert_eq!(looked_up.content, "tests live next to code, not in tests/");
    assert!(store.node_by_public_id("nod_missing").unwrap().is_none());
}

#[test]
fn empty_display_name_is_rejected() {
    let (_dir, store) = tmp_store();
    let conn = store.conn();
    let err = upsert_node(
        &conn,
        &NodeInput::new(NodeKind::Concept, "  "),
        "2026-01-01T00:00:00Z",
    )
    .unwrap_err();
    assert!(matches!(err, ContextError::InvalidInput(_)));
}

#[test]
fn vector_blob_roundtrips_little_endian() {
    let v = vec![1.0f32, -2.5, 3.25, 0.0];
    let blob = vector_to_blob(&v);
    assert_eq!(blob.len(), 16);
    assert_eq!(blob_to_vector(&blob).unwrap(), v);
}

#[test]
fn odd_length_blob_is_reported_as_corruption() {
    assert!(matches!(
        blob_to_vector(&[0u8, 1, 2]),
        Err(ContextError::Corruption(_))
    ));
}

#[test]
fn embedding_store_is_idempotent_and_reports_reuse() {
    let (_dir, store) = tmp_store();
    let conn = store.conn();
    let first = store_embedding(&conn, "hashA", "fp", &[0.1, 0.2], "2026-01-01T00:00:00Z").unwrap();
    let second =
        store_embedding(&conn, "hashA", "fp", &[0.1, 0.2], "2026-01-01T00:00:00Z").unwrap();
    assert!(first, "first insert writes a row");
    assert!(
        !second,
        "second insert is a no-op (byte-compat reuse, L-C2)"
    );
    assert!(embedding_exists(&conn, "hashA", "fp").unwrap());
    assert!(!embedding_exists(&conn, "hashA", "other-fp").unwrap());
}

#[test]
fn kill_mid_index_rolls_back_to_a_consistent_store() {
    // L-L1: a batch dropped without commit must leave no partial rows.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    {
        let s = ContextStore::open(&path).unwrap();
        let conn = s.conn();
        // Commit one durable node so the file has real content.
        upsert_node(
            &conn,
            &NodeInput::new(NodeKind::Concept, "committed"),
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
    }
    {
        // Start a batch, write several rows, then DROP without commit —
        // the stand-in for a kill mid-index.
        let s = ContextStore::open(&path).unwrap();
        let conn = s.conn();
        let tx = conn.unchecked_transaction().unwrap();
        for i in 0..10 {
            upsert_node(
                &tx,
                &NodeInput::new(NodeKind::Concept, format!("partial-{i}")),
                "2026-01-02T00:00:00Z",
            )
            .unwrap();
        }
        drop(tx); // rollback
    }
    let s = ContextStore::open(&path).unwrap();
    s.integrity_check()
        .expect("store must be consistent after a torn write");
    assert_eq!(
        s.node_count().unwrap(),
        1,
        "only the committed node survives; no partial rows"
    );
}

/// An embedder that counts how many texts it was asked to embed, wrapping
/// the real hashing projection. Lets tests prove where embedding work
/// happens (`L-C1`: never inline on query) and that identical content is
/// not re-embedded (`L-C2`).
struct CountingEmbedder {
    inner: crate::embed::HashEmbedder,
    embedded: std::sync::atomic::AtomicUsize,
    /// The largest single `embed` request seen — proves the caller batches
    /// rather than handing the backend a whole delta at once (#616 item 8).
    max_request: std::sync::atomic::AtomicUsize,
}

impl CountingEmbedder {
    fn new(revision: &str) -> Arc<Self> {
        Arc::new(Self {
            inner: crate::embed::HashEmbedder::with_revision(revision),
            embedded: std::sync::atomic::AtomicUsize::new(0),
            max_request: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn count(&self) -> usize {
        self.embedded.load(Ordering::SeqCst)
    }

    fn max_request(&self) -> usize {
        self.max_request.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl crate::embed::Embedder for CountingEmbedder {
    fn fingerprint(&self) -> EmbedderFingerprint {
        self.inner.fingerprint()
    }
    async fn embed(
        &self,
        texts: &[String],
    ) -> Result<Vec<crate::embed::Embedding>, crate::embed::EmbedError> {
        self.embedded.fetch_add(texts.len(), Ordering::SeqCst);
        self.max_request.fetch_max(texts.len(), Ordering::SeqCst);
        self.inner.embed(texts).await
    }
}

/// #616 item 8: `upsert` embeds in `warm::BATCH`-sized requests. It used to
/// hand the embedder every missing body in one call, so a full-workspace first
/// index put the entire corpus in a single request — the exact shape a backend
/// with a request-size limit rejects, and the reason the warm indexer has
/// batched since it shipped.
#[tokio::test]
async fn upsert_embeds_in_bounded_batches_not_one_request_for_the_whole_delta() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let embedder = CountingEmbedder::new("1");
    let store = ContextStore::open_with(&path, embedder.clone(), Arc::new(SystemClock)).unwrap();

    // Comfortably more than one batch, with distinct content so nothing is
    // deduped away before it reaches the embedder.
    let nodes = crate::warm::BATCH * 2 + 5;
    let mut delta = crate::writeback::ContextDelta::new();
    for i in 0..nodes {
        delta = delta.with_node(
            NodeInput::new(NodeKind::Concept, format!("node-{i}"))
                .with_content(format!("distinct body number {i}")),
        );
    }
    store.upsert(delta).await.unwrap();

    assert_eq!(
        embedder.count(),
        nodes,
        "every distinct body is embedded exactly once"
    );
    assert!(
        embedder.max_request() <= crate::warm::BATCH,
        "no single embed request may exceed warm::BATCH ({}), saw {}",
        crate::warm::BATCH,
        embedder.max_request()
    );
    assert!(
        embedder.max_request() > 1,
        "batches should be filled, not sent one text at a time (saw {})",
        embedder.max_request()
    );
}

#[tokio::test]
async fn recall_never_embeds_stored_content_inline_only_the_query() {
    // L-C1: the first query does not pay indexing. Seed content (embedded
    // once at upsert), reset the counter, then a recall must embed exactly
    // ONE text — the query itself — and nothing stored.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let embedder = CountingEmbedder::new("1");
    let store = ContextStore::open_with(&path, embedder.clone(), Arc::new(SystemClock)).unwrap();
    store
        .upsert(
            crate::writeback::ContextDelta::new()
                .with_node(NodeInput::new(NodeKind::Concept, "a").with_content("alpha content"))
                .with_node(NodeInput::new(NodeKind::Concept, "b").with_content("beta content")),
        )
        .await
        .unwrap();
    let before = embedder.count();
    let q = contextgraph_types::ContextQuery {
        goal: "find alpha".into(),
        query_text: Some("alpha content".into()),
        embedding: None,
        kinds: vec![],
        anchors: vec![],
        max_frames: 10,
        max_tokens: 4000,
        as_of: None,
        representation_preferences: vec![],
    };
    store.recall(&q).await.unwrap();
    assert_eq!(
        embedder.count() - before,
        1,
        "recall embeds only the query text, never stored content"
    );
}

#[tokio::test]
async fn open_and_warm_catches_up_embeddings_in_the_background() {
    // L-C1: warm at mount. Seed content under fingerprint rev-1, then mount
    // with a rev-2 embedder whose index is empty for this content. The
    // background warm task embeds it; after joining, a query is
    // vector-grounded (not lexical fallback) — proving warm did the work.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    {
        let a = ContextStore::open_with(
            &path,
            Arc::new(crate::embed::HashEmbedder::with_revision("1")),
            Arc::new(SystemClock),
        )
        .unwrap();
        a.upsert(
            crate::writeback::ContextDelta::new().with_node(
                NodeInput::new(NodeKind::Concept, "warmable")
                    .with_content("content that must be re-embedded under the new fingerprint"),
            ),
        )
        .await
        .unwrap();
    }
    let embedder = CountingEmbedder::new("2");
    let store =
        ContextStore::open_and_warm(&path, embedder.clone(), Arc::new(SystemClock)).unwrap();
    let warmed = store.await_warm().await.unwrap();
    assert_eq!(
        warmed, 1,
        "the background task embedded the stale-fingerprint node"
    );
    assert!(
        embedder.count() >= 1,
        "warm did real embedding work off the query path"
    );

    let q = contextgraph_types::ContextQuery {
        goal: "find it".into(),
        query_text: Some("content that must be re-embedded under the new fingerprint".into()),
        embedding: None,
        kinds: vec![],
        anchors: vec![],
        max_frames: 5,
        max_tokens: 2000,
        as_of: None,
        representation_preferences: vec![],
    };
    let result = store.recall(&q).await.unwrap();
    assert!(
        !result.used_lexical_fallback,
        "after warm, retrieval is vector-grounded"
    );
    assert!(!result.frames.is_empty());
}

/// End-to-end: a deferred memory loses the last slot to a normal one, through
/// a real store — real migration, real embedder, real ranking, real packer.
///
/// The unit test in `retrieval/tests.rs` builds `Ranked` values directly, so it
/// cannot catch the tier being dropped on the way to disk or left out of the
/// candidate scan — which is exactly where the reflection lifecycle lost the
/// distinction before: the taxonomy existed in the write path and never reached
/// the ranking.
///
/// **The deferred memory is the one that should win on rank.** The query is its
/// text verbatim, so it is the stronger match by construction and the tier is
/// the only thing that can dislodge it. An earlier version of this test queried
/// something both memories matched loosely; the durable fact won on similarity
/// alone, so the test passed with the tiering removed entirely and proved
/// nothing. Verified by mutation: reverting `pack_to_budget` to a single ranked
/// band fails this test.
#[tokio::test]
async fn a_deferred_memory_loses_the_last_slot_to_a_normal_one() {
    const PROCESS_NOTE: &str = "the agent should not retry the same command twice";
    const DOMAIN_FACT: &str = "money is stored as integer minor units";

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = ContextStore::open_with(
        &path,
        std::sync::Arc::new(crate::embed::HashEmbedder::default()),
        crate::clock::FixedClock::shared(1_000),
    )
    .unwrap();

    store
        .upsert(
            crate::writeback::ContextDelta::new()
                .with_memory(crate::writeback::MemoryInput::reflection(
                    DOMAIN_FACT,
                    Vec::<String>::new(),
                ))
                .with_memory(
                    crate::writeback::MemoryInput::reflection(PROCESS_NOTE, Vec::<String>::new())
                        .with_recall_tier(crate::retrieval::RecallTier::Deferred),
                ),
        )
        .await
        .unwrap();

    // The query IS the process note, so ranking prefers it outright.
    let one_slot = contextgraph_types::ContextQuery {
        goal: PROCESS_NOTE.into(),
        query_text: Some(PROCESS_NOTE.into()),
        embedding: None,
        kinds: vec![],
        anchors: vec![],
        max_frames: 1,
        max_tokens: 2000,
        as_of: None,
        representation_preferences: vec![],
    };
    let result = store.recall(&one_slot).await.unwrap();
    assert_eq!(
        result.frames.len(),
        1,
        "the budget allows exactly one frame"
    );
    let content = result.frames[0].content.as_deref().unwrap_or_default();
    assert_eq!(
        content, DOMAIN_FACT,
        "the durable fact must take the only slot even though the deferred \
         note is the better match"
    );

    // The negative: widen the budget and the deferred memory comes back. Without
    // this, an implementation that simply never recalled deferred memories would
    // pass the assertion above.
    let two_slots = contextgraph_types::ContextQuery {
        max_frames: 2,
        ..one_slot
    };
    let widened = store.recall(&two_slots).await.unwrap();
    let bodies: Vec<&str> = widened
        .frames
        .iter()
        .map(|f| f.content.as_deref().unwrap_or_default())
        .collect();
    assert_eq!(bodies.len(), 2, "both memories fit a two-frame budget");
    assert!(
        bodies.contains(&PROCESS_NOTE),
        "a deferred memory is still recalled when there is room, got {bodies:?}"
    );
}

#[tokio::test]
async fn scoped_recall_keeps_untagged_nodes_and_drops_out_of_scope_ones() {
    // Regression: the post-`stella init` failure mode. A workspace
    // taxonomy makes every recall domain-scoped; most memories are
    // written untagged (reflections with no domain, episodes touching no
    // covered file). The scope must keep those and exclude only nodes
    // tagged exclusively out of scope — the old hard filter returned
    // zero frames forever once a taxonomy existed.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = ContextStore::open_with(
        &path,
        Arc::new(crate::embed::HashEmbedder::default()),
        Arc::new(SystemClock),
    )
    .unwrap();
    store
        .upsert(
            crate::writeback::ContextDelta::new()
                .with_node(
                    NodeInput::new(NodeKind::Concept, "untagged-lesson")
                        .with_content("prefer rg over grep in shell commands"),
                )
                .with_node(
                    NodeInput::new(NodeKind::Concept, "in-scope")
                        .with_content("billing retries use exponential backoff")
                        .with_domains(["billing".to_string()]),
                )
                .with_node(
                    NodeInput::new(NodeKind::Concept, "out-of-scope")
                        .with_content("frontend uses tailwind for styling")
                        .with_domains(["frontend".to_string()]),
                ),
        )
        .await
        .unwrap();

    let q = contextgraph_types::ContextQuery {
        goal: "recall everything".into(),
        query_text: Some("prefer rg over grep billing retries".into()),
        embedding: None,
        kinds: vec![],
        anchors: vec![],
        max_frames: 10,
        max_tokens: 4000,
        as_of: None,
        representation_preferences: vec![],
    };
    let result = store
        .recall_scoped(&q, &["billing".to_string()])
        .await
        .unwrap();
    let titles: Vec<&str> = result.frames.iter().map(|f| f.title.as_str()).collect();
    assert!(
        titles.contains(&"untagged-lesson"),
        "untagged nodes must survive a domain scope: {titles:?}"
    );
    assert!(
        !titles.contains(&"out-of-scope"),
        "nodes tagged exclusively out of scope must be excluded: {titles:?}"
    );
}

#[tokio::test]
async fn warm_now_embeds_only_content_missing_under_the_active_fingerprint() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = ContextStore::open(&path).unwrap();
    {
        let conn = store.conn();
        upsert_node(
            &conn,
            &NodeInput::new(NodeKind::Concept, "thing").with_content("real content here"),
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
    }
    let n = store.warm_now().await.unwrap();
    assert_eq!(n, 1, "one node embedded");
    // A second warm is a no-op — the vector already exists.
    assert_eq!(store.warm_now().await.unwrap(), 0);
}

// ==================================================================
// Phase 0 characterization: `ContextQuery.as_of` bitemporal semantics
// ==================================================================
//
// The adaptive-context plan forbids *assuming* what `as_of` means
// (knowledge cutoff, world validity, or both). These tests PIN the
// observed contract of the two temporal readers that back it —
// `edges_as_of` (the public `facts_as_of`) and `neighbors` (the only
// consumer of `ContextQuery.as_of` inside `recall`). The characterized
// contract, which any future bitemporal API must preserve or migrate
// deliberately:
//
//   `as_of` filters on TRANSACTION / BELIEF time ONLY — the half-open
//   interval `[recorded_at, superseded_at)`:
//     * `as_of = None`      -> currently believed (`superseded_at IS NULL`)
//     * `as_of = Some(t)`   -> `recorded_at <= t AND
//                              (superseded_at IS NULL OR superseded_at > t)`
//   The start is INCLUSIVE (`t == recorded_at` is visible); the end is
//   EXCLUSIVE (`t == superseded_at` is NOT visible).
//
//   `as_of` DOES NOT consult world-validity (`valid_from` / `valid_to`).
//   An edge whose world-validity window has closed in the past is still
//   returned as long as it remains believed. This is the decisive
//   discriminator between "transaction time" and "world validity / both".

// Fixed, equal-length UTC instants so lexicographic order over the TEXT
// columns matches chronological order (the store compares timestamps as
// strings).
const T0: &str = "2026-01-01T00:00:00Z"; // before anything is recorded
const T1: &str = "2026-02-01T00:00:00Z"; // edge recorded (== recorded_at)
const T1_5: &str = "2026-02-15T00:00:00Z"; // strictly inside [T1, T2)
const T2: &str = "2026-03-01T00:00:00Z"; // edge superseded (== superseded_at)
const T3: &str = "2026-04-01T00:00:00Z"; // after supersession / world-validity

fn concept(conn: &Connection, name: &str) -> i64 {
    upsert_node(conn, &NodeInput::new(NodeKind::Concept, name), T1).unwrap()
}

/// Sorted `(src_id, dst_id)` pairs visible to `edges_as_of` at `as_of`.
fn edge_pairs(conn: &Connection, as_of: Option<&str>) -> Vec<(i64, i64)> {
    let mut v: Vec<(i64, i64)> = edges_as_of(conn, as_of)
        .unwrap()
        .into_iter()
        .map(|e| (e.src_id, e.dst_id))
        .collect();
    v.sort_unstable();
    v
}

/// Sorted neighbor ids of `seed` visible to `neighbors` at `as_of`.
fn neighbor_ids(conn: &Connection, seed: i64, as_of: Option<&str>) -> Vec<i64> {
    let mut v: Vec<i64> = neighbors(conn, &[seed], as_of)
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    v.sort_unstable();
    v
}

#[test]
fn as_of_none_returns_only_currently_believed_edges() {
    let (_dir, store) = tmp_store();
    let conn = store.conn();
    let (a, b, c) = (
        concept(&conn, "a"),
        concept(&conn, "b"),
        concept(&conn, "c"),
    );
    let props = serde_json::json!({});
    // Two beliefs recorded at T1: a->b and a->c.
    let e_ab = insert_edge(&conn, "relates_to", a, b, 1.0, &props, None, None, T1, None).unwrap();
    insert_edge(&conn, "relates_to", a, c, 1.0, &props, None, None, T1, None).unwrap();
    // a->b is later corrected away (superseded at T2). Never deleted.
    close_edge(&conn, e_ab, T2, T2).unwrap();

    // `None` == "currently believed": a->b is gone, a->c remains.
    assert_eq!(edge_pairs(&conn, None), vec![(a, c)]);
    assert_eq!(neighbor_ids(&conn, a, None), vec![c]);
    // Undirected: c still sees a; b no longer does.
    assert_eq!(neighbor_ids(&conn, c, None), vec![a]);
    assert_eq!(neighbor_ids(&conn, b, None), Vec::<i64>::new());
}

#[test]
fn as_of_reconstructs_half_open_transaction_interval() {
    let (_dir, store) = tmp_store();
    let conn = store.conn();
    let (a, b) = (concept(&conn, "a"), concept(&conn, "b"));
    let props = serde_json::json!({});
    // a->b believed from T1, superseded at T2: valid transaction interval
    // is the half-open [T1, T2).
    let e = insert_edge(&conn, "relates_to", a, b, 1.0, &props, None, None, T1, None).unwrap();
    close_edge(&conn, e, T2, T2).unwrap();

    // Before it was recorded: not yet believed.
    assert_eq!(edge_pairs(&conn, Some(T0)), Vec::<(i64, i64)>::new());
    assert_eq!(neighbor_ids(&conn, a, Some(T0)), Vec::<i64>::new());

    // At exactly recorded_at (T1): INCLUSIVE start -> visible.
    assert_eq!(edge_pairs(&conn, Some(T1)), vec![(a, b)]);
    assert_eq!(neighbor_ids(&conn, a, Some(T1)), vec![b]);

    // Strictly inside [T1, T2): visible.
    assert_eq!(edge_pairs(&conn, Some(T1_5)), vec![(a, b)]);
    assert_eq!(neighbor_ids(&conn, a, Some(T1_5)), vec![b]);

    // At exactly superseded_at (T2): EXCLUSIVE end -> NOT visible.
    // This is the line that pins the interval as half-open, not closed.
    assert_eq!(edge_pairs(&conn, Some(T2)), Vec::<(i64, i64)>::new());
    assert_eq!(neighbor_ids(&conn, a, Some(T2)), Vec::<i64>::new());

    // After supersession: still not visible.
    assert_eq!(edge_pairs(&conn, Some(T3)), Vec::<(i64, i64)>::new());
    assert_eq!(neighbor_ids(&conn, a, Some(T3)), Vec::<i64>::new());
}

#[test]
fn as_of_ignores_world_validity_valid_from_valid_to() {
    // THE DISCRIMINATOR. An edge whose world-validity window closed in the
    // past (`valid_to = T1`) but which is still BELIEVED (`superseded_at IS
    // NULL`) must remain visible to every `as_of` query at or after its
    // `recorded_at`. If `as_of` consulted world validity (or "both"), a
    // query at T3 > valid_to would hide it — it does not. Proves the filter
    // is transaction/belief time only.
    let (_dir, store) = tmp_store();
    let conn = store.conn();
    let (a, b) = (concept(&conn, "a"), concept(&conn, "b"));
    let props = serde_json::json!({});
    // Recorded at T1, world-valid only across [T0, T1], never superseded.
    insert_edge(
        &conn,
        "relates_to",
        a,
        b,
        1.0,
        &props,
        Some(T0), // valid_from
        Some(T1), // valid_to — world-validity ends in the past
        T1,       // recorded_at
        None,     // supersedes -> superseded_at stays NULL (still believed)
    )
    .unwrap();

    // Still believed now, despite a closed world-validity window.
    assert_eq!(edge_pairs(&conn, None), vec![(a, b)]);
    assert_eq!(neighbor_ids(&conn, a, None), vec![b]);

    // And still visible as-of T3, long after valid_to (T1): as_of never
    // reads valid_to.
    assert_eq!(edge_pairs(&conn, Some(T3)), vec![(a, b)]);
    assert_eq!(neighbor_ids(&conn, a, Some(T3)), vec![b]);

    // Only transaction time gates it: before recorded_at it is invisible.
    assert_eq!(edge_pairs(&conn, Some(T0)), Vec::<(i64, i64)>::new());
}

// ============================================================
// Phase 0 fixtures: schema-version migration is a lossless,
// idempotent replay carrying representative rows
// ============================================================
//
// `open`-ing a legacy `context.db` must migrate it to `SCHEMA_VERSION`
// without losing data, and re-opening must be a no-op. These fixtures carry
// the SAME representative rows the `as_of_*` tests characterize (a
// superseded edge, an edge whose world-validity closed in the past, a
// memory) so the migration is shown to preserve both the data AND the
// transaction-time semantics — not just an empty schema.

/// A raw connection migrated up to `version` with `user_version` stamped,
/// so `ContextStore::open` sees a legacy db to upgrade. The `node`/`edge`
/// tables are identical across v1..v7, so the crate's writers apply to any
/// version >= 1; the `memory` table requires v2, and `memory.lineage_id`
/// requires v5.
///
/// Each arm is the same statement the real ladder runs, so a fixture at
/// version N is the shape a stella that stopped at N would have left behind —
/// never today's shape with an old stamp on it.
pub(crate) fn open_legacy(path: &std::path::Path, version: i64) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(MIGRATION_V1).unwrap();
    if version >= 2 {
        conn.execute_batch(MIGRATION_V2).unwrap();
    }
    if version >= 3 {
        conn.execute_batch(MIGRATION_V3).unwrap();
    }
    if version >= 4 {
        conn.execute_batch(MIGRATION_V4).unwrap();
    }
    if version >= 5 {
        super::schema::migrate_v5(&conn).unwrap();
    }
    if version >= 6 {
        conn.execute_batch(MIGRATION_V6).unwrap();
    }
    if version >= 7 {
        conn.execute_batch(MIGRATION_V7).unwrap();
    }
    conn.pragma_update(None, "user_version", version).unwrap();
    conn
}

/// Insert a node the way a legacy binary would — the v1 column list, and
/// nothing a later migration added.
///
/// The fixtures below used [`upsert_node`] for this, which is the *current*
/// writer: it names every column the current schema has, so the first node
/// column added after v1 (`recall_tier`, v9) made three "does a v1 db migrate"
/// tests fail on the fixture rather than on the migration. A legacy row was
/// never written by a current binary, so writing one that way was the mistake.
/// Kept deliberately literal — this statement should not follow the schema.
pub(crate) fn insert_legacy_node(
    conn: &Connection,
    kind: NodeKind,
    display_name: &str,
    content: &str,
    uri: Option<&str>,
    now: &str,
) -> i64 {
    let public_id = format!("nod_legacy_{display_name}");
    conn.query_row(
        "INSERT INTO node (public_id, kind, display_name, content, content_hash, uri, properties, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, '{}', ?7)
         RETURNING id",
        params![
            public_id,
            kind.as_str(),
            display_name,
            content,
            sha256_hex(content),
            uri,
            now
        ],
        |r| r.get(0),
    )
    .unwrap()
}

/// A store stamped by a *newer* stella must be refused, not opened as-is:
/// episodic memory and the fact graph are not rebuildable, so an older
/// binary writing into a schema it does not know is unrecoverable data
/// loss. The message must name the real fault — an out-of-date binary.
#[test]
fn rejects_a_context_db_written_by_a_newer_stella() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    {
        // The full current schema, stamped one version past this build —
        // exactly what a newer stella leaves behind.
        // `open_legacy` now applies V3 itself, so applying it again here
        // would fail on a duplicate index.
        open_legacy(&path, SCHEMA_VERSION + 1);
    }

    let Err(err) = ContextStore::open(&path) else {
        panic!("a store stamped by a newer stella must not open");
    };
    assert!(
        matches!(err, ContextError::SchemaTooNew(_)),
        "unexpected error: {err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains(&(SCHEMA_VERSION + 1).to_string()), "{msg}");
    assert!(msg.contains("binary is out of date"), "{msg}");
}

#[test]
fn migrates_v1_context_db_preserving_bitemporal_edges() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let (a, b, c) = {
        let conn = open_legacy(&path, 1);
        let props = serde_json::json!({});
        let a = insert_legacy_node(&conn, NodeKind::Concept, "a", "", None, T1);
        let b = insert_legacy_node(&conn, NodeKind::Concept, "b", "", None, T1);
        let c = insert_legacy_node(&conn, NodeKind::Concept, "c", "", None, T1);
        // A belief a->b that was later corrected away (superseded at T2).
        let e_ab =
            insert_edge(&conn, "relates_to", a, b, 1.0, &props, None, None, T1, None).unwrap();
        close_edge(&conn, e_ab, T2, T2).unwrap();
        // A still-believed belief a->c whose world-validity window closed in
        // the past (valid_to = T1) — the `as_of` discriminator row.
        insert_edge(
            &conn,
            "relates_to",
            a,
            c,
            1.0,
            &props,
            Some(T0),
            Some(T1),
            T1,
            None,
        )
        .unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1, "fixture really is a v1 db before open");
        (a, b, c)
    };

    // Open through the store: migrate v1 -> SCHEMA_VERSION.
    let store = ContextStore::open(&path).unwrap();
    store.integrity_check().unwrap();
    {
        let conn = store.conn();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "v1 db upgraded to current");
        // Currently believed = only a->c (a->b superseded; a->c's past
        // valid_to is ignored by transaction-time queries).
        assert_eq!(edge_pairs(&conn, None), vec![(a, c)]);
        // Both edges still physically present, reconstructable as-of T1.
        assert_eq!(edge_pairs(&conn, Some(T1)), vec![(a, b), (a, c)]);
    }

    // Re-open: replay is a no-op — same version, same data.
    drop(store);
    let store2 = ContextStore::open(&path).unwrap();
    let conn = store2.conn();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
    assert_eq!(edge_pairs(&conn, None), vec![(a, c)]);
}

#[test]
fn migrates_v2_context_db_preserving_memories() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let mem_public = "mem_phase0fixturememory0001";
    {
        let conn = open_legacy(&path, 2);
        // Production writes a memory as a canonical `memory` row plus a
        // retrievable mirror `node` in one transaction; reproduce both.
        // Raw SQL rather than `insert_memory`, deliberately: a fixture for
        // schema version N must write version N's shape. Calling today's
        // writer would put a v5 `lineage_id` into a v2 table and test a
        // database that never existed.
        conn.execute(
            "INSERT INTO memory (public_id, kind, content, salience, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            params![mem_public, "reflection", "prefer rg over grep", 0.5, T1],
        )
        .unwrap();
        insert_legacy_node(
            &conn,
            NodeKind::Memory,
            "prefer rg over grep",
            "prefer rg over grep",
            Some(&format!("memory://{mem_public}")),
            T1,
        );
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 2, "fixture really is a v2 db before open");
    }

    let store = ContextStore::open(&path).unwrap();
    store.integrity_check().unwrap();
    {
        let conn = store.conn();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "v2 db upgraded to current");
        // The canonical memory row survived the migration.
        let content: String = conn
            .query_row(
                "SELECT content FROM memory WHERE public_id = ?1",
                params![mem_public],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(content, "prefer rg over grep");
    }
    // And it is still retrievable through the mirror node (the recall
    // surface behind `stella memory`).
    let mem_nodes = store.memory_nodes().unwrap();
    assert_eq!(mem_nodes.len(), 1);
    assert_eq!(mem_nodes[0].content, "prefer rg over grep");
}

/// The same fixture at v3 — one per schema version, because the lineage
/// backfill is the only Phase-1 change that rewrites existing rows and each
/// version reaches it by a different path.
#[test]
fn migrates_v3_context_db_preserving_memories() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let mem_public = "mem_v3fixturememory000001";
    {
        let conn = open_legacy(&path, 3);
        conn.execute(
            "INSERT INTO memory (public_id, kind, content, salience, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                mem_public,
                "note",
                "the deploy needs the migration first",
                0.0,
                T1
            ],
        )
        .unwrap();
        insert_legacy_node(
            &conn,
            NodeKind::Memory,
            "the deploy needs the migration first",
            "the deploy needs the migration first",
            Some(&format!("memory://{mem_public}")),
            T1,
        );
    }

    let store = ContextStore::open(&path).unwrap();
    store.integrity_check().unwrap();
    let stats = store.memory_lineage_stats().unwrap();
    assert_eq!(stats.lineages, 1);
    assert_eq!(stats.live, 1);
    assert_eq!(stats.superseded, 0);
    assert_eq!(
        store.memory_revisions(mem_public).unwrap().len(),
        1,
        "a migrated memory is a lineage with exactly one revision"
    );
}

/// The migration must be safe to run twice — reopening a store is the normal
/// case, and a backfill that is not idempotent corrupts on the second open.
#[test]
fn the_lineage_migration_is_idempotent_across_reopens() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let mem_public = "mem_idempotencefixture001";
    {
        let conn = open_legacy(&path, 2);
        conn.execute(
            "INSERT INTO memory (public_id, kind, content, salience, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![mem_public, "note", "a lesson", 0.0, T1],
        )
        .unwrap();
    }
    let first = {
        let store = ContextStore::open(&path).unwrap();
        store.memory_lineage_stats().unwrap()
    };
    let second = {
        let store = ContextStore::open(&path).unwrap();
        store.memory_lineage_stats().unwrap()
    };
    assert_eq!(first, second, "reopening must not change anything");
    assert_eq!(first.lineages, 1);
    assert_eq!(first.live, 1);
}

/// Witness for #712 deliverable 5: editing a memory yields one live record,
/// not two.
///
/// Identity was the hash of kind plus content, so changing a word minted a
/// second memory *and* a second mirror node, and the old text kept its own
/// vector and full participation in every future recall. Two rows, one lesson,
/// both citable, and no command could tell you which was current.
#[tokio::test]
async fn editing_a_memory_yields_one_live_record() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = ContextStore::open_with(
        &path,
        std::sync::Arc::new(crate::embed::HashEmbedder::default()),
        crate::clock::FixedClock::shared(1_000),
    )
    .unwrap();

    store
        .upsert(crate::writeback::ContextDelta::new().with_memory(
            crate::writeback::MemoryInput::reflection("prefer rg over grep", Vec::<String>::new()),
        ))
        .await
        .unwrap();
    let before = store.memory_nodes().unwrap();
    assert_eq!(before.len(), 1);
    let node_id = before[0].public_id.clone();
    let lineage = store.memory_lineage(&node_id).unwrap().expect("lineage");

    // The edit: same lineage, new words.
    store
        .upsert(
            crate::writeback::ContextDelta::new().with_memory(
                crate::writeback::MemoryInput::reflection(
                    "prefer rg over grep, and fd over find",
                    Vec::<String>::new(),
                )
                .revises(&lineage),
            ),
        )
        .await
        .unwrap();

    let after = store.memory_nodes().unwrap();
    assert_eq!(
        after.len(),
        1,
        "an edit revises the memory; it does not mint a second one: {:?}",
        after.iter().map(|n| &n.content).collect::<Vec<_>>()
    );
    assert_eq!(after[0].content, "prefer rg over grep, and fd over find");
    assert_eq!(
        after[0].public_id, node_id,
        "and the id a caller already holds still resolves to it"
    );

    // The old text is history, not a competitor: still readable, never live.
    let stats = store.memory_lineage_stats().unwrap();
    assert_eq!(stats.lineages, 1);
    assert_eq!(stats.live, 1);
    assert_eq!(stats.superseded, 1);
    let revisions = store.memory_revisions(&lineage).unwrap();
    assert_eq!(revisions.len(), 2, "the history survives (L-C3)");
    assert_eq!(
        revisions.iter().filter(|r| r.is_current()).count(),
        1,
        "exactly one revision of a lineage is live"
    );

    // And recall serves the new text only.
    let q = contextgraph_types::ContextQuery {
        goal: "shell tools".into(),
        query_text: Some("prefer rg over grep".into()),
        embedding: None,
        kinds: vec![],
        anchors: vec![],
        max_frames: 10,
        max_tokens: 4000,
        as_of: None,
        representation_preferences: vec![],
    };
    let recalled = store.recall(&q).await.unwrap();
    assert_eq!(
        recalled.frames.len(),
        1,
        "one lesson, one frame — not the old text competing with the new"
    );
    assert!(
        recalled.frames[0]
            .content
            .as_deref()
            .unwrap()
            .contains("fd")
    );
}

/// A v4 store — the last shape before memory lineage — must migrate all the way
/// to the compaction schema, and reopening must not re-run anything.
#[test]
fn a_v4_context_db_migrates_and_the_compaction_migration_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let mem_public = "mem_v4fixturememory000001";
    {
        let conn = open_legacy(&path, 4);
        conn.execute(
            "INSERT INTO memory (public_id, kind, content, salience, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![mem_public, "note", "a v4 lesson", 0.0, T1],
        )
        .unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 4, "fixture really is a v4 db before open");
    }

    let first = {
        let store = ContextStore::open(&path).unwrap();
        store.integrity_check().unwrap();
        let v: i64 = store
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "v4 db upgraded to current");
        assert!(
            store.compaction_watermark().unwrap().is_none(),
            "the table lands empty — a migration must not invent a compaction \
             that never ran"
        );
        store
            .compact(&ContextCompactPolicy::orphans_only())
            .unwrap();
        store.compaction_watermark().unwrap().expect("stamped")
    };

    // Reopen: the ladder replays as a no-op and the watermark is untouched.
    let store = ContextStore::open(&path).unwrap();
    store.integrity_check().unwrap();
    assert_eq!(
        store.compaction_watermark().unwrap().as_ref(),
        Some(&first),
        "reopening must not re-run the migration or clear the watermark"
    );
    assert_eq!(
        store.memory_lineage_stats().unwrap().lineages,
        1,
        "and the v4 memory still made it through the lineage backfill"
    );
}

// ── v8: the lifecycle ledger, and episode.lineage_id (#714) ──────────────

/// ADR 0010 point 6 (ratified 2026-07-26) says `lineage_id` lands on `memory`
/// **and `episode`**, and the plan document records Phase 1 as having delivered
/// exactly that. It did not — `migrate_v5` altered `memory` alone. v8 closes the
/// gap in favor of the ratified ADR rather than amending the ADR down to the
/// code.
///
/// The backfill is the same lossless one v5 used: every existing row is its own
/// lineage's first revision, so `lineage_id = public_id`. This asserts that on a
/// row written by a **pre-v6** binary, which is the only case where the backfill
/// does any work.
#[test]
fn v8_backfills_episode_lineage_from_public_id_without_touching_content() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    {
        let conn = open_legacy(&path, 3);
        conn.execute(
            "INSERT INTO episode (public_id, summary, files_touched, outcome, salience,
                                  started_at, ended_at, recorded_at)
             VALUES ('epi_legacy', 'fixed the tenancy leak', '[\"src/db.rs\"]', 'success',
                     0.5, '2026-01-01T00:00:00Z', '2026-01-01T01:00:00Z',
                     '2026-01-01T01:00:00Z')",
            [],
        )
        .unwrap();
    }

    let _store = ContextStore::open(&path).unwrap();
    let conn = Connection::open(&path).unwrap();

    let (lineage, summary, superseded): (String, String, Option<String>) = conn
        .query_row(
            "SELECT lineage_id, summary, superseded_at FROM episode WHERE public_id = 'epi_legacy'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        lineage, "epi_legacy",
        "an existing episode is its own lineage's first revision"
    );
    assert_eq!(
        summary, "fixed the tenancy leak",
        "the backfill read no content and changed none"
    );
    assert!(
        superseded.is_none(),
        "a migrated episode is live, not superseded"
    );
}

/// The migration is statement-level idempotent, not merely gated by the version
/// ladder — a rewound `user_version` is how the fixtures above are built and
/// what a partial restore looks like, and SQLite has no `ADD COLUMN IF NOT
/// EXISTS` to fall back on.
#[test]
fn v8_is_idempotent_across_a_rewound_user_version() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    ContextStore::open(&path).unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 5i64).unwrap();
    }
    // Re-running v6 over a database that already has every v6 object must not
    // fail on a duplicate column, index, table, or trigger.
    ContextStore::open(&path).unwrap();
    let conn = Connection::open(&path).unwrap();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
}

/// The ledger's append-only guarantee survives a migration, because it is
/// installed as triggers rather than enforced by the writing code.
#[test]
fn v8_installs_the_append_only_triggers_on_a_migrated_store() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    drop(open_legacy(&path, 3));
    let store = ContextStore::open(&path).unwrap();
    store
        .append_record(crate::LedgerAppend {
            record_id: "obs_1",
            lineage_id: "obs_1",
            record_kind: "observation",
            record_hash: "sha256:aa",
            schema_version: "1.0-draft",
            body: "{}",
            observed_at: "2026-07-26T12:00:00Z",
            supersedes: None,
        })
        .unwrap();
    drop(store);

    let conn = Connection::open(&path).unwrap();
    assert!(
        conn.execute("DELETE FROM context_records", []).is_err(),
        "a migrated store's ledger is rewritable"
    );
}

/// Not one legacy row is rewritten by v8 (ADR 0010: new kinds are born
/// canonical and have no migration). The only writes are the additive episode
/// columns and their backfill.
#[test]
fn v8_creates_the_ledger_empty() {
    let dir = TempDir::new().unwrap();
    let store = ContextStore::open(dir.path().join("context.db")).unwrap();
    assert!(
        store.record_counts().unwrap().is_empty(),
        "a fresh ledger holds nothing — records are born, never migrated in"
    );
}
