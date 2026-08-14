//! The whole-file embedding passes behind semantic ranking.
//!
//! Deliberately split three ways, because only one third of the work is
//! allowed to do I/O and only one third is allowed to make a decision:
//!
//! - `stella-graph` stores the vectors beside the nodes they describe and
//!   reports what still needs embedding. No transport, no key, no model.
//! - `stella-embed` owns the seam, the fingerprint, the HTTP backend and the
//!   pure ranker.
//! - This module is the orchestration: embed what is pending and commit it.
//!   The eager pass is `stella init`'s; the lazy catch-up is the search
//!   ladder's ([`super::engine`]).

use std::path::Path;

use stella_embed::Embedder;
use stella_graph::FileVector;

/// Files embedded per request to the backend. Every provider accepts a
/// batch; 32 keeps a single request well inside every documented body limit
/// while still amortising the round trip across a warm-up pass.
pub(crate) const EMBED_BATCH: usize = 32;

/// The most files one eager (`stella init`) pass will embed.
///
/// Ten times the lazy per-query cap, and bounded for a different reason. The
/// lazy cap trades coverage for the latency of a query someone is waiting
/// on; this pass runs before any turn starts, so its budget is the user's
/// money and patience rather than a round trip. A repository larger than this
/// gets a **stated** partial index — the emitted line names how many files
/// were left — because a partial index that silently ranks a subset is worse
/// than one that says which subset it ranked.
pub(crate) const MAX_FILES_PER_EAGER_PASS: usize = 2_000;

/// What an eager pass did, as data the caller renders.
///
/// Total by construction and never a `Result`: on this path a failure is a
/// *report*, not an error to propagate — `stella init` must succeed with no
/// embedder, with a broken embedder, and with a repository too big to finish,
/// and the difference between those is something a human reads, not something
/// a caller branches on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WarmOutcome {
    /// The pass ran. `remaining` is what the cap left for the lazy path to
    /// pick up on the first semantic query.
    Warmed {
        /// Files embedded by this pass.
        embedded: usize,
        /// Indexed files still carrying no vector under this fingerprint.
        remaining: usize,
        /// How many of those the pass could not read from disk. Separated from
        /// `remaining` because the two want different sentences: the cap's
        /// leftovers embed themselves on the next query, and an unreadable
        /// file never will (#3016).
        unreadable: usize,
    },
    /// The pass stopped early. `reason` is prose meant to be shown verbatim;
    /// whatever was embedded before the failure is committed and counted.
    Failed {
        /// Files embedded before the failure — batches commit as they go.
        embedded: usize,
        /// Why it stopped, already phrased for a human.
        reason: String,
    },
}

/// Embed the workspace's indexed files **without answering a query** — the
/// `stella init` pass.
///
/// The lazy per-query pass is right for a session and wrong for a
/// single-turn run: it fires only once a search has already been asked, so
/// the first searches of a session would rank against whichever files
/// happened to be processed first. This runs the same render, the same table
/// and the same `(file_id, fingerprint)` keying ahead of the first turn,
/// where the work is free from the session's perspective.
///
/// Batched rather than one big pass: each round asks for at most one
/// request's worth of pending files, so the blocking file reads stay bounded
/// between awaits and a pass killed halfway has committed every batch before
/// it.
#[cfg(test)]
pub(crate) async fn warm_file_vectors(
    root: &Path,
    embedder: &dyn Embedder,
    limit: usize,
) -> WarmOutcome {
    warm_file_vectors_with_progress(root, embedder, limit, &mut |_| {}).await
}

/// The eager whole-file pass, with a progress callback (#3102): `progress`
/// receives the cumulative embedded-file count after each batch commits, so
/// a long pass can be narrated while it happens instead of summarised after.
/// Display-only — the callback cannot affect the pass.
pub(crate) async fn warm_file_vectors_with_progress(
    root: &Path,
    embedder: &dyn Embedder,
    limit: usize,
    progress: &mut dyn FnMut(usize),
) -> WarmOutcome {
    // Deliberately NOT `open_or_build`: the caller has just run `index_all`,
    // and a second catch-up pass would re-walk and re-hash the whole tree for
    // a graph nothing has touched since. This pass embeds what the index
    // already holds and indexes nothing itself.
    let graph = stella_store::workspace_private_sqlite_path(root, "codegraph.db")
        .map_err(|error| format!("cannot prepare the code graph store: {error}"))
        .and_then(|db_path| {
            stella_graph::CodeGraph::open(root, &db_path)
                .map_err(|error| format!("could not open the code graph: {error}"))
        });
    let graph = match graph {
        Ok(graph) => graph,
        Err(reason) => {
            return WarmOutcome::Failed {
                embedded: 0,
                reason,
            };
        }
    };
    let outcome = warm_opened(&graph, embedder, limit, progress).await;
    graph.shutdown();
    outcome
}

