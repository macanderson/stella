//! Tests for the recall pipeline: the scoring/fusion/packing units, the
//! end-to-end integration over a real store, and the two cost guards that
//! pin recall's complexity and I/O class rather than its wall clock.

use super::*;

use contextgraph_types::FrameKind;
use proptest::prelude::*;

fn frame(id: &str, token_cost: u32) -> ContextFrame {
    ContextFrame {
        id: id.into(),
        kind: FrameKind::Snippet,
        title: id.into(),
        content: Some(String::new()),
        uri: None,
        score: 0.5,
        token_cost,
        content_digest: None,
        representation: Representation::Full,
        content_fidelity: None,
        canonical_content_hash: None,
        content_ref: None,
        transform: None,
        minimum_content_fidelity: None,
        inline_content_requirement: None,
        canonical_token_cost: None,
        tokenizer_ref: None,
        valid_from: None,
        valid_to: None,
        recorded_at: None,
        provenance: vec![],
        citation_label: Some(id.into()),
        embedding: None,
        relations: vec![],
    }
}

#[test]
fn cosine_is_one_for_identical_and_zero_for_orthogonal() {
    assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
}

fn node_row(id: i64, content: &str) -> NodeRow {
    NodeRow {
        id,
        public_id: format!("nod_{id}"),
        kind: crate::store::NodeKind::Concept,
        display_name: format!("node {id}"),
        content: content.into(),
        content_hash: crate::store::sha256_hex(content),
        uri: None,
        valid_from: None,
        recorded_at: "2026-01-01T00:00:00Z".into(),
    }
}

/// The metadata projection of [`node_row`] — what the dedup pass actually
/// reads now that recall ranks the corpus without its bodies.
fn node_meta(id: i64, content: &str) -> NodeMeta {
    NodeMeta {
        id,
        public_id: format!("nod_{id}"),
        display_name: format!("node {id}"),
        content_hash: crate::store::sha256_hex(content),
        content_bytes: content.len(),
        content_blank: content.trim().is_empty(),
        recorded_at: "2026-01-01T00:00:00Z".into(),
    }
}

#[test]
fn dedup_keeps_distinct_empty_content_nodes_but_collapses_true_dupes() {
    // Three distinct nodes with empty content (all share sha256("")) plus
    // two nodes with identical non-empty content (a true duplicate pair).
    let nodes = [
        node_meta(1, ""),
        node_meta(2, ""),
        node_meta(3, ""),
        node_meta(4, "the same body"),
        node_meta(5, "the same body"),
    ];
    let node_by_id: HashMap<i64, &NodeMeta> = nodes.iter().map(|n| (n.id, n)).collect();
    let fused: HashMap<i64, f64> = [(1, 0.5), (2, 0.4), (3, 0.3), (4, 0.9), (5, 0.8)]
        .into_iter()
        .collect();

    let out = dedup_by_content_hash(&fused, &node_by_id);
    let kept: HashSet<i64> = out.iter().map(|(id, _)| *id).collect();

    // Every distinct empty-content node survives (the graph/taxonomy recall
    // the old sha256("")-collapse destroyed).
    assert!(kept.contains(&1) && kept.contains(&2) && kept.contains(&3));
    // The true duplicate pair collapses to exactly the strongest (id 4).
    assert!(kept.contains(&4));
    assert!(!kept.contains(&5));
    // 3 distinct empties + 1 survivor of the dup pair = 4.
    assert_eq!(out.len(), 4);
}

/// The metadata projection must answer every question the body answered,
/// or ranking the corpus without its bodies would change what recall
/// returns. Hash, declared token cost, and blankness, over the cases that
/// could diverge: empty, ASCII whitespace, multi-byte content (where a
/// character count would disagree with a byte count), and a long body.
#[test]
fn metadata_projection_answers_everything_the_body_did() {
    let long = "x".repeat(1000);
    for content in ["", " ", "\n\t ", "plain ascii", "héllo — unicode ✓", &long] {
        let row = node_row(1, content);
        let meta = node_meta(1, content);
        assert_eq!(meta.content_hash, row.content_hash, "hash for {content:?}");
        assert_eq!(
            budget_tokens_for_bytes(meta.content_bytes),
            contextgraph_types::budget_tokens(&row.content),
            "declared token cost for {content:?} must equal the protocol's \
             own count over the same bytes"
        );
        assert_eq!(
            meta.content_blank,
            row.content.trim().is_empty(),
            "dedup blankness for {content:?}"
        );
    }
}

