// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What one search costs — the embedding backend (#4035, #4041, #4043) and
//! the caller's context window (#3140).
//!
//! A sibling of the ladder tests rather than part of them: `tests.rs` sits
//! against the 1500-line ceiling, and this is its own subject — not what a
//! search *answers*, but what it *spends* on the way there.
//!
//! The number is one, and one is the whole contract: the query. Everything
//! else a search used to buy — 71 round trips on the workspace #4035
//! measured, 12 after #4041 batched them — was write-side index maintenance
//! performed inside a latency-sensitive read, and #4043 moved it to
//! [`crate::search::backfill`]. Counted here with a fake embedder and no
//! network, so the assertion is exact rather than a timing.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// A backend that answers deterministically and counts every call.
#[derive(Debug, Default)]
struct CountingEmbedder {
    calls: AtomicUsize,
}

impl CountingEmbedder {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Embedder for CountingEmbedder {
    fn fingerprint(&self) -> EmbedderFingerprint {
        ConceptEmbedder.fingerprint()
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, EmbedError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ConceptEmbedder.embed(texts).await
    }

    fn similarity_posture(&self) -> SimilarityPosture {
        ConceptEmbedder.similarity_posture()
    }
}

/// **The witness for #4043.** A search over an index that is *entirely*
/// behind — nothing embedded at all, the worst case the old lazy pass paid
/// most for — makes exactly one call to the embedder: the query's.
///
/// Fails before this change: `semantic_hits` ran `catch_up_embeddings` and
/// `catch_up_chunk_embeddings` first, so this same fixture cost several calls
/// and a real workspace cost dozens.
#[tokio::test]
async fn one_search_over_a_behind_index_costs_one_round_trip() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_fixture(workspace.path());
    let embedder = CountingEmbedder::default();

    // The premise: the index really is behind. Without this the test would
    // pass on a warm index and prove nothing.
    let pending = crate::search::readiness::measure(&graph, &embedder.fingerprint().id(), false);
    assert!(
        pending.unindexed_files > 0,
        "the fixture must start unembedded: {pending:?}"
    );

    let _ = dispatch(Some(&graph), &root, QUESTION, Some(&embedder)).await;
    assert_eq!(
        embedder.calls(),
        1,
        "a search may embed the query and nothing else — filling the index is the background \
         pass's job (#4043)"
    );

    // And it really did not fill it on the way past: a search that quietly
    // embedded would satisfy the count above only by batching, which is what
    // #4041 did and what #4043 decided was still the wrong path.
    let after = crate::search::readiness::measure(&graph, &embedder.fingerprint().id(), false);
    assert_eq!(
        after.unindexed_files, pending.unindexed_files,
        "a search must leave the index exactly as it found it"
    );
    graph.shutdown();
}

/// The same count on a *fully warm* index, so the contract is one round trip
/// per search rather than one per search-that-had-nothing-to-do.
#[tokio::test]
async fn one_search_over_a_warm_index_costs_one_round_trip_too() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = embedded_fixture(workspace.path(), &ConceptEmbedder).await;
    let embedder = CountingEmbedder::default();

    let answer = dispatch(Some(&graph), &root, QUESTION, Some(&embedder)).await;
    assert_eq!(embedder.calls(), 1);
    assert_eq!(
        answer.hits.first().map(|hit| hit.path.as_str()),
        Some(ANSWER),
        "the warm index must still answer, or the count above is measuring a broken search"
    );
    graph.shutdown();
}

/// Two files whose bodies are long enough that a rendered `Facet::Body` costs
/// several times what `Facet::estimated_cost` prices it at — the exact
/// divergence #3140 is about, reproduced rather than simulated.
fn long_bodied_fixture(workspace: &Path) -> (std::path::PathBuf, CodeGraph) {
    let filler = "x".repeat(160);
    for name in ["alpha", "beta"] {
        let body: String = (0..60)
            .map(|line| format!("    let value_{line} = {line}; // {filler}\n"))
            .collect();
        fs::write(
            workspace.join(format!("{name}.rs")),
            format!("pub fn {name}() {{\n{body}}}\n"),
        )
        .expect("write");
    }
    let root = workspace.canonicalize().expect("canonicalize");
    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");
    (root, graph)
}

/// **The witness for #3140.** An answer the allocator says fits — every hit
/// granted, nothing omitted, the estimate inside the budget — renders longer
/// than the budget allows, and the renderer must notice that from the
/// measured text rather than trust the estimate.
///
/// Fails before this change: `render` appended every granted block
/// unconditionally and reported `allocation.spent` (the estimate) in the
/// truncation line, so this fixture rendered both bodies — about twice the
/// budget — and said nothing at all, because the allocator had omitted
/// nothing.
#[test]
fn a_render_that_outgrows_its_estimate_stops_at_the_budget() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = long_bodied_fixture(workspace.path());

    let hits: Vec<Hit> = ["alpha", "beta"]
        .into_iter()
        .map(|name| Hit {
            path: format!("{name}.rs"),
            why: "matched 1 query term(s)".into(),
            focus: Some(name.into()),
        })
        .collect();
    let answer = Answer {
        hits: hits.clone(),
        strategies: vec![Strategy::FileScan],
        note: None,
    };
    let config = SearchConfig {
        depth: Depth::MAX,
        budget: 8_000,
    };

    // The premise, checked rather than assumed: the estimate says the whole
    // answer fits. Without this the test would pass on an answer the
    // allocator had already truncated and prove nothing about measurement.
    let allocation = allocate(hits.len(), config.depth, config.budget);
    assert_eq!(
        allocation.granted.len(),
        hits.len(),
        "the estimate must grant every hit, or the fixture is not the #3140 shape"
    );
    assert_eq!(allocation.omitted, 0);
    assert!(allocation.spent <= config.budget);

    // ...and the render really is bigger than the estimate priced it.
    let first = super::enrich::render_hit(Some(&graph), &root, &hits[0], Depth::MAX);
    assert!(
        first.len() > allocation.spent,
        "one rendered block ({}) must already outgrow the whole estimate ({}), or this fixture \
         does not reproduce the overshoot",
        first.len(),
        allocation.spent
    );

    let content = content_of(&render(Some(&graph), &root, "anything", &answer, config));
    graph.shutdown();

    assert!(
        content.contains("TRUNCATED"),
        "an answer cut short by its measured length did not say so: {content}"
    );
    assert!(
        content.len() <= config.budget + first.len(),
        "the rendered answer ({} characters) overran the budget ({}) by more than one hit block \
         ({})",
        content.len(),
        config.budget,
        first.len()
    );
    // The count in the head is the count that survived, not the count the
    // allocator granted: a head claiming two results above one block is the
    // same lie in a different line.
    assert!(
        content.contains("1 result(s)"),
        "the head did not name the hits actually rendered: {content}"
    );
    assert!(
        !content.contains("beta.rs"),
        "the dropped hit was rendered anyway: {content}"
    );
}
