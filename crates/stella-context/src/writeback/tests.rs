//! Tests for the write-back path: bi-temporal supersession, the one-transaction
//! delta, the re-embed skip, and the anchor bookkeeping `stella memory validate`
//! reads.
//!
//! Split out of `writeback.rs` under the crate's own seam pattern
//! (`store.rs` → `store/tests.rs`, `retrieval.rs` → `retrieval/tests.rs`)
//! before the file crossed the gate's 1500-line ceiling (#3705).

use super::*;
use crate::clock::FixedClock;
use crate::embed::{Embedder, HashEmbedder};
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
        open.iter().all(|a| a.source.contains("registry.rs")),
        "each anchor names the memory it came from, so a scan can report it"
    );
}

/// **The witness (#5338).** An episode's `files_touched` become `observed_in`
/// edges, so "what happened to this file" reaches the turns that touched it.
///
/// The paths were stored as a JSON array on the `episode` row and nowhere
/// else, which is a column a traversal cannot follow: the graph channel the
/// anchors design exists for reached memories and stopped there.
#[tokio::test]
async fn an_episode_anchors_to_every_file_it_touched() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock.clone());

    let receipt = store
        .upsert(
            ContextDelta::new().with_episode(
                EpisodeInput::new(
                    "split the registry",
                    "2026-01-01T00:00:00Z",
                    "2026-01-01T00:05:00Z",
                )
                .with_files(["src/registry.rs", "src/main.rs"])
                .with_domains(["core"]),
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
        open.iter().all(|a| a.source.contains("split the registry")),
        "each anchor names the record it came from, so a scan can report it"
    );
}

/// The same de-duplication memories get. A session that re-writes an episode
/// over the same window updates one row, and must not grow one edge per pass.
#[tokio::test]
async fn re_anchoring_an_episodes_files_writes_nothing_new() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock.clone());

    let delta = || {
        ContextDelta::new().with_episode(
            EpisodeInput::new(
                "split the registry",
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:05:00Z",
            )
            .with_files(["src/registry.rs"]),
        )
    };
    let first = store.upsert(delta()).await.unwrap();
    clock.advance(1_000);
    let second = store.upsert(delta()).await.unwrap();

    assert_eq!(first.anchors_written, 1);
    assert_eq!(second.anchors_written, 0, "the anchor already held");
    assert_eq!(store.open_anchors().unwrap().len(), 1);
}

/// The staleness scan sees episode anchors on the same terms as memory ones:
/// a deleted file ends world validity, and belief is untouched. Anything else
/// would leave episode anchors pointing at files that are gone forever, since
/// the scan is the only thing that ends them.
#[tokio::test]
async fn an_episode_anchor_ends_world_validity_like_a_memory_anchor() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock.clone());
    store
        .upsert(
            ContextDelta::new().with_episode(
                EpisodeInput::new(
                    "split the registry",
                    "2026-01-01T00:00:00Z",
                    "2026-01-01T00:05:00Z",
                )
                .with_files(["src/registry.rs"]),
            ),
        )
        .await
        .unwrap();

    let anchor = store.open_anchors().unwrap().remove(0);
    clock.advance(1_000);
    let gone_at = store.clock().now_rfc3339();
    assert!(store.end_anchor_validity(anchor.edge_id, &gone_at).unwrap());
    assert!(
        store.open_anchors().unwrap().is_empty(),
        "the anchor stopped holding in the world"
    );
    assert!(
        store
            .facts_as_of(None)
            .unwrap()
            .iter()
            .any(|f| f.predicate == crate::writeback::ANCHOR_REL),
        "the anchor is still BELIEVED; only the present stops seeing it"
    );
}

/// An episode that touched nothing writes no edges and no file nodes — the
/// common case for a summary with no `files_touched`.
#[tokio::test]
async fn an_episode_touching_no_files_anchors_nothing() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock.clone());
    let receipt = store
        .upsert(ContextDelta::new().with_episode(EpisodeInput::new(
            "thought about it",
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:05:00Z",
        )))
        .await
        .unwrap();
    assert_eq!(receipt.anchors_written, 0);
    assert!(store.open_anchors().unwrap().is_empty());
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