/// Scoring straight off the stored BLOB must be bit-identical to decoding
/// first — it replaces that decode on the corpus-wide pass, so any drift
/// would silently reorder every recall.
#[test]
fn blob_cosine_matches_the_decoded_one() {
    let query: Vec<f32> = (0..64).map(|i| (i as f32 * 0.37).sin()).collect();
    let stored: Vec<f32> = (0..64).map(|i| (i as f32 * 0.11).cos()).collect();
    let blob = crate::store::vector_to_blob(&stored);
    assert_eq!(cosine_blob(&query, &blob), cosine(&query, &stored));

    // Zero-norm and length-mismatch both answer 0.0, exactly as `cosine`.
    let zeros = vec![0.0f32; 64];
    assert_eq!(
        cosine_blob(&query, &crate::store::vector_to_blob(&zeros)),
        0.0
    );
    assert_eq!(
        cosine_blob(&query, &crate::store::vector_to_blob(&[1.0])),
        0.0
    );
    assert_eq!(cosine_blob(&query, &[0u8; 3]), 0.0);
}

#[test]
fn rrf_rewards_appearing_high_in_multiple_lists() {
    let fused = rrf_fuse(&[(vec![1, 2, 3], 1.0), (vec![1, 3, 2], 1.0)], 60.0);
    // id 1 is rank-0 in both lists → strictly highest.
    assert!(fused[&1] > fused[&2]);
    assert!(fused[&1] > fused[&3]);
}

/// Build a ranked list of `len` ids, placing specific ids at specific
/// ranks. Filler ids start at 1000 so they never collide with the ids
/// under test.
fn ranked_with(placements: &[(i64, usize)], len: usize) -> Vec<i64> {
    let mut list: Vec<i64> = (0..len).map(|i| 1000 + i as i64).collect();
    for &(id, rank) in placements {
        list[rank] = id;
    }
    list
}

/// The contamination regression, reduced to its arithmetic.
///
/// Faithful to the real failure rather than a symmetric reversal (which
/// scores identically and proves nothing): `STALE` is the best semantic
/// match but was written long ago, while `FRESH` is the newest row in the
/// store with only middling similarity — exactly the shape of a previous
/// run's leftover reflections meeting an unrelated new prompt. At equal
/// weight recency hands `FRESH` the win; damped, similarity decides.
#[test]
fn recency_cannot_outrank_similarity_on_its_own() {
    const STALE: i64 = 1;
    const FRESH: i64 = 2;
    let vector_ranked = ranked_with(&[(STALE, 0), (FRESH, 50)], 200);
    let recency_ranked = ranked_with(&[(FRESH, 0), (STALE, 150)], 200);

    let equal_weight = rrf_fuse(
        &[(vector_ranked.clone(), 1.0), (recency_ranked.clone(), 1.0)],
        RRF_K,
    );
    assert!(
        equal_weight[&FRESH] > equal_weight[&STALE],
        "the pre-fix behavior this test exists to prevent: the newest row \
         beats the best semantic match purely on recency"
    );

    let damped = rrf_fuse(
        &[(vector_ranked, 1.0), (recency_ranked, RECENCY_WEIGHT)],
        RRF_K,
    );
    assert!(
        damped[&STALE] > damped[&FRESH],
        "the best semantic match must outrank the newest unrelated node"
    );
}

