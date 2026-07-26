//! Tests for the recall pipeline: the scoring/fusion/packing units, the
//! end-to-end integration over a real store, and the two cost guards that
//! pin recall's complexity and I/O class rather than its wall clock.

use super::*;

use proptest::prelude::*;

/// A packing candidate. Packing moved off `ContextFrame` and onto ranking
/// metadata when frame construction was deferred to the survivors (#712
/// deliverable 2), so these tests build what the packer actually sees.
fn candidate(id: &str, token_cost: u32) -> Ranked {
    Ranked {
        meta: crate::candidates::NodeMeta {
            id: id.len() as i64,
            public_id: id.into(),
            display_name: id.into(),
            content_hash: String::new(),
            // The packer spends `budget_tokens_for_bytes(content_bytes)`, so a
            // test that wants a cost of N must ask for N tokens' worth of bytes.
            content_bytes: (token_cost as usize) * 4,
            content_blank: false,
            recorded_at: "2026-01-01T00:00:00Z".into(),
        },
        relevance: 0.5,
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
        DEFAULT_RRF_K,
    );
    assert!(
        equal_weight[&FRESH] > equal_weight[&STALE],
        "the pre-fix behavior this test exists to prevent: the newest row \
         beats the best semantic match purely on recency"
    );

    let damped = rrf_fuse(
        &[
            (vector_ranked, 1.0),
            (recency_ranked, DEFAULT_RECENCY_WEIGHT),
        ],
        DEFAULT_RRF_K,
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
            (recency_ranked, DEFAULT_RECENCY_WEIGHT),
        ],
        DEFAULT_RRF_K,
    );
    assert!(
        damped[&NEWER] > damped[&OLDER],
        "recency must still decide between frames similarity ranks alike"
    );
    // Without any recency signal the similarity order stands, which is
    // what makes the assertion above attributable to recency.
    let no_recency = rrf_fuse(&[(vector_ranked, 1.0)], DEFAULT_RRF_K);
    assert!(no_recency[&OLDER] > no_recency[&NEWER]);
}

#[test]
fn packing_respects_frame_count() {
    let frames = vec![candidate("a", 1), candidate("b", 1), candidate("c", 1)];
    let (kept, dropped) = pack_to_budget(frames, 1000, 2);
    assert_eq!(kept.len(), 2);
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].reason, DropReason::FrameCount);
}

#[test]
fn packing_skips_an_oversized_frame_but_fits_a_later_small_one() {
    let frames = vec![candidate("big", 500), candidate("small", 10)];
    let (kept, dropped) = pack_to_budget(frames, 100, 10);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].meta.public_id, "small");
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
        let frames: Vec<Ranked> =
            costs.iter().enumerate().map(|(i, c)| candidate(&format!("f{i}"), *c)).collect();
        let (kept, dropped) = pack_to_budget(frames, max_tokens, max_frames);
        let kept_tokens: u64 = kept
                .iter()
                .map(|f| crate::retrieval::budget_tokens_for_bytes(f.meta.content_bytes) as u64)
                .sum();
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

    let _ = crate::cost_counters::take_cosine_calls();
    let result = store.recall(&q).await.unwrap();
    let calls = crate::cost_counters::take_cosine_calls();

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
    let candidates = 5 * DEFAULT_MMR_CANDIDATE_MULTIPLE;
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
    let mut longest_body = 0usize;
    for i in 0..NODES {
        let content = format!("note number {i} about packing frames to a budget");
        corpus_bytes += content.len();
        longest_body = longest_body.max(content.len());
        delta = delta.with_node(
            NodeInput::new(NodeKind::Artifact, format!("note-{i}")).with_content(content),
        );
    }
    store.upsert(delta).await.unwrap();

    let mut q = base_query("open the database", "packing frames to a budget");
    q.max_frames = 5;

    let _ = crate::cost_counters::take_content_bytes_loaded();
    let result = store.recall(&q).await.unwrap();
    let loaded = crate::cost_counters::take_content_bytes_loaded() as usize;

    // Measuring the graph/vector arm: the lexical fallback scan is
    // corpus-wide by definition, so it would make this assertion meaningless.
    assert!(
        !result.used_lexical_fallback,
        "this guard measures the fused arm; coverage unexpectedly fell back"
    );
    // 5 frames x DEFAULT_MMR_CANDIDATE_MULTIPLE = 20 candidates. Their bodies are all
    // recall is entitled to read. Budgeted against the LONGEST body rather than
    // the mean: which 20 nodes win is a ranking outcome, and a mean-based bound
    // would flake whenever the winners ran slightly above average.
    let candidates = 5 * DEFAULT_MMR_CANDIDATE_MULTIPLE;
    let budgeted = candidates * longest_body;
    assert!(
        loaded <= budgeted,
        "recall read {loaded} content bytes of a {corpus_bytes}-byte corpus for \
         5 frames; the candidate bound entitles it to about {budgeted} \
         ({candidates} of {NODES} nodes). Loading the corpus and then \
         declining to score it is the cost this bound exists to remove."
    );
}

