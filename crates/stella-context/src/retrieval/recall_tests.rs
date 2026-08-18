//! End-to-end recall over a real store — the public-API integration half of
//! the retrieval tests, driving [`ContextStore::recall`] against a seeded
//! SQLite workspace rather than the pure ranking units in
//! [`super::tests`](super::tests).
//!
//! Split out of `retrieval/tests.rs` before that file crossed the gate's
//! 1500-line ceiling (#3705); it is a sibling test module like
//! [`super::evidence_tests`](super::evidence_tests), not a submodule of the
//! unit tests, because it shares nothing with them but `super::*`.

use super::*;

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
/// fold them all — which is what it used to do. This pins the other half,
/// under the gate's declared budget: a gated 5-frame recall may read the
/// corpus **once** (the evidence term pass, #2289) plus the candidates'
/// bodies, and nothing more. What it must never regress to is the old
/// failure — bodies materialized per candidate and thrown away, a *multiple*
/// of the corpus rather than one pass of it.
///
/// (Renamed from `recall_reads_only_the_candidates_bodies_not_the_whole_corpus`
/// when the evidence gate landed: the old name claimed a zero-corpus-read
/// property the gated default deliberately no longer has.)
#[tokio::test]
async fn recall_reads_one_term_pass_plus_only_the_candidates_bodies() {
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
        // Two topics, not one: a single-topic corpus saturates every query
        // term (df = N) and the evidence gate abstains, which would let this
        // bound pass vacuously with zero candidate bodies read. Half on-topic
        // keeps the query's terms distinctive (df = N/2) so a full shortlist
        // is admitted and genuinely measured.
        let content = if i % 2 == 0 {
            format!("note number {i} about packing frames to a budget")
        } else {
            format!("note number {i} rendering the quarterly revenue chart")
        };
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
    // the *ranking* is entitled to read — budgeted against the LONGEST body
    // rather than the mean: which 20 nodes win is a ranking outcome, and a
    // mean-based bound would flake whenever the winners ran slightly above
    // average. On top of that sits exactly ONE streaming pass over the corpus:
    // the evidence gate's term scan (#2289), which reads each body once,
    // retains only per-term counts, and is the declared price of "a frame is
    // recalled iff something ties it to this query". What this bound still
    // removes is the old failure — bodies materialized per candidate and then
    // thrown away, which cost a *multiple* of the corpus, not one pass of it.
    let candidates = 5 * DEFAULT_MMR_CANDIDATE_MULTIPLE;
    let budgeted = corpus_bytes + candidates * longest_body;
    assert!(
        loaded <= budgeted,
        "recall read {loaded} content bytes of a {corpus_bytes}-byte corpus for \
         5 frames; one evidence-gate term pass plus the candidate bound \
         entitles it to about {budgeted} ({candidates} of {NODES} nodes' \
         bodies on top of the single streaming scan)."
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
    // Gate off: this fixture's one-topic corpus saturates every query term
    // (df = N), so the evidence gate would rightly abstain and leave no
    // shortlist to measure. The denominator arithmetic being pinned here is
    // independent of admission; the gate has its own witnesses in
    // `evidence_tests`.
    let store = ContextStore::open_with(
        &path,
        Arc::new(HashEmbedder::default()),
        FixedClock::shared(1_000),
    )
    .unwrap()
    .with_tuning(RecallTuning {
        require_evidence: false,
        ..RecallTuning::default()
    });

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
    // The query names both the sqlite node's and the budgeting node's terms,
    // so two candidates carry evidence and the one-frame budget genuinely has
    // something to drop — under the evidence gate a drop report needs a
    // *relevant* loser, not just any second row.
    let mut q = base_query(
        "open the database",
        "open the sqlite connection in wal mode and pack the context frames to the token budget",
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
        .upsert(
            ContextDelta::new()
                .with_node(
                    NodeInput::new(NodeKind::Concept, "early")
                        .with_content("the flux capacitor note"),
                )
                // Off-topic ballast: with only the two capacitor nodes every
                // query term saturates (df = N) and the evidence gate rightly
                // abstains. Two unrelated nodes keep "flux capacitor"
                // distinctive at both instants this test queries.
                .with_node(
                    NodeInput::new(NodeKind::Concept, "chart")
                        .with_content("render a bar chart of quarterly revenue"),
                )
                .with_node(
                    NodeInput::new(NodeKind::Concept, "budget")
                        .with_content("pack context frames and report drops"),
                ),
        )
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
    // Two nodes' terms, so that when the winner is suppressed a *relevant*
    // next candidate exists to take the slot — the evidence gate refuses an
    // irrelevant stand-in, and rightly so.
    let mut q = base_query(
        "open the database",
        "open the sqlite connection in wal mode and pack the context frames to the token budget",
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
    // Same two-node query as the suppression test above, for the same reason:
    // the freed slot must have a relevant candidate to go to.
    let mut q = base_query(
        "open the database",
        "open the sqlite connection in wal mode and pack the context frames to the token budget",
    );
    q.max_frames = 1;

    let before = store.recall(&q).await.unwrap();
    let winner = before.frames[0].id.clone();

    let excluded: std::collections::HashSet<String> = [winner.clone()].into_iter().collect();
    let after = store
        .recall_scoped_excluding(&q, &RecallScope::default(), &excluded)
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
    // Gate off throughout: this fixture's one-topic corpus saturates every
    // query term (df = N), and an abstaining recall has no shortlist widths
    // or fallback arms left to compare. Each knob under test is set on top.
    let make = |tuning: RecallTuning| {
        let tuning = RecallTuning {
            require_evidence: false,
            ..tuning
        };
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

/// The benchmark #712's gate names: **ungated** recall work bounded by the
/// requested frame count, at three corpus sizes spanning two orders of
/// magnitude.
///
/// "Ungated" is in the name because it is the honest scope: under the default
/// evidence gate (#2289) every recall also pays one streaming term pass, so
/// the shipped default's bytes are corpus-linear by declared design — that
/// cost is budgeted by `recall_reads_one_term_pass_plus_only_the_candidates_bodies`
/// and tracked for sublinear replacement in #2297, at which point this
/// benchmark should run gate-on again and the "not corpus size" claim return
/// to the default path. What survives here meanwhile is the two-phase
/// candidate load: with the gate off, content bytes must come out
/// *byte-identical* across a 100x corpus, and the cosine scan — the one
/// honest exception, since "most similar" is not something SQLite can
/// `ORDER BY` without an ANN index — must stay linear rather than quadratic.
///
/// It measures work, not wall clock. A wall-clock assertion cannot tell
/// "bounded" from "fast on this machine today", and is the kind of test that
/// gets marked flaky and deleted.
///
/// (Renamed from `recall_work_is_bounded_by_frame_count_not_corpus_size`
/// when the evidence gate landed: the unqualified name overclaimed for the
/// shipped default.)
#[tokio::test]
async fn ungated_recall_work_is_bounded_by_frame_count_not_corpus_size() {
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
        .unwrap()
        .with_tuning(RecallTuning {
            require_evidence: false,
            ..RecallTuning::default()
        });

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

/// The load-bearing integration test for memory anchors.
///
/// #775 gave the store the ability to say "this stopped being true" without
/// saying "we were wrong". That was inert on its own: retrieval read belief
/// time only, so an anchor to a deleted file kept feeding graph adjacency
/// forever. This asserts the two halves are actually connected — end an
/// anchor's world validity, and live recall stops traversing it.
///
/// The memory is written to be unreachable by every *other* signal: its text
/// shares no vocabulary with the query, and later memories outrank it on
/// recency. The graph edge is the only thing that can put it in the result, so
/// its disappearance is attributable.
#[tokio::test]
async fn ending_an_anchor_stops_live_recall_traversing_it() {
    let clock = FixedClock::shared(1_000);
    let dir = TempDir::new().unwrap();
    let store = ContextStore::open_with(
        dir.path().join("context.db"),
        Arc::new(HashEmbedder::default()),
        clock.clone(),
    )
    .unwrap();

    // The anchored memory, plus the file it is about.
    store
        .upsert(
            crate::writeback::ContextDelta::new().with_memory(
                crate::writeback::MemoryInput::reflection(
                    "zzz quokka marmalade tessellation",
                    Vec::<String>::new(),
                )
                .with_anchors(["src/registry.rs"]),
            ),
        )
        .await
        .unwrap();

    // A corpus big enough that the frame budget genuinely binds. Without this
    // the anchored memory is one of a handful and recency alone seats it, so
    // its presence would prove nothing about the edge.
    for i in 0..40 {
        clock.advance(100);
        store
            .upsert(crate::writeback::ContextDelta::new().with_memory(
                crate::writeback::MemoryInput::reflection(
                    format!("unrelated later note number {i} about deployment rollout"),
                    Vec::<String>::new(),
                ),
            ))
            .await
            .unwrap();
    }

    let mut q = base_query("what do we know here", "deployment note");
    // Seed the graph expansion at the anchored file. The uri spelling must
    // match what the write side minted.
    q.anchors = vec!["file://src/registry.rs".into()];
    q.max_frames = 3;

    let before = store.recall(&q).await.unwrap();
    let seen_before = before
        .frames
        .iter()
        .any(|f| f.content.as_deref().unwrap_or_default().contains("quokka"));
    assert!(
        seen_before,
        "the anchor should pull the memory in: {:?}",
        before
            .frames
            .iter()
            .map(|f| f.content.clone())
            .collect::<Vec<_>>()
    );

    // The file is deleted. End world validity — belief untouched.
    let anchor = store.open_anchors().unwrap().remove(0);
    clock.advance(1_000);
    let deleted_at = store.clock().now_rfc3339();
    assert!(
        store
            .end_anchor_validity(anchor.edge_id, &deleted_at)
            .unwrap()
    );

    let after = store.recall(&q).await.unwrap();
    let seen_after = after
        .frames
        .iter()
        .any(|f| f.content.as_deref().unwrap_or_default().contains("quokka"));
    assert!(
        !seen_after,
        "an anchor to a deleted file must stop feeding recall: {:?}",
        after
            .frames
            .iter()
            .map(|f| f.content.clone())
            .collect::<Vec<_>>()
    );

    // And the belief is still there to be audited — this is the whole reason
    // it is `end_world_validity` and not `close_edge`.
    assert!(
        store
            .facts_as_of(None)
            .unwrap()
            .iter()
            .any(|f| f.predicate == crate::writeback::ANCHOR_REL),
        "the anchor is still BELIEVED; only the present stops seeing it"
    );
}
