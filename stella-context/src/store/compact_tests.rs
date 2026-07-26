//! Compaction tests — the witnesses that reclaiming the derived retrieval index
//! changes no answer the plane can give.
//!
//! Their own file rather than an appendix to `store/tests.rs` because that file
//! is within 400 lines of the 1500-line ratchet (`make file-size`) and these are
//! a self-contained subject: every one of them asserts some form of "this row
//! went, and nothing observable moved". The one compaction test that is NOT here
//! is the v4-fixture migration, which lives beside `open_legacy` in
//! `store/tests.rs` with the rest of the schema ladder.
//!
//! House style, deliberately: assertion messages name the law they defend, so a
//! red test says *what broke*, not merely *which line*.

use std::sync::Arc;

use rusqlite::params;
use tempfile::TempDir;

use super::*;
use crate::clock::Clock;
use crate::error::ContextError;
use crate::store::{
    ContextStore, NodeInput, NodeKind, insert_edge, tag_edge_domains, tag_node_domains, upsert_node,
};

/// Transaction time for the raw-SQL fixtures, matching `store/tests.rs`.
const T1: &str = "2026-02-01T00:00:00Z";

/// A store on a frozen clock with the default hash embedder — the shape every
/// compaction test wants, because supersession timestamps have to be pinned for
/// a point-in-time assertion to mean anything.
fn compactable_store(path: &std::path::Path) -> (ContextStore, Arc<crate::clock::FixedClock>) {
    let clock = crate::clock::FixedClock::shared(1_700_000_000);
    let store = ContextStore::open_with(
        path,
        Arc::new(crate::embed::HashEmbedder::default()),
        clock.clone(),
    )
    .unwrap();
    (store, clock)
}

fn embedding_count(store: &ContextStore) -> i64 {
    store
        .conn()
        .query_row("SELECT COUNT(*) FROM embedding", [], |r| r.get(0))
        .unwrap()
}

fn table_count(store: &ContextStore, table: &str) -> i64 {
    store
        .conn()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

/// A recall rendered as bytes, for "compaction changed nothing" comparisons.
/// Serialized rather than compared field-by-field so a frame gaining a field
/// later is covered without anyone remembering to extend the assertion.
async fn recalled_bytes(store: &ContextStore, text: &str) -> String {
    let q = contextgraph_types::ContextQuery {
        goal: "compaction must not move this".into(),
        query_text: Some(text.into()),
        embedding: None,
        kinds: vec![],
        anchors: vec![],
        max_frames: 10,
        max_tokens: 4000,
        as_of: None,
        representation_preferences: vec![],
    };
    let result = store.recall(&q).await.unwrap();
    serde_json::to_string(&result.frames).unwrap()
}

/// Write a memory, then revise it `edits` times. Returns the lineage id.
async fn memory_edited(store: &ContextStore, edits: usize) -> String {
    store
        .upsert(crate::writeback::ContextDelta::new().with_memory(
            crate::writeback::MemoryInput::reflection("revision 0", Vec::<String>::new()),
        ))
        .await
        .unwrap();
    let node_id = store.memory_nodes().unwrap()[0].public_id.clone();
    let lineage = store.memory_lineage(&node_id).unwrap().expect("lineage");
    for n in 1..=edits {
        store
            .upsert(
                crate::writeback::ContextDelta::new().with_memory(
                    crate::writeback::MemoryInput::reflection(
                        format!("revision {n}"),
                        Vec::<String>::new(),
                    )
                    .revises(&lineage),
                ),
            )
            .await
            .unwrap();
    }
    lineage
}

/// The mass this whole module exists for. A memory's mirror node is keyed by
/// lineage, so an edit rewrites its `content_hash` in place — and the vector the
/// old hash keyed is left with nothing pointing at it. N edits strand exactly N
/// vectors, one per superseded revision, and at 256 dims each is ~1 KiB that
/// nothing could ever read again.
#[tokio::test]
async fn a_memory_edited_twice_strands_two_vectors_and_compaction_reclaims_exactly_those() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let (store, _clock) = compactable_store(&path);
    memory_edited(&store, 2).await;

    assert_eq!(
        embedding_count(&store),
        3,
        "three revisions, three vectors — two of them already unreachable"
    );

    let report = store
        .compact(&ContextCompactPolicy::orphans_only())
        .unwrap();
    assert_eq!(
        report.orphaned_embeddings, 2,
        "exactly the vectors the edits stranded — one per superseded revision"
    );
    assert_eq!(
        embedding_count(&store),
        1,
        "the live revision keeps its own"
    );

    let survivor: String = store
        .conn()
        .query_row("SELECT content_hash FROM embedding", [], |r| r.get(0))
        .unwrap();
    let live = &store.memory_nodes().unwrap()[0];
    assert_eq!(
        survivor, live.content_hash,
        "and the survivor is the LIVE memory's vector, not an arbitrary one"
    );

    // Every revision is still readable: the vectors went, the record did not.
    let lineage = store
        .memory_lineage(&live.public_id)
        .unwrap()
        .expect("lineage");
    assert_eq!(
        store.memory_revisions(&lineage).unwrap().len(),
        3,
        "the history survives (L-C3) — compaction reclaims the index, not the record"
    );
    assert_eq!(
        store.memory_lineage_stats().unwrap().superseded,
        2,
        "and the derived superseded count is unchanged, because its rows are untouched"
    );
}