/// Damping must not *silence* recency — ordering comparably-relevant
/// frames is its legitimate job, and a weight of 0 would be a different
/// (also wrong) change. Adjacent similarity ranks, big recency gap: the
/// newer one still wins.
#[test]
fn recency_still_reorders_comparably_relevant_frames() {
    const OLDER: i64 = 1;
    const NEWER: i64 = 2;
    // Effectively tied on similarity (ranks 10 and 11)...
    let vector_ranked = ranked_with(&[(OLDER, 10), (NEWER, 11)], 200);
    // ...and far apart in age.
    let recency_ranked = ranked_with(&[(NEWER, 0), (OLDER, 100)], 200);

    let damped = rrf_fuse(
        &[
            (vector_ranked.clone(), 1.0),
            (recency_ranked, RECENCY_WEIGHT),
        ],
        RRF_K,
    );
    assert!(
        damped[&NEWER] > damped[&OLDER],
        "recency must still decide between frames similarity ranks alike"
    );
    // Without any recency signal the similarity order stands, which is
    // what makes the assertion above attributable to recency.
    let no_recency = rrf_fuse(&[(vector_ranked, 1.0)], RRF_K);
    assert!(no_recency[&OLDER] > no_recency[&NEWER]);
}

#[test]
fn packing_respects_frame_count() {
    let frames = vec![frame("a", 1), frame("b", 1), frame("c", 1)];
    let (kept, dropped) = pack_to_budget(frames, 1000, 2);
    assert_eq!(kept.len(), 2);
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].reason, DropReason::FrameCount);
}

#[test]
fn packing_skips_an_oversized_frame_but_fits_a_later_small_one() {
    let frames = vec![frame("big", 500), frame("small", 10)];
    let (kept, dropped) = pack_to_budget(frames, 100, 10);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].id, "small");
    assert_eq!(dropped[0].id, "big");
    assert_eq!(dropped[0].reason, DropReason::TokenBudget);
}

#[test]
fn missing_citation_is_a_constructor_error() {
    let node = NodeRow {
        id: 1,
        public_id: "nod_x".into(),
        kind: crate::store::NodeKind::Concept,
        display_name: "   ".into(), // whitespace-only → no human label
        content: "body".into(),
        content_hash: "h".into(),
        uri: None,
        valid_from: None,
        recorded_at: "2026-01-01T00:00:00Z".into(),
    };
    let err = frame_from_node(&node, 0.5, "fp", false, &[]).unwrap_err();
    assert!(matches!(err, ContextError::MissingCitation { .. }));
}

#[test]
fn lexical_frames_are_labeled_in_provenance() {
    let node = NodeRow {
        id: 1,
        public_id: "nod_x".into(),
        kind: crate::store::NodeKind::Concept,
        display_name: "a concept".into(),
        content: "body".into(),
        content_hash: "h".into(),
        uri: None,
        valid_from: None,
        recorded_at: "2026-01-01T00:00:00Z".into(),
    };
    let frame = frame_from_node(&node, 0.5, "fp", true, &["billing".to_string()]).unwrap();
    assert!(
        is_lexical_fallback(&frame),
        "fallback frames must be labeled"
    );
    let graph_frame = frame_from_node(&node, 0.5, "fp", false, &[]).unwrap();
    assert!(!is_lexical_fallback(&graph_frame));
}

proptest! {
    /// The core budgeting guarantee (`L-C5`): the packer never exceeds
    /// either budget, and no frame is silently lost — kept ⊎ dropped == in.
    #[test]
    fn packing_never_exceeds_budget_and_loses_nothing(
        costs in prop::collection::vec(0u32..300, 0..40),
        max_tokens in 0u32..500,
        max_frames in 0u32..20,
    ) {
        let n = costs.len();
        let frames: Vec<ContextFrame> =
            costs.iter().enumerate().map(|(i, c)| frame(&format!("f{i}"), *c)).collect();
        let (kept, dropped) = pack_to_budget(frames, max_tokens, max_frames);
        let kept_tokens: u64 = kept.iter().map(|f| f.token_cost as u64).sum();
        prop_assert!(kept_tokens <= max_tokens as u64);
        prop_assert!(kept.len() as u32 <= max_frames);
        prop_assert_eq!(kept.len() + dropped.len(), n);
    }
}