/// The drop report is a numerator over the shortlist the budget actually
/// chose between — not over the corpus (#712 deliverable 3).
///
/// It used to be over the corpus, because the recency signal contributed every
/// live node to the fusion. That made a 5-frame recall on a 60-node store
/// report 55 drops and `truncated: true`, and it would have reported 4,995 on a
/// 5,000-node store: a number that grew with how long the workspace had been
/// alive and told a caller nothing they could act on.
///
/// `L-C5` still holds — nothing vanishes unreported. Candidates ranked below
/// the shortlist are reported as `candidates_cut`, kept separate because the
/// two facts differ in kind: a budget drop is reversible by asking for more,
/// while a cut candidate was judged not worth scoring.
#[tokio::test]
async fn the_drop_report_counts_candidates_considered_not_the_corpus() {
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

    // The partition invariant, now over the shortlist.
    assert_eq!(
        result.frames.len() + result.dropped.len(),
        result.considered,
        "kept + dropped must partition exactly the candidates the budget saw"
    );
    // The shortlist is `max_frames * DEFAULT_MMR_CANDIDATE_MULTIPLE`, so the
    // report is about this query, not about the workspace's history.
    assert_eq!(
        result.considered,
        (q.max_frames as usize) * DEFAULT_MMR_CANDIDATE_MULTIPLE,
        "the denominator is the shortlist the frame count asks for"
    );
    assert!(
        result.considered < NODES,
        "the whole point: {} considered out of {NODES} stored",
        result.considered
    );
    // Nothing is silent: everything the fusion ranked is either considered or
    // counted as cut.
    assert!(
        result.candidates_cut > 0,
        "a 60-node store ranks more than the shortlist, and the remainder is \
         reported rather than dropped on the floor"
    );
}