/// The behavioural contract in one assertion: whatever compaction reclaims, the
/// answer a caller gets must be byte-identical. If this ever fails, compaction
/// deleted something that was not disposable.
#[tokio::test]
async fn compaction_never_changes_what_recall_returns() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let (store, _clock) = compactable_store(&path);
    memory_edited(&store, 2).await;
    store
        .upsert(
            crate::writeback::ContextDelta::new()
                .with_node(
                    NodeInput::new(NodeKind::Concept, "backoff")
                        .with_content("billing retries use exponential backoff"),
                )
                .with_node(
                    NodeInput::new(NodeKind::Concept, "styling")
                        .with_content("frontend uses tailwind for styling"),
                ),
        )
        .await
        .unwrap();

    let before = recalled_bytes(&store, "revision billing retries backoff").await;
    let report = store
        .compact(&ContextCompactPolicy::orphans_only())
        .unwrap();
    assert!(report.orphaned_embeddings > 0, "the test reclaimed nothing");
    let after = recalled_bytes(&store, "revision billing retries backoff").await;

    assert_eq!(
        before, after,
        "recall is byte-identical across a compaction — the reclaimed rows were \
         unreadable by definition"
    );
}

/// The `L-C3` witness. A superseded fact edge is the plane's only record of what
/// was believed before a correction, and `facts_as_of` reconstructs it purely
/// from `recorded_at`/`superseded_at`. Compaction must leave that answer
/// bit-for-bit alone — which is why `edge` rows are a named exclusion, in any
/// state.
#[tokio::test]
async fn a_point_in_time_query_answers_identically_before_and_after_compaction() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let (store, clock) = compactable_store(&path);

    let assert_deploys_to = |target: &str| {
        crate::writeback::FactAssertion::new(
            NodeInput::new(NodeKind::Concept, "the api"),
            "deploys_to",
            NodeInput::new(NodeKind::Concept, target),
        )
    };
    store
        .upsert(crate::writeback::ContextDelta::new().with_fact(assert_deploys_to("staging")))
        .await
        .unwrap();
    let believed_at = clock.now_rfc3339();

    // A later correction closes that belief and asserts a new one.
    clock.advance(86_400);
    store
        .upsert(crate::writeback::ContextDelta::new().with_fact(assert_deploys_to("production")))
        .await
        .unwrap();
    // And an edit, so there is genuinely something for compaction to reclaim.
    memory_edited(&store, 1).await;

    let historical_before = store.facts_as_of(Some(&believed_at)).unwrap();
    let current_before = store.facts_as_of(None).unwrap();
    assert_eq!(historical_before.len(), 1);
    assert_eq!(historical_before[0].object, "staging");
    assert_eq!(current_before[0].object, "production");
    let edges_before = table_count(&store, "edge");

    let report = store
        .compact(&ContextCompactPolicy::orphans_only())
        .unwrap();
    assert!(report.orphaned_embeddings > 0, "the test reclaimed nothing");

    assert_eq!(
        store.facts_as_of(Some(&believed_at)).unwrap(),
        historical_before,
        "what we believed at T is unchanged by compaction (L-C3)"
    );
    assert_eq!(
        store.facts_as_of(None).unwrap(),
        current_before,
        "and so is what we believe now"
    );
    assert_eq!(
        table_count(&store, "edge"),
        edges_before,
        "no edge row went, superseded or otherwise — they are the audit substrate"
    );
}

