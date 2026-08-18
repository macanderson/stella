//! Tests for the write-back path: bi-temporal supersession, the one-transaction
//! delta, the re-embed skip, and the anchor bookkeeping `stella memory validate`
//! reads.
//!
//! Split out of `writeback.rs` under the crate's own seam pattern
//! (`store.rs` → `store/tests.rs`, `retrieval.rs` → `retrieval/tests.rs`)
//! before the file crossed the gate's 1500-line ceiling (#3705).

use super::*;
use crate::clock::FixedClock;
use crate::embed::HashEmbedder;
use std::sync::Arc;
use tempfile::TempDir;

fn store_at(clock: Arc<FixedClock>) -> (TempDir, ContextStore) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = ContextStore::open_with(&path, Arc::new(HashEmbedder::default()), clock).unwrap();
    (dir, store)
}

#[tokio::test]
async fn supersession_preserves_point_in_time_belief() {
    // L-C3: correct a fact at T2; querying belief-time T1 still answers
    // with the pre-correction value; history is never destroyed.
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock.clone());

    // T1: the build system is make.
    let fact = FactAssertion::new(
        NodeInput::new(NodeKind::Concept, "build system"),
        "IS",
        NodeInput::new(NodeKind::Concept, "make"),
    );
    let r1 = store
        .upsert(ContextDelta::new().with_fact(fact))
        .await
        .unwrap();
    assert_eq!(r1.facts_asserted, 1);
    assert_eq!(r1.facts_superseded, 0);
    let t1 = store.clock().now_rfc3339();

    // T2: we learn it's actually bazel — supersede.
    clock.advance(1_000);
    let fact2 = FactAssertion::new(
        NodeInput::new(NodeKind::Concept, "build system"),
        "IS",
        NodeInput::new(NodeKind::Concept, "bazel"),
    );
    let r2 = store
        .upsert(ContextDelta::new().with_fact(fact2))
        .await
        .unwrap();
    assert_eq!(r2.facts_superseded, 1, "the make belief was superseded");
    assert_eq!(r2.facts_asserted, 1);

    // Now (currently believed): bazel.
    let now_beliefs = store.facts_as_of(None).unwrap();
    assert_eq!(now_beliefs.len(), 1);
    assert_eq!(now_beliefs[0].object, "bazel");

    // As believed at T1: make. History survived the correction.
    let then = store.facts_as_of(Some(&t1)).unwrap();
    assert_eq!(then.len(), 1, "exactly one belief held at T1");
    assert_eq!(then[0].object, "make");
    assert_eq!(then[0].subject, "build system");
}

/// #617: a single-valued assert closes **every** live belief for its
/// `(subject, predicate)`, not just the newest.
///
/// Two live edges are reachable the ordinary way — the same predicate
/// asserted multivalued, which is allowed to coexist, and later corrected
/// as single-valued. Against the old `ORDER BY id DESC LIMIT 1` lookup
/// this closed one of the two and left the store believing "the build
/// system IS make" *and* "the build system IS buck2" simultaneously, with
/// `facts_superseded` reporting 1 for a correction that replaced two.
#[tokio::test]
async fn a_single_valued_assert_closes_every_live_belief() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock.clone());
    for object in ["make", "bazel"] {
        let mut fact = FactAssertion::new(
            NodeInput::new(NodeKind::Concept, "build system"),
            "IS",
            NodeInput::new(NodeKind::Concept, object),
        );
        fact.multivalued = true;
        store
            .upsert(ContextDelta::new().with_fact(fact))
            .await
            .unwrap();
    }
    assert_eq!(
        store.facts_as_of(None).unwrap().len(),
        2,
        "the fixture needs two coexisting live beliefs"
    );
    let before_correction = store.clock().now_rfc3339();

    clock.advance(1_000);
    let receipt = store
        .upsert(ContextDelta::new().with_fact(FactAssertion::new(
            NodeInput::new(NodeKind::Concept, "build system"),
            "IS",
            NodeInput::new(NodeKind::Concept, "buck2"),
        )))
        .await
        .unwrap();

    assert_eq!(receipt.facts_superseded, 2, "both beliefs were replaced");
    assert_eq!(receipt.facts_asserted, 1);
    let live = store.facts_as_of(None).unwrap();
    assert_eq!(
        live.len(),
        1,
        "a single-valued fact must leave exactly one live belief, got {live:?}"
    );
    assert_eq!(live[0].object, "buck2");

    // L-C3: closing them is not deleting them.
    let then = store.facts_as_of(Some(&before_correction)).unwrap();
    assert_eq!(then.len(), 2, "history survives the correction");
}

