// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The pass that fills the semantic index — and, since #4043, the only one
//! that does.
//!
//! # Why this exists at all
//!
//! Embedding used to happen in three places: eagerly in `stella init`, in a
//! bounded top-up at session start, and — the expensive one — lazily inside
//! [`super::engine::dispatch`], on the query path, where a search paid for
//! whatever the index was missing before it was allowed to answer. That last
//! one is what #4035 measured at 46.9 seconds a call, and what #4041 cut to
//! roughly a sixth by batching. A sixth of a latency that large is still on a
//! latency-sensitive read, and it still could not converge: the per-query pass
//! was capped, so a workspace further behind than the cap paid *something*
//! forever and never caught up.
//!
//! #4043 decided the trade the two issues left open, in favour of the option
//! the maintainer named: **the backfill moves off the query path entirely.**
//! One pass, in the background, at session start, running to exhaustion rather
//! than to a cap. A search then ranks over whatever the index holds and
//! reports its coverage, which it already did.
//!
//! # What is given up, stated plainly
//!
//! Self-healing by query is gone. Before this, a workspace whose index was
//! behind repaired itself a little on every search, so a user who never ran
//! `stella init` and never started an interactive session still converged
//! eventually. Now nothing converges unless a session start (or `stella init`)
//! runs this pass — a one-shot `stella search` in a cold checkout ranks over
//! an empty index and says so, where it used to embed 200 files first.
//!
//! That is the intended trade, and two things pay for it. The pass here is
//! unbounded where the lazy one was capped, so the first session in a
//! workspace finishes the job the lazy path could only nibble at. And
//! [`super::readiness`] holds the first prompt while it runs, so "the index is
//! not ready yet" is a sentence the user reads rather than a silently thin
//! answer they act on.
//!
//! # Ordering
//!
//! Whole-file vectors first, then chunks. Both rungs are merged into one
//! ranking ([`super::engine::semantic_hits`]), but file vectors give coarse
//! coverage of the *whole* tree for the cost of one row a file, so a pass
//! interrupted halfway leaves an index that can answer roughly about
//! everything rather than precisely about a tenth of it.

use std::path::Path;

use stella_embed::Embedder;
use stella_graph::CodeGraph;

use super::engine::{ChunkWarmOutcome, warm_chunks_opened};
use super::readiness::{IndexReadiness, measure};
use super::semantic::{WarmOutcome, warm_opened};

/// What one backfill pass did.
///
/// Total by construction and never a `Result`, for the reason
/// [`WarmOutcome`] gives: a session start must survive an absent embedder, a
/// broken one, and a workspace too big to finish, and the difference between
/// those is something a human reads rather than something a caller branches
/// on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackfillOutcome {
    /// The pass held the lease and ran. Each half reports itself.
    Ran {
        files: WarmOutcome,
        chunks: ChunkWarmOutcome,
    },
    /// Another pass — this process's or another `stella`'s — holds the embed
    /// lease. It is doing this work; this one steps aside rather than paying
    /// for it twice (#3650).
    Busy,
    /// The code graph could not be opened, so there was nothing to fill.
    Unavailable(String),
}

/// Fill the workspace's semantic index to exhaustion, reporting readiness as
/// it goes.
///
/// `progress` receives a fresh [`IndexReadiness`] after every committed batch,
/// always with `settled: false` — this function is the thing that is not
/// settled yet. The caller marks the end, because only the caller knows
/// whether it is going to call this again.
///
/// Bounded by the workspace rather than by a cap: each pass is limited to the
/// number of files in the index, which is the exact bound (a file is embedded
/// at most once per pass) and the honest one. There is no latency budget to
/// protect here — nothing is waiting on this call — so the reason the lazy
/// path was capped does not apply.
pub async fn backfill_workspace_vectors<P: FnMut(IndexReadiness) + ?Sized>(
    root: &Path,
    embedder: &dyn Embedder,
    progress: &mut P,
) -> BackfillOutcome {
    // Deliberately NOT `codegraph::open_or_build`: the caller has just run the
    // index walk, and a second catch-up pass would re-walk and re-hash a tree
    // nothing has touched since — the same reasoning [`super::semantic`]'s
    // eager pass gives for opening the store directly.
    let graph = match stella_store::workspace_private_sqlite_path(root, "codegraph.db")
        .map_err(|error| format!("cannot prepare the code graph store: {error}"))
        .and_then(|db_path| {
            CodeGraph::open(root, &db_path)
                .map_err(|error| format!("could not open the code graph: {error}"))
        }) {
        Ok(graph) => graph,
        Err(reason) => return BackfillOutcome::Unavailable(reason),
    };
    let outcome = backfill_opened(&graph, embedder, progress).await;
    graph.shutdown();
    outcome
}