// ── Identity, validation, and the reuse race ────────────────────────────────

/// How many `episode` rows the store holds.
fn episode_count(store: &ContextStore) -> i64 {
    store
        .conn()
        .query_row("SELECT COUNT(*) FROM episode", [], |r| r.get(0))
        .unwrap()
}

/// Two turns that share a prompt and a second are two episodes.
///
/// Both timestamps are second-resolution, so the summary and the window come
/// out equal for two distinct turns often enough to matter — and the second
/// write then landed on the first turn's row, replacing its outcome and its
/// file list with another turn's. The occurrence key is how a caller says the
/// two are different; without it this test writes one row.
#[tokio::test]
async fn two_turns_in_one_second_write_two_episodes() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock);
    for occurrence in ["execution:41", "execution:42"] {
        store
            .upsert(
                ContextDelta::new().with_episode(
                    EpisodeInput::new(
                        "run the failing test",
                        "2026-07-01T10:00:00Z",
                        "2026-07-01T10:00:00Z",
                    )
                    .with_occurrence(occurrence),
                ),
            )
            .await
            .unwrap();
    }
    assert_eq!(episode_count(&store), 2, "one turn overwrote the other");

    // The same key again is still the update it always was — a turn recorded
    // twice must not double its row.
    store
        .upsert(
            ContextDelta::new().with_episode(
                EpisodeInput::new(
                    "run the failing test",
                    "2026-07-01T10:00:00Z",
                    "2026-07-01T10:00:00Z",
                )
                .with_occurrence("execution:42"),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        episode_count(&store),
        2,
        "re-recording one turn added a row"
    );
}

/// An episode written with no occurrence key keeps the identity it always had.
#[tokio::test]
async fn an_episode_without_an_occurrence_keeps_its_old_identity() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock);
    for _ in 0..2 {
        store
            .upsert(ContextDelta::new().with_episode(EpisodeInput::new(
                "the same turn, recorded twice",
                "2026-07-01T10:00:00Z",
                "2026-07-01T10:05:00Z",
            )))
            .await
            .unwrap();
    }
    assert_eq!(episode_count(&store), 1);
}

/// An embedder that reclaims one stored vector while it is embedding.
///
/// This is the `compact` race, made deterministic and single-threaded: the
/// store holds no lock while it embeds, so anything may delete a row in that
/// window, and the row this deletes is one the same delta was about to reuse.
/// A real reclaim is legitimate — until this delta's nodes exist, that vector
/// belongs to no node and is exactly what `compact` collects.
struct ReclaimingEmbedder {
    inner: HashEmbedder,
    /// The store's own connection, attached after the store is built.
    conn: std::sync::Mutex<Option<Arc<std::sync::Mutex<rusqlite::Connection>>>>,
    /// The content hash to reclaim.
    victim: String,
    /// Which call reclaims it. Only one, so the retry can settle.
    reclaim_on_call: usize,
    calls: std::sync::atomic::AtomicUsize,
}