#[tokio::test]
async fn reasserting_the_same_fact_is_idempotent() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock);
    let make_fact = || {
        FactAssertion::new(
            NodeInput::new(NodeKind::Concept, "lang"),
            "IS",
            NodeInput::new(NodeKind::Concept, "rust"),
        )
    };
    store
        .upsert(ContextDelta::new().with_fact(make_fact()))
        .await
        .unwrap();
    let r2 = store
        .upsert(ContextDelta::new().with_fact(make_fact()))
        .await
        .unwrap();
    assert_eq!(r2.facts_asserted, 0, "same object → no new edge");
    assert_eq!(r2.facts_superseded, 0);
    assert_eq!(store.facts_as_of(None).unwrap().len(), 1);
}

#[tokio::test]
async fn byte_identical_content_is_never_re_embedded() {
    // L-C2: the byte-compat skip. The receipt distinguishes computed from
    // reused; the second upsert of identical content computes nothing.
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock);
    let node = || {
        NodeInput::new(NodeKind::Concept, "doc")
            .with_content("a paragraph of stable indexed content")
    };
    let r1 = store
        .upsert(ContextDelta::new().with_node(node()))
        .await
        .unwrap();
    assert_eq!(r1.embeddings_computed, 1);
    assert_eq!(r1.embeddings_reused, 0);
    let r2 = store
        .upsert(ContextDelta::new().with_node(node()))
        .await
        .unwrap();
    assert_eq!(
        r2.embeddings_computed, 0,
        "identical content is reused, not re-embedded"
    );
    assert_eq!(r2.embeddings_reused, 1);
}

#[tokio::test]
async fn episodes_are_written_and_become_retrievable_nodes() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock);
    let ep = EpisodeInput::new(
        "fixed the failing budget test by clamping the token estimate",
        "2026-07-01T10:00:00Z",
        "2026-07-01T10:05:00Z",
    );
    let receipt = store
        .upsert(ContextDelta::new().with_episode(ep))
        .await
        .unwrap();
    assert_eq!(receipt.episodes_written, 1);
    assert_eq!(receipt.nodes_upserted, 1, "the episode also indexed a node");
    assert!(store.node_count().unwrap() >= 1);
}

#[tokio::test]
async fn the_builder_can_write_memories_and_domains() {
    // Witness: before `with_memory`/`with_domain` existed the builder could
    // not express the crate's most important record type at all, so every
    // memory writer had to drop to struct-literal syntax — this body did
    // not compile.
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock);
    let receipt = store
        .upsert(
            ContextDelta::new()
                .with_domain(DomainInput::new("auth", Some("login and sessions".into())))
                .with_memory(MemoryInput::reflection(
                    "prefer typed errors over stringly ones",
                    ["auth"],
                )),
        )
        .await
        .unwrap();
    assert_eq!(receipt.memories_written, 1);
    assert_eq!(receipt.nodes_upserted, 1, "the memory also indexed a node");
    assert_eq!(
        receipt.domain_tags_added, 1,
        "the memory's mirror node carries the declared domain"
    );
}

#[tokio::test]
async fn multivalued_facts_coexist() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock);
    let mut a = FactAssertion::new(
        NodeInput::new(NodeKind::Concept, "service"),
        "DEPENDS_ON",
        NodeInput::new(NodeKind::Concept, "postgres"),
    );
    a.multivalued = true;
    let mut b = FactAssertion::new(
        NodeInput::new(NodeKind::Concept, "service"),
        "DEPENDS_ON",
        NodeInput::new(NodeKind::Concept, "redis"),
    );
    b.multivalued = true;
    store
        .upsert(ContextDelta::new().with_fact(a).with_fact(b))
        .await
        .unwrap();
    let beliefs = store.facts_as_of(None).unwrap();
    assert_eq!(
        beliefs.len(),
        2,
        "both dependencies are concurrently believed"
    );
}

/// A memory carrying anchors writes one `observed_in` edge per file, and
/// the store reports them as open.
#[tokio::test]
async fn a_memory_anchors_to_the_files_it_is_about() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock.clone());

    let receipt = store
        .upsert(
            ContextDelta::new().with_memory(
                MemoryInput::reflection("handlers register in registry.rs", ["core"])
                    .with_anchors(["src/registry.rs", "src/main.rs"]),
            ),
        )
        .await
        .unwrap();

    assert_eq!(receipt.anchors_written, 2);
    let open = store.open_anchors().unwrap();
    let mut paths: Vec<String> = open.iter().map(|a| a.path.clone()).collect();
    paths.sort();
    assert_eq!(paths, vec!["src/main.rs", "src/registry.rs"]);
    assert!(
        open.iter().all(|a| a.memory.contains("registry.rs")),
        "each anchor names the memory it came from, so a scan can report it"
    );
}