// End-to-end recall over a real store (public-API integration)

use crate::clock::FixedClock;
use crate::embed::HashEmbedder;
use crate::store::{ContextStore, NodeInput, NodeKind};
use crate::writeback::ContextDelta;
use contextgraph_types::ContextQuery;
use std::sync::Arc;
use tempfile::TempDir;

fn base_query(goal: &str, query_text: &str) -> ContextQuery {
    ContextQuery {
        goal: goal.into(),
        query_text: Some(query_text.into()),
        embedding: None,
        kinds: vec![],
        anchors: vec![],
        max_frames: 10,
        max_tokens: 4000,
        as_of: None,
        representation_preferences: vec![],
    }
}

async fn seeded() -> (TempDir, ContextStore) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = ContextStore::open_with(
        &path,
        Arc::new(HashEmbedder::default()),
        FixedClock::shared(1_000),
    )
    .unwrap();
    store
        .upsert(
            ContextDelta::new()
                .with_node(
                    NodeInput::new(NodeKind::File, "src/store.rs")
                        .with_uri("file:///repo/src/store.rs")
                        .with_content(
                            "open the sqlite connection in wal mode with foreign keys on",
                        ),
                )
                .with_node(
                    NodeInput::new(NodeKind::Artifact, "notes")
                        .with_content("render a bar chart of quarterly revenue in the dashboard"),
                )
                .with_node(
                    NodeInput::new(NodeKind::Concept, "budgeting")
                        .with_content("pack context frames to the token budget and report drops"),
                ),
        )
        .await
        .unwrap();
    (dir, store)
}

/// Witness for the unbounded candidate set. The MMR pass is `Θ(n²)` in the
/// candidates handed to it, and it used to be handed *every live node* —
/// the recency ranking contributes all of them at any relevance — so a
/// 5-frame recall did quadratic work in lifetime memory size and threw
/// >99% of it away at the budget pass.
///
/// This asserts the complexity class, not the wall clock: with the bound
/// in place the MMR fold is quadratic in `max_frames`, so total cosines
/// stay near the unavoidable `n` similarity scorings. Without it the fold
/// alone costs ~n²/2, which at n=120 is over 7000 extra calls.
#[tokio::test]
async fn recall_does_not_scale_quadratically_with_lifetime_memory_size() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = ContextStore::open_with(
        &path,
        Arc::new(HashEmbedder::default()),
        FixedClock::shared(1_000),
    )
    .unwrap();

    // A workspace with some history. Every one of these is live, so every
    // one lands in the recency ranking and therefore in the fused list.
    const NODES: usize = 120;
    let mut delta = ContextDelta::new();
    for i in 0..NODES {
        delta = delta.with_node(
            NodeInput::new(NodeKind::Artifact, format!("note-{i}"))
                .with_content(format!("note number {i} about packing frames to a budget")),
        );
    }
    store.upsert(delta).await.unwrap();

    let mut q = base_query("open the database", "packing frames to a budget");
    q.max_frames = 5;

    let _ = take_cosine_calls();
    let result = store.recall(&q).await.unwrap();
    let calls = take_cosine_calls();

    assert!(
        result.frames.len() <= 5,
        "the frame budget still binds: {}",
        result.frames.len()
    );

    // The similarity scoring pass legitimately touches every stored vector
    // once. The MMR fold on top must be bounded by the candidate cut
    // (5 x 4 = 20 candidates → at most 20²/2 = 200 more), NOT by n²/2.
    //
    // The counter is thread-local, so this is recall's own work and the
    // ceiling can be the real one rather than a generous fraction of the
    // blowup it is watching for.
    let candidates = 5 * MMR_CANDIDATE_MULTIPLE;
    let ceiling = NODES + candidates * candidates / 2;
    let unbounded_mmr_cost = NODES * NODES / 2;
    assert!(
        calls <= ceiling,
        "recall made {calls} cosine calls for {NODES} nodes and 5 frames; the \
         {NODES} similarity scorings plus a fold bounded by {candidates} \
         candidates is at most {ceiling}. An unbounded MMR pass would add about \
         {unbounded_mmr_cost}, which is the quadratic blowup this bound exists \
         to remove."
    );
}

