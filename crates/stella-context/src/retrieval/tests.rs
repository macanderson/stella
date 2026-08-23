//! Tests for the recall pipeline's pure units: the scoring, fusion, and
//! packing functions in [`super::ranking`], property-tested over owned data.
//!
//! The end-to-end integration over a real store — and the two cost guards
//! that pin recall's complexity and I/O class rather than its wall clock —
//! live beside this file in [`super::recall_tests`](super::recall_tests),
//! split out before this one crossed the gate's 1500-line ceiling (#3705).

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
            recall_tier: RecallTier::Normal,
        },
        relevance: 0.5,
        selection_reason: SelectionReason::Ranked,
    }
}

/// A candidate the caller asked for by name — ranking may not evict it.
fn required(id: &str, token_cost: u32) -> Ranked {
    Ranked {
        selection_reason: SelectionReason::Anchored,
        ..candidate(id, token_cost)
    }
}

/// A candidate that yields to every `Normal` one when the budget binds.
fn deferred(id: &str, token_cost: u32) -> Ranked {
    let mut c = candidate(id, token_cost);
    c.meta.recall_tier = RecallTier::Deferred;
    c
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
        recorded_at: "2026-01-01T00:00:00Z".into(),
        recall_tier: RecallTier::Normal,
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
        recall_tier: RecallTier::Normal,
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

// ── Deferred items yield the budget, and nothing else ───────────────────────

/// The gate criterion for the tier. The deferred candidate ranks FIRST — it
/// would win under a strictly rank-ordered packer — and there is exactly one
/// slot, so this fails on any implementation that reads the tier as a tiebreak
/// or ignores it.
#[test]
fn a_deferred_item_yields_its_slot_to_a_lower_ranked_normal_one() {
    let frames = vec![deferred("process_note", 1), candidate("domain_fact", 1)];
    let (kept, dropped) = pack_to_budget(frames, 1000, 1);
    let ids: Vec<&str> = kept.iter().map(|k| k.meta.public_id.as_str()).collect();
    assert_eq!(ids, vec!["domain_fact"], "kept: {ids:?}");
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].id, "process_note");
    assert_eq!(dropped[0].reason, DropReason::FrameCount);
}

/// The negative that makes the test above mean something: a tier is not a
/// score. With room for everyone the deferred candidate is admitted, so what
/// the assertion above measures is the *budget binding*, not a general demotion
/// that would quietly cost recall a frame on every uncontended turn.
#[test]
fn a_deferred_item_is_admitted_whenever_the_budget_has_room() {
    let frames = vec![deferred("process_note", 1), candidate("domain_fact", 1)];
    let (kept, dropped) = pack_to_budget(frames, 1000, 5);
    let ids: HashSet<&str> = kept.iter().map(|k| k.meta.public_id.as_str()).collect();
    assert!(ids.contains("process_note"), "kept: {ids:?}");
    assert!(ids.contains("domain_fact"), "kept: {ids:?}");
    assert!(dropped.is_empty(), "dropped: {dropped:?}");
}

/// Rank still decides everything *within* a band — the tier partitions, it does
/// not re-sort. Two deferred candidates compete with each other in the order
/// ranking put them in, so the stronger one takes the leftover slot.
#[test]
fn rank_order_still_decides_among_deferred_items() {
    let frames = vec![
        candidate("normal", 1),
        deferred("stronger", 1),
        deferred("weaker", 1),
    ];
    let (kept, _) = pack_to_budget(frames, 1000, 2);
    let ids: Vec<&str> = kept.iter().map(|k| k.meta.public_id.as_str()).collect();
    assert_eq!(ids, vec!["normal", "stronger"], "kept: {ids:?}");
}

/// A deferred item never outranks a *required* one. The required pass runs
/// before either ranked band, so the precedence is required → normal →
/// deferred, and adding the tier did not open a path around the ADR 0006
/// guarantee.
#[test]
fn a_deferred_item_cannot_displace_a_required_one() {
    let frames = vec![deferred("process_note", 1), required("anchored", 1)];
    let (kept, dropped) = pack_to_budget(frames, 1000, 0);
    let ids: Vec<&str> = kept.iter().map(|k| k.meta.public_id.as_str()).collect();
    assert_eq!(ids, vec!["anchored"], "kept: {ids:?}");
    assert_eq!(dropped[0].id, "process_note");
}

// ── Required items survive ranking pressure (#713 deliverable 5) ────────────

#[test]
fn a_required_item_survives_ranking_pressure_that_would_have_evicted_it() {
    // The gate criterion. `max_frames` is 2 and the anchored item ranks LAST,
    // so under the old strictly-rank-ordered packer it was dropped for
    // FrameCount — silently answering a different question than the one the
    // user asked, since an anchor is a file they named verbatim.
    let frames = vec![
        candidate("ranked_a", 1),
        candidate("ranked_b", 1),
        candidate("ranked_c", 1),
        required("anchored", 1),
    ];
    let (kept, dropped) = pack_to_budget(frames, 1000, 2);
    let ids: Vec<&str> = kept.iter().map(|k| k.meta.public_id.as_str()).collect();
    assert!(ids.contains(&"anchored"), "kept: {ids:?}");
    // …and the required item does not silently steal a ranked slot either: the
    // count budget is a floor for what the caller asked for, so asking for two
    // frames while anchoring one yields the anchor plus two ranked.
    assert_eq!(ids, vec!["ranked_a", "ranked_b", "anchored"]);
    // Rank order is preserved — a packer that emitted required-first would
    // reorder the rendered block whenever an anchor appeared, which is a
    // cache-prefix change (spec §5.1) bought for nothing.
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].id, "ranked_c");
    assert_eq!(dropped[0].reason, DropReason::FrameCount);
}