/// Asking for more frames must widen the shortlist the drops are counted
/// against — otherwise "considered" is just another constant.
#[tokio::test]
async fn the_denominator_tracks_the_requested_frame_count() {
    let (_dir, store) = seeded().await;
    let mut small = base_query("open the database", "packing frames to a budget");
    small.max_frames = 1;
    let mut large = small.clone();
    large.max_frames = 20;

    let small = store.recall(&small).await.unwrap();
    let large = store.recall(&large).await.unwrap();
    assert!(
        large.considered >= small.considered,
        "a larger frame budget considers at least as many candidates: {} vs {}",
        large.considered,
        small.considered
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
        result.coverage >= DEFAULT_MIN_COVERAGE,
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

/// Witness for #712 deliverable 7: a point-in-time recall answers from one
/// instant, across every signal.
///
/// The defect was that `as_of` reached `neighbors` and nothing else, so a
/// query about yesterday returned today's content wearing yesterday's edges.
/// Here a node written *after* the cutoff must not appear at all — which can
/// only hold if the cutoff reached the node, vector, and recency reads too.
#[tokio::test]
async fn point_in_time_recall_excludes_content_recorded_after_the_cutoff() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let clock = FixedClock::shared(1_000);
    let store =
        ContextStore::open_with(&path, Arc::new(HashEmbedder::default()), clock.clone()).unwrap();
    store
        .upsert(ContextDelta::new().with_node(
            NodeInput::new(NodeKind::Concept, "early").with_content("the flux capacitor note"),
        ))
        .await
        .unwrap();
    let cutoff = crate::clock::format_rfc3339(1_500);
    clock.set(2_000);
    store
        .upsert(ContextDelta::new().with_node(
            NodeInput::new(NodeKind::Concept, "late").with_content("the flux capacitor note two"),
        ))
        .await
        .unwrap();

    let mut q = base_query("capacitor", "the flux capacitor note");
    let now = store.recall(&q).await.unwrap();
    let titles: Vec<&str> = now.frames.iter().map(|f| f.title.as_str()).collect();
    assert!(
        titles.contains(&"early") && titles.contains(&"late"),
        "without a cutoff both are current: {titles:?}"
    );

    q.as_of = Some(cutoff);
    let past = store.recall(&q).await.unwrap();
    let titles: Vec<&str> = past.frames.iter().map(|f| f.title.as_str()).collect();
    assert!(
        titles.contains(&"early"),
        "content that existed at the cutoff must still be recalled: {titles:?}"
    );
    assert!(
        !titles.contains(&"late"),
        "content recorded after the cutoff must not appear in a point-in-time \
         recall — the parameter looked honored and was not: {titles:?}"
    );
}

/// Witness for #712 deliverable 4: a suppressed memory never occupies a budget
/// slot.
///
/// The defect was arithmetic, not philosophy. Suppression ran at the CLI on
/// frames the budget had already chosen, so a `max_frames: 1` recall that
/// ranked the suppressed memory first returned *zero* frames — the budget was
/// spent on a row that was then thrown away, and the turn silently got less
/// context than it asked for. Suppressing before the budget means the slot goes
/// to the next candidate instead.
#[tokio::test]
async fn a_superseded_memory_never_costs_a_budget_slot() {
    let (_dir, store) = seeded().await;
    let mut q = base_query(
        "open the database",
        "open the sqlite connection in wal mode with foreign keys on",
    );
    q.max_frames = 1;

    let before = store.recall(&q).await.unwrap();
    assert_eq!(before.frames.len(), 1, "one frame fits");
    let winner = before.frames[0].id.clone();

    assert!(
        store.supersede_node(&winner).unwrap(),
        "the winning frame is suppressible"
    );
    let after = store.recall(&q).await.unwrap();
    assert_eq!(
        after.frames.len(),
        1,
        "the slot goes to the next candidate — suppression must not silently \
         hand the turn fewer frames than it asked for"
    );
    assert_ne!(
        after.frames[0].id, winner,
        "and it is not the suppressed one"
    );

    // Reversible and singular (spec §5.7).
    assert!(store.restore_node(&winner).unwrap(), "restore lifts it");
    let restored = store.recall(&q).await.unwrap();
    assert_eq!(restored.frames[0].id, winner, "restore is an exact inverse");
    assert!(
        !store.supersede_node("nod_does_not_exist").unwrap(),
        "suppressing an unknown id reports no change rather than erroring"
    );
}

/// The derived half of suppression — quarantine, which has no row in this
/// database to tombstone — must also apply before the budget.
#[tokio::test]
async fn an_excluded_id_never_costs_a_budget_slot_either() {
    let (_dir, store) = seeded().await;
    let mut q = base_query(
        "open the database",
        "open the sqlite connection in wal mode with foreign keys on",
    );
    q.max_frames = 1;

    let before = store.recall(&q).await.unwrap();
    let winner = before.frames[0].id.clone();

    let excluded: std::collections::HashSet<String> = [winner.clone()].into_iter().collect();
    let after = store
        .recall_scoped_excluding(&q, &[], &excluded)
        .await
        .unwrap();
    assert_eq!(
        after.frames.len(),
        1,
        "an excluded candidate frees its slot rather than wasting it"
    );
    assert_ne!(after.frames[0].id, winner);
    assert!(
        !after.dropped.iter().any(|d| d.id == winner),
        "an excluded id is not a candidate at all, so it is not a budget drop \
         either — it never entered the ranking"
    );
}

/// Witness for #712 deliverable 8: the knobs are reachable, and reaching them
/// changes what recall does.
///
/// A settings block that deserializes but steers nothing is indistinguishable
/// from no settings block at all — which is what these knobs were before this
/// change, as eight `const`s the settings file had no path to.
#[tokio::test]
async fn tuning_reaches_the_ranking() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let make = |tuning: RecallTuning| {
        ContextStore::open_with(
            &path,
            Arc::new(HashEmbedder::default()),
            FixedClock::shared(1_000),
        )
        .unwrap()
        .with_tuning(tuning)
    };

    let store = make(RecallTuning::default());
    let mut delta = ContextDelta::new();
    for i in 0..40 {
        delta = delta.with_node(
            NodeInput::new(NodeKind::Artifact, format!("note-{i}"))
                .with_content(format!("note {i:03} about packing frames to a budget")),
        );
    }
    store.upsert(delta).await.unwrap();
    drop(store);

    let mut q = base_query("open the database", "packing frames to a budget");
    q.max_frames = 3;

    let narrow = make(RecallTuning::default()).recall(&q).await.unwrap();
    let wide = make(RecallTuning {
        mmr_candidate_multiple: 10,
        ..RecallTuning::default()
    })
    .recall(&q)
    .await
    .unwrap();
    assert!(
        wide.considered > narrow.considered,
        "widening the shortlist multiple must widen the shortlist: {} vs {}",
        wide.considered,
        narrow.considered
    );

    // The coverage floor decides grounding vs. labeled lexical fallback.
    let grounded = make(RecallTuning::default()).recall(&q).await.unwrap();
    assert!(!grounded.used_lexical_fallback);
    let forced = make(RecallTuning {
        min_coverage: 1.0,
        ..RecallTuning::default()
    })
    .recall(&q)
    .await
    .unwrap();
    assert!(
        forced.used_lexical_fallback,
        "a coverage floor of 1.0 can never be met, so retrieval must fall back \
         and say so rather than dressing weak hits up as grounding"
    );
}