/// The candidate bound must govern **I/O**, not just arithmetic.
///
/// The cosine-call guard above passes just as well when recall has already
/// loaded every body and every vector in the workspace and merely declines to
/// fold them all — which is what it used to do. This pins the other half: a
/// 5-frame recall may move only the candidates' bodies across the SQLite
/// boundary, so the bytes it reads track `max_frames`, not lifetime memory
/// size.
#[tokio::test]
async fn recall_reads_only_the_candidates_bodies_not_the_whole_corpus() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = ContextStore::open_with(
        &path,
        Arc::new(HashEmbedder::default()),
        FixedClock::shared(1_000),
    )
    .unwrap();

    const NODES: usize = 120;
    let mut delta = ContextDelta::new();
    let mut corpus_bytes = 0usize;
    for i in 0..NODES {
        let content = format!("note number {i} about packing frames to a budget");
        corpus_bytes += content.len();
        delta = delta.with_node(
            NodeInput::new(NodeKind::Artifact, format!("note-{i}")).with_content(content),
        );
    }
    store.upsert(delta).await.unwrap();

    let mut q = base_query("open the database", "packing frames to a budget");
    q.max_frames = 5;

    let _ = crate::candidates::take_content_bytes_loaded();
    let result = store.recall(&q).await.unwrap();
    let loaded = crate::candidates::take_content_bytes_loaded() as usize;

    // Measuring the graph/vector arm: the lexical fallback scan is
    // corpus-wide by definition, so it would make this assertion meaningless.
    assert!(
        !result.used_lexical_fallback,
        "this guard measures the fused arm; coverage unexpectedly fell back"
    );
    // 5 frames x MMR_CANDIDATE_MULTIPLE = 20 candidates. Their bodies are
    // all recall is entitled to read.
    let candidates = 5 * MMR_CANDIDATE_MULTIPLE;
    let budgeted = corpus_bytes * candidates / NODES;
    assert!(
        loaded <= budgeted,
        "recall read {loaded} content bytes of a {corpus_bytes}-byte corpus for \
         5 frames; the candidate bound entitles it to about {budgeted} \
         ({candidates} of {NODES} nodes). Loading the corpus and then \
         declining to score it is the cost this bound exists to remove."
    );
}

/// The bound must not become a silent truncation — `L-C5` requires every
/// scored candidate to appear in exactly one of `frames` / `dropped`.
#[tokio::test]
async fn candidates_cut_before_the_budget_pass_are_still_reported() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = ContextStore::open_with(
        &path,
        Arc::new(HashEmbedder::default()),
        FixedClock::shared(1_000),
    )
    .unwrap();

    const NODES: usize = 60;
    let mut delta = ContextDelta::new();
    for i in 0..NODES {
        delta = delta.with_node(
            NodeInput::new(NodeKind::Artifact, format!("note-{i}"))
                .with_content(format!("note number {i} about packing frames to a budget")),
        );
    }
    store.upsert(delta).await.unwrap();

    let mut q = base_query("open the database", "packing frames to a budget");
    q.max_frames = 5;
    let result = store.recall(&q).await.unwrap();

    assert_eq!(
        result.frames.len() + result.dropped.len(),
        NODES,
        "kept + dropped must partition every scored candidate, not just the \
         ones that reached the budget pass"
    );
}