/// Superseded `node` rows are a named exclusion, and this is why: a forget is a
/// tombstone that `stella memory restore` lifts by resolving *through* the
/// surviving row. A compaction that reclaimed it would silently turn every
/// reversible forget into a permanent one.
#[tokio::test]
async fn forget_then_compact_then_restore_still_works() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let (store, _clock) = compactable_store(&path);
    memory_edited(&store, 1).await;
    let node_id = store.memory_nodes().unwrap()[0].public_id.clone();

    assert!(store.supersede_node(&node_id).unwrap(), "forgotten");
    assert!(store.memory_nodes().unwrap().is_empty(), "and suppressed");

    let report = store
        .compact(&ContextCompactPolicy::orphans_only())
        .unwrap();
    assert_eq!(
        report.orphaned_embeddings, 1,
        "only the superseded revision's vector — a forgotten node still owns its own"
    );
    assert!(
        store.node_exists(&node_id).unwrap(),
        "the suppressed row survives compaction; it is a record, not an index entry"
    );

    assert!(store.restore_node(&node_id).unwrap(), "restore lifts it");
    let restored = store.memory_nodes().unwrap();
    assert_eq!(restored.len(), 1, "the memory is back");
    assert_eq!(restored[0].content, "revision 1");
    assert_eq!(
        embedding_count(&store),
        1,
        "with its vector intact — a restore must not need a re-embed"
    );
}

/// Junction rows whose owner is gone. `PRAGMA foreign_keys=ON` makes these
/// unreachable through the write path, which is exactly why the fixture has to
/// turn it off: the class exists for stores written without it, partial
/// restores, and hand-edited files — and `store/domain.rs` has never contained a
/// single `DELETE`, so nothing else would ever remove them.
#[test]
fn compaction_reclaims_domain_junctions_whose_owner_is_gone() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let (store, _clock) = compactable_store(&path);
    {
        let conn = store.conn();
        let node = upsert_node(&conn, &NodeInput::new(NodeKind::Concept, "doomed"), T1).unwrap();
        let keep = upsert_node(&conn, &NodeInput::new(NodeKind::Concept, "kept"), T1).unwrap();
        let props = serde_json::json!({});
        let edge = insert_edge(
            &conn,
            "relates_to",
            node,
            keep,
            1.0,
            &props,
            None,
            None,
            T1,
            None,
        )
        .unwrap();
        tag_node_domains(&conn, node, &["billing".to_string()], T1).unwrap();
        tag_node_domains(&conn, keep, &["billing".to_string()], T1).unwrap();
        tag_edge_domains(&conn, edge, &["billing".to_string()], T1).unwrap();
        // Orphan them the only way the schema allows.
        conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        conn.execute("DELETE FROM edge WHERE id = ?1", params![edge])
            .unwrap();
        conn.execute("DELETE FROM node WHERE id = ?1", params![node])
            .unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    }

    let report = store
        .compact(&ContextCompactPolicy::orphans_only())
        .unwrap();
    assert_eq!(report.orphaned_node_domains, 1);
    assert_eq!(report.orphaned_edge_domains, 1);
    assert_eq!(
        table_count(&store, "node_domains"),
        1,
        "the surviving node keeps its tag"
    );
    assert_eq!(table_count(&store, "edge_domains"), 0);
    assert_eq!(
        table_count(&store, "domain"),
        1,
        "the domain itself is a definition, not a junction — it stays"
    );
}