#[test]
fn a_required_item_takes_its_budget_before_ranked_candidates_compete_for_it() {
    // Token pressure rather than count pressure: the ranked candidates alone
    // would consume the whole budget, and the anchor ranks last.
    let frames = vec![
        candidate("ranked_a", 60),
        candidate("ranked_b", 60),
        required("anchored", 40),
    ];
    let (kept, dropped) = pack_to_budget(frames, 100, 10);
    let ids: Vec<&str> = kept.iter().map(|k| k.meta.public_id.as_str()).collect();
    assert_eq!(ids, vec!["ranked_a", "anchored"]);
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].id, "ranked_b");
    assert_eq!(dropped[0].reason, DropReason::TokenBudget);
}

#[test]
fn a_required_item_that_cannot_fit_at_all_is_dropped_by_an_explicit_named_decision() {
    // The one way a required item may be dropped: it exceeds the entire
    // budget, so no ordering could have admitted it. Reported under its own
    // reason, not folded into TokenBudget — collapsing them would hide the
    // only case where the caller's own instruction was overruled, behind a
    // reason that reads as "the budget filled up before we got here".
    let frames = vec![required("enormous", 500), candidate("ranked", 10)];
    let (kept, dropped) = pack_to_budget(frames, 100, 10);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].meta.public_id, "ranked");
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].id, "enormous");
    assert_eq!(dropped[0].reason, DropReason::RequiredOverBudget);
    assert_ne!(
        dropped[0].reason,
        DropReason::TokenBudget,
        "an overruled instruction must not read as ordinary budget pressure"
    );
}

#[test]
fn a_required_item_crowded_out_by_an_earlier_one_reports_budget_pressure() {
    // Two anchors that each fit alone but not together. The second one lost
    // to budget pressure — a larger `max_tokens` admits it — so it must NOT
    // read as `RequiredOverBudget`, whose claim is "no ordering could have
    // admitted it". That label was reachable here because pass 1 accumulates
    // `spent` across required items and used to derive the reason from the
    // remainder rather than from the item's own cost.
    let frames = vec![required("first_anchor", 60), required("second_anchor", 60)];
    let (kept, dropped) = pack_to_budget(frames, 100, 10);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].meta.public_id, "first_anchor");
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].id, "second_anchor");
    assert_eq!(
        dropped[0].reason,
        DropReason::TokenBudget,
        "a drop a larger budget would fix must not claim it could never fit"
    );
}

#[test]
fn the_selection_reason_crosses_the_cgp_seam_on_the_frames_provenance() {
    // The RecallResult → ContextQueryResult conversion keeps frames and a
    // collapsed `truncated`/`dropped_estimate` and nothing else, so a reason
    // recorded anywhere but on the frame is a reason a CGP consumer never
    // sees. Provenance is the channel; this pins that it is actually written.
    let node = node_row(1, "some content");
    let frame = frame_from_node(
        &node,
        0.5,
        "fp",
        false,
        &[],
        SelectionReason::Anchored,
        RecallTier::Normal,
    )
    .expect("frame");
    let reason = frame
        .provenance
        .iter()
        .find(|p| p.kind == SELECTION_PROVENANCE_KIND)
        .and_then(|p| p.method.clone());
    assert_eq!(reason.as_deref(), Some("anchored"));

    let frame = frame_from_node(
        &node,
        0.5,
        "fp",
        false,
        &[],
        SelectionReason::Ranked,
        RecallTier::Normal,
    )
    .expect("frame");
    let reason = frame
        .provenance
        .iter()
        .find(|p| p.kind == SELECTION_PROVENANCE_KIND)
        .and_then(|p| p.method.clone());
    assert_eq!(reason.as_deref(), Some("ranked"));
}

#[test]
fn only_an_anchor_is_required_today() {
    // The category-aware precedence ADR 0006 asks for, stated as a test so
    // widening it is a deliberate edit rather than a drift. An anchor is the
    // one signal in the ranking that came from the user rather than from a
    // heuristic; nothing else has earned the exemption yet.
    assert!(SelectionReason::Anchored.is_required());
    assert!(!SelectionReason::Ranked.is_required());
    assert!(!SelectionReason::LexicalFallback.is_required());
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
        recorded_at: "2026-01-01T00:00:00Z".into(),
        recall_tier: RecallTier::Normal,
    };
    let err = frame_from_node(
        &node,
        0.5,
        "fp",
        false,
        &[],
        SelectionReason::Ranked,
        RecallTier::Normal,
    )
    .unwrap_err();
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
        recorded_at: "2026-01-01T00:00:00Z".into(),
        recall_tier: RecallTier::Normal,
    };
    let frame = frame_from_node(
        &node,
        0.5,
        "fp",
        true,
        &["billing".to_string()],
        SelectionReason::Ranked,
        RecallTier::Normal,
    )
    .unwrap();
    assert!(
        is_lexical_fallback(&frame),
        "fallback frames must be labeled"
    );
    let graph_frame = frame_from_node(
        &node,
        0.5,
        "fp",
        false,
        &[],
        SelectionReason::Ranked,
        RecallTier::Normal,
    )
    .unwrap();
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
