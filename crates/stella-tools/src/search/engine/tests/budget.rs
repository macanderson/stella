// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What one search costs the embedding backend (#4035, #4041, #4043).
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