/// The benchmark #712's gate names: recall work bounded by the requested frame
/// count, at three corpus sizes spanning two orders of magnitude.
///
/// The two guards above pin the same property at a single size, which cannot
/// distinguish "bounded" from "the constant happens to be small here". Three
/// sizes can: content bytes must come out *byte-identical* across a 100x corpus,
/// and the cosine scan — the one honest exception, since "most similar" is not
/// something SQLite can `ORDER BY` without an ANN index — must stay linear
/// rather than quadratic.
///
/// It measures work, not wall clock. A wall-clock assertion cannot tell
/// "bounded" from "fast on this machine today", and is the kind of test that
/// gets marked flaky and deleted.
#[tokio::test]
async fn recall_work_is_bounded_by_frame_count_not_corpus_size() {
    const FRAMES: u32 = 5;
    // (corpus, content bytes read, cosine calls)
    let mut measured: Vec<(usize, u64, usize)> = Vec::new();

    for corpus in [50usize, 500, 5_000] {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        let store = ContextStore::open_with(
            &path,
            Arc::new(HashEmbedder::default()),
            FixedClock::shared(1_000),
        )
        .unwrap();

        let mut delta = ContextDelta::new();
        for i in 0..corpus {
            // Fixed-width index so every body is byte-identical in length: the
            // byte assertion below is then exact rather than approximate, and a
            // regression cannot hide in the rounding.
            delta = delta.with_node(
                NodeInput::new(NodeKind::Artifact, format!("note-{i}")).with_content(format!(
                    "note number {i:06} about packing context frames to a token budget \
                     and reporting exactly what was dropped and why"
                )),
            );
        }
        store.upsert(delta).await.unwrap();

        let mut q = base_query("open the database", "packing frames to a budget");
        q.max_frames = FRAMES;

        let _ = crate::cost_counters::take_content_bytes_loaded();
        let _ = crate::cost_counters::take_cosine_calls();
        let result = store.recall(&q).await.unwrap();
        measured.push((
            corpus,
            crate::cost_counters::take_content_bytes_loaded(),
            crate::cost_counters::take_cosine_calls(),
        ));

        assert!(
            result.frames.len() <= FRAMES as usize,
            "the frame budget binds at corpus {corpus}: {}",
            result.frames.len()
        );
    }

    assert_eq!(
        measured[0].1, measured[2].1,
        "content bytes must not grow with the corpus — 100x the store, the same \
         bytes read: {measured:?}"
    );
    let per_node_first = measured[0].2 as f64 / measured[0].0 as f64;
    let per_node_last = measured[2].2 as f64 / measured[2].0 as f64;
    assert!(
        per_node_last < per_node_first * 2.0 + 1.0,
        "cosine cost per stored node must stay flat as the corpus grows — \
         {per_node_first:.2} at 50 nodes vs {per_node_last:.2} at 5000: {measured:?}"
    );
}
