// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What one `search` call costs, in embedder round-trips (#4035).
//!
//! A sibling of the ladder tests rather than part of them: `tests.rs` sits
//! against the 1500-line ceiling, and latency is its own subject — not whether
//! the ladder finds the right file, but what it spends finding it.

use super::*;

/// **The #4035 measurement, and the budget it became.** Where one `search`
/// call's wall clock goes, split by phase, counted rather than reasoned about.
///
/// `search` averaged **46.9 seconds** per call across 52 recorded calls in one
/// workspace (max 155.5s; 38 of 52 over ten seconds), and the issue's first
/// definition-of-done was to establish where that time goes before anything
/// was changed. This answers it without a network or an API key: a counting
/// embedder records every round-trip the ladder makes, so the split is a
/// measured ratio rather than a reading of the code.
///
/// **What it found.** The query embedding — the only round-trip a search
/// actually needs — is one call. Every other one is a lazy index backfill
/// running inside the tool call, and `catch_up_chunk_embeddings` issued **one
/// sequential round-trip per pending file**, up to
/// `MAX_FILES_PER_CHUNK_PASS` (64) of them, each carrying two texts against a
/// batch size of 32. One search over a behind index cost **72 round-trips, of
/// which 1 was the query** — and it recurred on every call for as long as the
/// index stayed behind, which is also why the answer kept saying PARTIAL
/// INDEX. Batching the embedding across files (storing still per-file, which
/// is the constraint that actually exists) took the same search to **12**.
///
/// **The budget this now holds.** Round-trips, not seconds: seconds are
/// machine- and provider-dependent and would make this flaky, while sequential
/// round-trips are both deterministic and the actual cause of the latency. The
/// target is that backfill cost scales with the *volume of text* — one
/// round-trip per [`EMBED_BATCH`] — never with the *number of files*. Regress
/// to a per-file loop on the query path and the mean batch size collapses,
/// which is what fails here.
#[tokio::test]
async fn a_search_keeps_its_embedder_round_trips_within_budget() {
    /// Records every `embed` call's batch size, in order.
    #[derive(Debug, Default)]
    struct CountingEmbedder {
        batches: std::sync::Mutex<Vec<usize>>,
    }

    #[async_trait]
    impl Embedder for CountingEmbedder {
        fn fingerprint(&self) -> EmbedderFingerprint {
            EmbedderFingerprint {
                model_id: "counting".into(),
                revision: "1".into(),
                dims: DIMS,
                normalization: "l2".into(),
            }
        }

        async fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, EmbedError> {
            if texts.is_empty() {
                return Err(EmbedError::EmptyInput);
            }
            self.batches.lock().expect("not poisoned").push(texts.len());
            let fingerprint = self.fingerprint().id();
            Ok(texts
                .iter()
                .map(|text| Embedding {
                    fingerprint: fingerprint.clone(),
                    vector: ConceptEmbedder::project(text),
                })
                .collect())
        }

        fn similarity_posture(&self) -> SimilarityPosture {
            SimilarityPosture::Semantic {
                admission_floor: 0.2,
            }
        }
    }

    // A workspace bigger than both per-pass caps, so one call cannot finish
    // the backfill — the state the measured workspace was in (447 files still
    // carrying symbols with no vector).
    let workspace = tempfile::tempdir().expect("tempdir");
    for n in 0..300 {
        let file = workspace.path().join(format!("src/mod{n}.rs"));
        fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
        fs::write(
            &file,
            format!(
                "//! Module {n}.\n\
                 pub fn alpha_{n}() -> usize {{ {n} }}\n\
                 pub fn beta_{n}() -> usize {{ {n} + 1 }}\n"
            ),
        )
        .expect("write");
    }
    let root = workspace.path().canonicalize().expect("canonicalize");
    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");

    let embedder = CountingEmbedder::default();
    let _ = dispatch(Some(&graph), &root, "matrix arithmetic", Some(&embedder)).await;

    let batches = embedder.batches.lock().expect("not poisoned").clone();
    let round_trips = batches.len();
    let texts: usize = batches.iter().sum();
    eprintln!(
        "#4035 budget: {round_trips} embedder round-trips for ONE search, \
         {texts} texts (batch sizes: {batches:?})"
    );

    // Exactly one single-text round-trip: the query. Everything else is
    // backfill the caller did not ask for and cannot see.
    assert_eq!(
        batches.iter().filter(|size| **size == 1).count(),
        1,
        "exactly one single-text round-trip — the query itself: {batches:?}"
    );

    // The budget. Backfill scales with text volume, not with file count: the
    // floor is `texts / EMBED_BATCH`, and each of the two catch-up phases may
    // leave one short final batch. Anything beyond that is a per-file loop
    // back on the query path, which is what cost 46.9 seconds a call.
    let ceiling = texts.div_ceil(crate::search::semantic::EMBED_BATCH) + 2 + 1;
    assert!(
        round_trips <= ceiling,
        "one search may cost at most {ceiling} embedder round-trips for {texts} \
         texts, got {round_trips} — the backfill is batching per file again \
         rather than per {} texts: {batches:?}",
        crate::search::semantic::EMBED_BATCH
    );
}