/// Stale-fingerprint vectors are inert, not garbage: nothing reads them today
/// (`L-C2` forbids mixing fingerprints), but reclaiming them makes a revert to
/// that embedder re-embed the corpus. So they survive the default policy and go
/// only when asked for — with the future bill reported before it is incurred.
#[tokio::test]
async fn stale_fingerprint_vectors_survive_unless_the_policy_asks_for_them() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    {
        let old = ContextStore::open_with(
            &path,
            Arc::new(crate::embed::HashEmbedder::with_revision("1")),
            crate::clock::FixedClock::shared(1_700_000_000),
        )
        .unwrap();
        old.upsert(
            crate::writeback::ContextDelta::new().with_node(
                NodeInput::new(NodeKind::Concept, "durable")
                    .with_content("content indexed under the old embedder"),
            ),
        )
        .await
        .unwrap();
    }
    let store = ContextStore::open_with(
        &path,
        Arc::new(crate::embed::HashEmbedder::with_revision("2")),
        crate::clock::FixedClock::shared(1_700_000_000),
    )
    .unwrap();
    store.warm_now().await.unwrap();
    assert_eq!(embedding_count(&store), 2, "one vector per fingerprint");

    let default_pass = store
        .compact(&ContextCompactPolicy::orphans_only())
        .unwrap();
    assert_eq!(
        default_pass.stale_fingerprint_embeddings, 0,
        "the default policy never re-embeds anything"
    );
    assert_eq!(embedding_count(&store), 2);

    let report = store
        .compact(&ContextCompactPolicy {
            orphans: true,
            stale_fingerprints: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(report.stale_fingerprint_embeddings, 1);
    assert_eq!(
        report.stale_vectors_of_live_nodes, 1,
        "and the cost of a revert to that embedder is stated, not hidden"
    );
    assert_eq!(embedding_count(&store), 1);

    // Recall is unaffected, because it could never see the stale vector.
    let q = contextgraph_types::ContextQuery {
        goal: "still grounded".into(),
        query_text: Some("content indexed under the old embedder".into()),
        embedding: None,
        kinds: vec![],
        anchors: vec![],
        max_frames: 5,
        max_tokens: 2000,
        as_of: None,
        representation_preferences: vec![],
    };
    let result = store.recall(&q).await.unwrap();
    assert!(!result.used_lexical_fallback, "still vector-grounded");
    assert!(!result.frames.is_empty());
}

/// A dry run must be indistinguishable from not running at all — including in
/// the watermark, which is the one place a "harmless" write would hide.
#[tokio::test]
async fn a_dry_run_reports_the_same_reclaim_without_deleting_anything() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let (store, _clock) = compactable_store(&path);
    memory_edited(&store, 2).await;

    let dry = store
        .compact(&ContextCompactPolicy {
            orphans: true,
            dry_run: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(dry.orphaned_embeddings, 2, "it reports what it would take");
    assert!(!dry.vacuumed, "and never vacuums");
    assert_eq!(embedding_count(&store), 3, "but nothing is gone");
    assert!(
        store.compaction_watermark().unwrap().is_none(),
        "a dry run leaves no trace, not even a record that it ran"
    );

    let real = store
        .compact(&ContextCompactPolicy::orphans_only())
        .unwrap();
    assert_eq!(
        real.orphaned_embeddings, dry.orphaned_embeddings,
        "the real pass takes exactly what the dry run promised"
    );
}

/// The scalar that remembers in the deleted rows' place. It exists so an
/// `embedding` count below the node count reads as "compacted", never as
/// "corrupt" — and it can only rise, so a clock that jumped backwards or a
/// second pass that found nothing cannot un-record what happened.
#[tokio::test]
async fn the_compaction_watermark_only_ever_rises() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let (store, clock) = compactable_store(&path);
    memory_edited(&store, 2).await;

    store
        .compact(&ContextCompactPolicy::orphans_only())
        .unwrap();
    let first = store.compaction_watermark().unwrap().expect("stamped");
    assert_eq!(first.orphaned_embeddings, 2);
    assert_eq!(first.compacted_at, clock.now_rfc3339());

    // A second pass with the clock wound BACK and nothing left to reclaim.
    clock.set(1_600_000_000);
    let second_report = store
        .compact(&ContextCompactPolicy::orphans_only())
        .unwrap();
    assert_eq!(second_report.orphaned_embeddings, 0, "nothing left");
    let second = store.compaction_watermark().unwrap().expect("still there");
    assert_eq!(
        second.compacted_at, first.compacted_at,
        "a backwards clock cannot walk the watermark back"
    );
    assert_eq!(
        second.orphaned_embeddings, 2,
        "and the lifetime total is cumulative, never overwritten by a smaller pass"
    );
}

/// "Nothing selected" and "nothing to reclaim" are different answers, and only
/// one of them is a mistake in the call. `stella stats prune` made the same
/// distinction; a compaction that silently did nothing would be the failure mode
/// this rule exists to prevent.
#[test]
fn compaction_refuses_a_policy_that_would_reclaim_nothing() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let (store, _clock) = compactable_store(&path);
    assert!(ContextCompactPolicy::default().is_noop());
    let Err(err) = store.compact(&ContextCompactPolicy::default()) else {
        panic!("a policy that reclaims no class must be refused, not silently obeyed");
    };
    assert!(
        matches!(err, ContextError::InvalidInput(_)),
        "unexpected error: {err:?}"
    );
    assert!(
        store.compaction_watermark().unwrap().is_none(),
        "a refused policy is not a compaction, so nothing is stamped"
    );
}