impl ReclaimingEmbedder {
    fn new(victim: &str, reclaim_on_call: usize) -> Self {
        Self {
            inner: HashEmbedder::default(),
            conn: std::sync::Mutex::new(None),
            victim: victim.to_string(),
            reclaim_on_call,
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn attach(&self, conn: Arc<std::sync::Mutex<rusqlite::Connection>>) {
        *self.conn.lock().expect("attach") = Some(conn);
    }
}

impl ReclaimingEmbedder {
    /// Delete the victim row through the store's own connection. Nothing holds
    /// that lock while the store is embedding.
    fn reclaim(&self) {
        let handle = self.conn.lock().expect("reclaim").clone();
        let Some(conn) = handle else {
            return;
        };
        conn.lock()
            .expect("reclaim conn")
            .execute(
                "DELETE FROM embedding WHERE content_hash = ?1",
                [self.victim.as_str()],
            )
            .expect("reclaim the orphan");
    }
}

#[async_trait::async_trait]
impl Embedder for ReclaimingEmbedder {
    fn fingerprint(&self) -> crate::embed::EmbedderFingerprint {
        self.inner.fingerprint()
    }

    fn similarity_posture(&self) -> crate::embed::SimilarityPosture {
        self.inner.similarity_posture()
    }

    async fn embed(
        &self,
        texts: &[String],
    ) -> Result<Vec<crate::embed::Embedding>, crate::embed::EmbedError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if call == self.reclaim_on_call {
            self.reclaim();
        }
        self.inner.embed(texts).await
    }
}

/// A vector reclaimed mid-write is embedded again, not assumed.
///
/// The reuse decision is made before the embedder runs and the write happens
/// after it, so a `compact` in that window can leave the new node with no
/// vector at all — invisible to similarity recall until the next mount's warm
/// indexer finds it. The write transaction re-asks, and retries the delta with
/// the lost content in the embed set.
#[tokio::test]
async fn a_vector_reclaimed_while_embedding_is_written_again() {
    let clock = FixedClock::shared(1_000);
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let reused_hash = crate::store::sha256_hex("alpha content");
    // Reclaim on the second call: the first embeds "alpha content" itself, and
    // the second serves the delta that plans to reuse it.
    let embedder = Arc::new(ReclaimingEmbedder::new(&reused_hash, 2));
    let store = ContextStore::open_with(&path, embedder.clone(), clock).unwrap();
    embedder.attach(store.conn_handle());
    let fingerprint = store.fingerprint().id();

    store
        .upsert(
            ContextDelta::new().with_node(
                NodeInput::new(NodeKind::Concept, "alpha").with_content("alpha content"),
            ),
        )
        .await
        .unwrap();
    assert!(embedding_exists(&store.conn(), &reused_hash, &fingerprint).unwrap());

    // This delta reuses "alpha content" and embeds "beta content"; the vector
    // it plans to reuse is reclaimed while that embedding is computed.
    store
        .upsert(
            ContextDelta::new()
                .with_node(
                    NodeInput::new(NodeKind::Concept, "alpha again").with_content("alpha content"),
                )
                .with_node(NodeInput::new(NodeKind::Concept, "beta").with_content("beta content")),
        )
        .await
        .unwrap();

    assert!(
        embedding_exists(&store.conn(), &reused_hash, &fingerprint).unwrap(),
        "the reclaimed vector was never written back, so its node is unembedded"
    );
    assert!(
        embedding_exists(
            &store.conn(),
            &crate::store::sha256_hex("beta content"),
            &fingerprint
        )
        .unwrap()
    );
}

/// One unwritable record is named before anything is written.
///
/// A memory's mirror node takes its label from the memory's own text, so a
/// whitespace-only lesson mints a node with no citable name and the store
/// refuses it (`L-C4`). A refusal half way through the transaction costs the
/// delta its embeddings and rolls the whole batch back, so one blank lesson
/// discards every good lesson written beside it with nothing saying which
/// record was at fault.
#[tokio::test]
async fn a_blank_record_is_named_before_anything_is_written() {
    let clock = FixedClock::shared(1_000);
    let (_dir, store) = store_at(clock);
    let err = store
        .upsert(
            ContextDelta::new()
                .with_memory(MemoryInput::reflection(
                    "a real lesson",
                    Vec::<String>::new(),
                ))
                .with_memory(MemoryInput::reflection("   \n ", Vec::<String>::new())),
        )
        .await
        .unwrap_err();
    match err {
        ContextError::InvalidInput(message) => {
            assert!(message.contains("memory"), "unhelpful message: {message}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
    assert_eq!(
        store.node_count().unwrap(),
        0,
        "a rejected delta wrote rows"
    );

    // The good half of that batch is perfectly writable on its own, which is
    // what makes dropping the blank record at the caller the right repair.
    store
        .upsert(ContextDelta::new().with_memory(MemoryInput::reflection(
            "a real lesson",
            Vec::<String>::new(),
        )))
        .await
        .unwrap();
    assert_eq!(store.node_count().unwrap(), 1);
}