/// Re-learning the same lesson must not grow the graph. The reflection loop
/// re-mines paraphrases constantly, so an anchor that duplicated per turn
/// would make one file accumulate hundreds of identical edges.
#[tokio::test]
async fn re_anchoring_the_same_file_writes_nothing_new() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock.clone());

    let delta = || {
        ContextDelta::new().with_memory(
            MemoryInput::reflection("handlers register in registry.rs", ["core"])
                .with_anchors(["src/registry.rs"]),
        )
    };
    let first = store.upsert(delta()).await.unwrap();
    clock.advance(1_000);
    let second = store.upsert(delta()).await.unwrap();

    assert_eq!(first.anchors_written, 1);
    assert_eq!(second.anchors_written, 0, "the anchor already held");
    assert_eq!(store.open_anchors().unwrap().len(), 1);
}

/// The distinction `live_triple_exists` cannot make.
///
/// A deleted file ends the anchor's world validity but leaves it believed.
/// A belief-only idempotence check therefore sees it as "already there"
/// forever, and a file that comes back would never regain an anchor.
#[tokio::test]
async fn an_anchor_reopens_after_its_file_comes_back() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock.clone());

    let delta = || {
        ContextDelta::new().with_memory(
            MemoryInput::reflection("handlers register in registry.rs", ["core"])
                .with_anchors(["src/registry.rs"]),
        )
    };
    store.upsert(delta()).await.unwrap();
    let anchor = store.open_anchors().unwrap().remove(0);

    // The file is deleted: world validity ends, belief is untouched.
    clock.advance(1_000);
    let deleted_at = store.clock().now_rfc3339();
    assert!(
        store
            .end_anchor_validity(anchor.edge_id, &deleted_at)
            .unwrap()
    );
    assert!(
        store.open_anchors().unwrap().is_empty(),
        "an ended anchor is not open"
    );

    // The file comes back and the lesson is re-learned.
    clock.advance(1_000);
    let again = store.upsert(delta()).await.unwrap();
    assert_eq!(
        again.anchors_written, 1,
        "a file that returned is a new fact about the world, not a duplicate"
    );
    assert_eq!(store.open_anchors().unwrap().len(), 1);
}

/// Re-learning a forgotten memory must revive its mirror node.
/// `insert_memory` already resurrects the record row (`superseded_at =
/// NULL` on conflict); a mirror node left tombstoned would keep the
/// lineage invisible to every candidate reader, so the re-learn would be
/// a write with no recallable effect.
#[tokio::test]
async fn re_learning_a_forgotten_memory_revives_its_mirror_node() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock.clone());
    let delta = || {
        ContextDelta::new().with_memory(MemoryInput::reflection(
            "the build system is bazel, not make",
            ["build"],
        ))
    };
    store.upsert(delta()).await.unwrap();
    let node = store.memory_nodes().unwrap().remove(0);

    clock.advance(1_000);
    assert!(store.supersede_node(&node.public_id).unwrap());
    assert!(
        store.node_by_public_id(&node.public_id).unwrap().is_none(),
        "the forgotten memory is hidden from every live reader"
    );

    clock.advance(1_000);
    store.upsert(delta()).await.unwrap();
    assert!(
        store.node_by_public_id(&node.public_id).unwrap().is_some(),
        "re-learning must lift the mirror node's tombstone"
    );

    // And recall actually serves it again — the whole point of the write.
    let q = contextgraph_types::ContextQuery {
        goal: "what is the build system".into(),
        query_text: Some("the build system is bazel, not make".into()),
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
        result.frames.iter().any(|f| f.id == node.public_id),
        "a re-learned memory must be recallable, got {:?}",
        result
            .frames
            .iter()
            .map(|f| f.id.as_str())
            .collect::<Vec<_>>()
    );
}

/// Ending an anchor twice must not move the date the file disappeared.
#[tokio::test]
async fn ending_an_anchor_twice_keeps_the_first_date() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock.clone());
    store
        .upsert(
            ContextDelta::new().with_memory(
                MemoryInput::reflection("about registry.rs", ["core"])
                    .with_anchors(["src/registry.rs"]),
            ),
        )
        .await
        .unwrap();
    let anchor = store.open_anchors().unwrap().remove(0);

    clock.advance(1_000);
    let first = store.clock().now_rfc3339();
    assert!(store.end_anchor_validity(anchor.edge_id, &first).unwrap());

    clock.advance(10_000);
    let later = store.clock().now_rfc3339();
    assert!(
        !store.end_anchor_validity(anchor.edge_id, &later).unwrap(),
        "a second scan reports no change rather than re-dating the deletion"
    );
}