async fn warm_opened(
    graph: &stella_graph::CodeGraph,
    embedder: &dyn Embedder,
    limit: usize,
    progress: &mut dyn FnMut(usize),
) -> WarmOutcome {
    let fingerprint = embedder.fingerprint().id();
    let mut embedded = 0usize;
    let mut unreadable = 0usize;
    while embedded < limit {
        let want = EMBED_BATCH.min(limit - embedded);
        let scan = match graph.files_pending_embedding(&fingerprint, want) {
            Ok(scan) => scan,
            Err(error) => {
                return WarmOutcome::Failed {
                    embedded,
                    reason: format!("cannot read the code graph: {error}"),
                };
            }
        };
        // Overwritten, never summed: each round rescans the pending set from
        // the start, so an unreadable file is stepped over again every round
        // and adding the counts would multiply it by the round count. The last
        // round's figure is the one that walked furthest.
        unreadable = unreadable.max(scan.unreadable);
        // No files means the pending set is exhausted — a window landing on
        // unreadable rows keeps filling past them, so this is genuinely done
        // rather than a scan that gave up early (#3016).
        if scan.files.is_empty() {
            break;
        }
        if let Err(reason) = embed_batch(graph, embedder, &fingerprint, &scan.files).await {
            return WarmOutcome::Failed { embedded, reason };
        }
        embedded += scan.files.len();
        progress(embedded);
    }

    let total = graph.file_count().unwrap_or(0);
    let stored = graph.embedded_file_count(&fingerprint).unwrap_or(embedded);
    WarmOutcome::Warmed {
        embedded,
        remaining: total.saturating_sub(stored),
        unreadable,
    }
}

/// Embed every file still pending under `fingerprint`, up to the per-pass
/// cap. The search ladder's lazy catch-up, sharing `stella-graph`'s
/// pending-scan cursor with the eager pass above so the two warm one index
/// rather than two.
pub(crate) async fn catch_up_embeddings(
    graph: &stella_graph::CodeGraph,
    embedder: &dyn Embedder,
    fingerprint: &str,
) -> Result<(), String> {
    let scan = graph
        .files_pending_embedding(fingerprint, stella_graph::MAX_FILES_PER_PASS)
        .map_err(|error| format!("the code graph could not be read: {error}"))?;
    if scan.files.is_empty() {
        return Ok(());
    }
    for chunk in scan.files.chunks(EMBED_BATCH) {
        embed_batch(graph, embedder, fingerprint, chunk).await?;
    }
    Ok(())
}

/// Embed one batch and commit it — the single place a file's vector comes
/// into existence, shared by the lazy query pass and the eager `init` pass so
/// there is one render, one table and one fingerprint discipline.
async fn embed_batch(
    graph: &stella_graph::CodeGraph,
    embedder: &dyn Embedder,
    fingerprint: &str,
    batch: &[stella_graph::PendingEmbed],
) -> Result<(), String> {
    let texts: Vec<String> = batch.iter().map(|p| p.text.clone()).collect();
    let embeddings = embedder
        .embed(&texts)
        .await
        .map_err(|error| format!("the embedder failed: {error}"))?;
    let rows: Vec<FileVector> = batch
        .iter()
        .zip(embeddings)
        .map(|(pending, embedding)| FileVector {
            path: pending.path.clone(),
            content_sha256: pending.content_sha256.clone(),
            vector: embedding.vector,
        })
        .collect();
    graph
        .store_file_vectors(fingerprint, &rows)
        // A write failure here is not recoverable by continuing: the next
        // batch would be written into the same broken store and the pass
        // would report success having stored nothing.
        .map_err(|error| format!("cannot store file vectors: {error}"))
        .map(|_| ())
}