/// [`backfill_workspace_vectors`] against a graph the caller already holds —
/// the seam a test drives, and the one place the lease discipline lives.
///
/// `progress` is generic rather than `&mut dyn FnMut(_)` so each caller's
/// `Send`-ness is inferred instead of erased: this runs inside a
/// `tokio::spawn`ed session task and needs the whole future to be `Send`,
/// while `stella init` narrates through a callback borrowing a non-`Send`
/// emitter. A trait object forces one answer on both, and it is the wrong one
/// for whichever caller did not pick it.
pub async fn backfill_opened<P: FnMut(IndexReadiness) + ?Sized>(
    graph: &CodeGraph,
    embedder: &dyn Embedder,
    progress: &mut P,
) -> BackfillOutcome {
    let fingerprint = embedder.fingerprint().id();
    // Single-flight across processes as well as within one: two `stella`
    // sessions opened on the same workspace would otherwise both embed the
    // whole tree, and the user would pay the bill twice for one index.
    let Some(lease) = graph.acquire_lease(stella_graph::lease::Purpose::Embed) else {
        return BackfillOutcome::Busy;
    };

    // Reported before a single vector is written, so a surface gating on
    // readiness learns the workspace is behind at the start of the pass
    // rather than one round trip into it — on a cold checkout that first
    // batch is exactly when the user is typing.
    progress(measure(graph, &fingerprint, false));

    // One file, one row: the file count is exactly how many files a pass can
    // embed, so this cap can only ever be reached by finishing.
    let limit = graph.file_count().unwrap_or(0);
    let files = warm_opened(graph, embedder, limit, &mut |_| {
        progress(measure(graph, &fingerprint, false));
    })
    .await;
    let chunks = warm_chunks_opened(graph, embedder, limit, &mut |_| {
        progress(measure(graph, &fingerprint, false));
    })
    .await;

    // Released on every path: a pass that failed holds no claim on the next
    // one, and leaving the lease behind would stall embedding for the whole
    // TTL over an error the caller already has in hand.
    graph.release_lease(&lease);
    BackfillOutcome::Ran { files, chunks }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use stella_embed::{EmbedError, Embedder, EmbedderFingerprint, Embedding, SimilarityPosture};

    use super::*;

    /// A backend that answers deterministically and counts its round trips —
    /// the instrument both witnesses here read.
    #[derive(Debug, Default)]
    struct CountingEmbedder {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Embedder for CountingEmbedder {
        fn fingerprint(&self) -> EmbedderFingerprint {
            EmbedderFingerprint {
                model_id: "counting".into(),
                revision: "1".into(),
                dims: 2,
                normalization: "l2".into(),
            }
        }

        async fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, EmbedError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let fingerprint = self.fingerprint().id();
            Ok(texts
                .iter()
                .map(|text| {
                    let mut vector = vec![text.len() as f32, 1.0];
                    stella_embed::l2_normalize(&mut vector);
                    Embedding {
                        fingerprint: fingerprint.clone(),
                        vector,
                    }
                })
                .collect())
        }

        fn similarity_posture(&self) -> SimilarityPosture {
            SimilarityPosture::Semantic {
                admission_floor: 0.2,
            }
        }
    }

    fn indexed_fixture(root: &Path, files: usize) -> CodeGraph {
        for index in 0..files {
            std::fs::write(
                root.join(format!("file_{index}.rs")),
                format!("pub fn thing_{index}() -> usize {{ {index} }}\n"),
            )
            .expect("write a fixture file");
        }
        let graph = CodeGraph::open(root, &root.join("codegraph.db")).expect("open the graph");
        graph.index_all().expect("index the fixture");
        graph
    }

    /// **The witness for the pass itself.** One backfill leaves nothing
    /// pending — the property the lazy per-query pass could not have, because
    /// it was capped and only ever ran a window at a time.
    #[tokio::test]
    async fn one_pass_leaves_nothing_pending() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let root = workspace.path().canonicalize().expect("canonicalize");
        let graph = indexed_fixture(&root, 6);
        let embedder = CountingEmbedder::default();
        let fingerprint = embedder.fingerprint().id();

        let before = measure(&graph, &fingerprint, false);
        assert!(
            before.unindexed_files > 0,
            "the fixture must start behind or this proves nothing: {before:?}"
        );

        let outcome = backfill_opened(&graph, &embedder, &mut |_| {}).await;
        assert!(
            matches!(outcome, BackfillOutcome::Ran { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            measure(&graph, &fingerprint, true).unindexed_files,
            0,
            "the pass must run to exhaustion, not to a cap"
        );
        graph.shutdown();
    }

    /// The lease is what stops two sessions on one workspace embedding the
    /// same tree twice, on the user's bill.
    #[tokio::test]
    async fn a_second_pass_steps_aside_while_the_lease_is_held() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let root = workspace.path().canonicalize().expect("canonicalize");
        let graph = indexed_fixture(&root, 2);
        let held = graph
            .acquire_lease(stella_graph::lease::Purpose::Embed)
            .expect("the first caller takes the lease");

        let outcome = backfill_opened(&graph, &CountingEmbedder::default(), &mut |_| {}).await;
        assert_eq!(outcome, BackfillOutcome::Busy);

        graph.release_lease(&held);
        graph.shutdown();
    }

    /// Progress is reported while the pass runs, always unsettled: a surface
    /// that renders these must be able to tell "still filling" from "done",
    /// and only the caller knows which of those the end of the pass is.
    #[tokio::test]
    async fn progress_is_reported_unsettled_as_the_pass_runs() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let root = workspace.path().canonicalize().expect("canonicalize");
        let graph = indexed_fixture(&root, 4);

        let mut ticks: Vec<IndexReadiness> = Vec::new();
        backfill_opened(&graph, &CountingEmbedder::default(), &mut |readiness| {
            ticks.push(readiness);
        })
        .await;

        assert!(!ticks.is_empty(), "a pass that embedded files must report");
        assert!(
            ticks.iter().all(|tick| !tick.settled),
            "the pass never declares itself settled: {ticks:?}"
        );
        graph.shutdown();
    }
}