#[tokio::test]
async fn recall_returns_cited_budget_respecting_frames() {
    let (_dir, store) = seeded().await;
    let q = base_query(
        "open the database",
        "open the sqlite connection in wal mode with foreign keys on",
    );
    let result = store.recall(&q).await.unwrap();
    assert!(!result.frames.is_empty(), "recall found grounding");
    assert!(
        result.assembled_tokens() <= q.max_tokens as u64,
        "packing respects the budget"
    );
    // The strongly-matching node is retrieved (coverage should be high).
    assert!(
        result.coverage >= MIN_COVERAGE,
        "coverage {} too low",
        result.coverage
    );
    assert!(
        !result.used_lexical_fallback,
        "strong coverage → real grounding, not fallback"
    );
    assert!(
        result
            .frames
            .iter()
            .any(|f| f.content.as_deref().unwrap_or("").contains("sqlite"))
    );
    // Every frame is humanly citable (`L-C4`).
    assert!(result.frames.iter().all(|f| {
        f.citation_label
            .as_deref()
            .map(|l| !l.is_empty())
            .unwrap_or(false)
    }));
}

#[tokio::test]
async fn recalled_frames_declare_honest_token_cost() {
    // §B3: a full frame's `token_cost` MUST equal `budget_tokens` over its
    // inline content — exact, no tolerance. This drives the real `recall`
    // builder with a query that provably surfaces a frame (same shape as
    // `recall_returns_cited_budget_respecting_frames`), so the check has a
    // non-empty result to bite on rather than passing vacuously.
    let (_dir, store) = seeded().await;
    let q = base_query(
        "open the database",
        "open the sqlite connection in wal mode with foreign keys on",
    );
    let result = store.recall(&q).await.unwrap();
    assert!(!result.frames.is_empty(), "the query must surface a frame");
    for frame in &result.frames {
        assert!(
            frame.declares_honest_token_cost(),
            "frame {:?} declares token_cost {} but its content is worth {} budget tokens (§B3)",
            frame.id,
            frame.token_cost,
            frame.expected_inline_token_cost(),
        );
    }
}

#[tokio::test]
async fn recall_reports_dropped_frames_under_a_tight_frame_budget() {
    let (_dir, store) = seeded().await;
    let mut q = base_query(
        "open the database",
        "open the sqlite connection in wal mode",
    );
    q.max_frames = 1;
    let result = store.recall(&q).await.unwrap();
    assert_eq!(result.frames.len(), 1, "only one frame fits");
    assert!(
        !result.dropped.is_empty(),
        "the rest are reported dropped, never silent (L-C5)"
    );
    assert!(
        result
            .dropped
            .iter()
            .all(|d| d.reason == DropReason::FrameCount)
    );
}

#[tokio::test]
async fn recall_falls_back_to_labeled_lexical_when_no_vectors_under_fingerprint() {
    // Seed vectors under fingerprint rev "1", then recall through a store
    // whose active embedder is rev "2": its vector index is empty for this
    // content, so coverage is 0 and retrieval honestly falls back to
    // lexical search — and labels those frames (`L-C6`). This also proves
    // retrieval never mixes fingerprints (`L-C2`).
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    {
        let store_a = ContextStore::open_with(
            &path,
            Arc::new(HashEmbedder::with_revision("1")),
            FixedClock::shared(1_000),
        )
        .unwrap();
        store_a
            .upsert(
                ContextDelta::new().with_node(
                    NodeInput::new(NodeKind::Concept, "flux capacitor note")
                        .with_content("the flux capacitor requires exactly gigawatts"),
                ),
            )
            .await
            .unwrap();
    }
    let store_b = ContextStore::open_with(
        &path,
        Arc::new(HashEmbedder::with_revision("2")),
        FixedClock::shared(2_000),
    )
    .unwrap();
    let q = base_query("capacitor question", "flux capacitor");
    let result = store_b.recall(&q).await.unwrap();
    assert_eq!(result.coverage, 0.0, "no rev-2 vectors → zero coverage");
    assert!(result.used_lexical_fallback);
    assert!(
        !result.frames.is_empty(),
        "lexical search found the node by term"
    );
    assert!(
        result.frames.iter().all(is_lexical_fallback),
        "every fallback frame is labeled, never dressed up as grounding (L-C6)"
    );
}
