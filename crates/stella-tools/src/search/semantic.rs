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
//!
//! Both callers of the pass below run it **without a query waiting on it**:
//! the eager one is `stella init`'s, and the background one is
//! [`super::backfill`]'s, at session start. There is no longer a third — the
//! lazy per-query catch-up the search ladder used to run was deleted in #4043,
//! and that module's header carries the argument.

use std::path::Path;

use stella_embed::Embedder;
use stella_graph::FileVector;

/// Files embedded per request to the backend. Every provider accepts a
/// batch; 32 keeps a single request well inside every documented body limit
/// while still amortising the round trip across a warm-up pass.
pub const EMBED_BATCH: usize = 32;

/// **There is no ceiling on how much of a workspace gets indexed.** Every
/// pass — `stella init`'s eager one and [`super::backfill`]'s background one —
/// embeds every pending file, and the number of files in the workspace is the
/// only bound.
///
/// It used to stop at two thousand, on the argument that a pass in front of a
/// person should be bounded by their patience. The argument fails on the only
/// workspace it applies to: a repository over the cap could never become
/// fully searchable, because each pass stopped at the same number and every
/// search's answer stayed drawn from the same partial corpus. A cap on an
/// index is not a budget, it is a permanent hole — and a hole exactly where
/// the tool is most needed, since the repositories over the cap are the ones
/// nobody can hold in their head.
///
/// What is given up: a first `stella init` on a very large repository embeds
/// all of it, which costs more of the user's embedding budget up front than
/// it used to. The pass still narrates as it goes, still commits every batch,
/// and is still interruptible — and this is a one-time cost per workspace,
/// where the hole it replaces was permanent.
pub const NO_FILE_CEILING: usize = usize::MAX;

/// What an eager pass did, as data the caller renders.
///
/// Total by construction and never a `Result`: on this path a failure is a
/// *report*, not an error to propagate — `stella init` must succeed with no
/// embedder, with a broken embedder, and with a repository too big to finish,
/// and the difference between those is something a human reads, not something
/// a caller branches on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarmOutcome {
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
pub async fn warm_file_vectors(root: &Path, embedder: &dyn Embedder, limit: usize) -> WarmOutcome {
    warm_file_vectors_with_progress(root, embedder, limit, &mut |_| {}).await
}

/// The eager whole-file pass, with a progress callback (#3102): `progress`
/// receives the cumulative embedded-file count after each batch commits, so
/// a long pass can be narrated while it happens instead of summarised after.
/// Display-only — the callback cannot affect the pass.
pub async fn warm_file_vectors_with_progress(
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

/// One warm pass over an open graph, shared by the eager `stella init` pass
/// above and the background one in [`super::backfill`] — which is where the
/// lease discipline lives, because it is the pass that also fills chunks and
/// the two halves must be single-flight together.
///
/// The session-start *refresh* that used to live here (#3649) was bounded to
/// [`stella_graph::MAX_FILES_PER_PASS`] and skipped a workspace with no
/// vectors at all, on the argument that embedding a tree unasked spends the
/// user's money. #4043 replaced it with the unbounded, always-on pass in
/// `backfill`: with the lazy per-query catch-up gone, a workspace that opts
/// out of the background pass has nothing left to fill its index at all, so
/// the opt-in signal moved from "already has vectors" to "has an embedder
/// configured".
///
/// `progress` is generic rather than `&mut dyn FnMut(usize)` so each caller's
/// `Send`-ness is inferred instead of erased. The eager `stella init` pass
/// narrates through a callback that borrows a non-`Send` emitter; the
/// background pass runs inside a `tokio::spawn`ed task and needs the whole
/// future to be `Send`. A trait object forces one answer on both, and it is
/// the wrong one for whichever caller did not pick it.
pub(super) async fn warm_opened<P: FnMut(usize) + ?Sized>(
    graph: &stella_graph::CodeGraph,
    embedder: &dyn Embedder,
    limit: usize,
    progress: &mut P,
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
        if let Err(error) = embed_batch(graph, embedder, &fingerprint, &scan.files).await {
            return WarmOutcome::Failed {
                embedded,
                reason: error.to_string(),
            };
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

/// Why a warm pass stopped (invariant #5).
///
/// The split that matters to a caller is embedder-versus-index: an embedder
/// failure is usually configuration or a network hop and leaves the index
/// perfectly usable for the lexical rungs, while a graph read/write failure
/// means the index itself is the problem. The narrating callers degrade on
/// the first and report the second, and they could not tell them apart while
/// both arrived as prose.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    /// The code graph could not be read for pending work.
    #[error("the code graph could not be read: {0}")]
    GraphRead(String),
    /// The embedder itself failed — unconfigured, unreachable, or refusing.
    #[error("the embedder failed: {0}")]
    Embedder(String),
    /// The vectors were produced but could not be committed to the index.
    #[error("the embeddings could not be stored: {0}")]
    GraphWrite(String),
}

/// Embed one batch and commit it — the single place a file's vector comes
/// into existence, shared by the lazy query pass and the eager `init` pass so
/// there is one render, one table and one fingerprint discipline.
async fn embed_batch(
    graph: &stella_graph::CodeGraph,
    embedder: &dyn Embedder,
    fingerprint: &str,
    batch: &[stella_graph::PendingEmbed],
) -> Result<(), EmbedError> {
    let texts: Vec<String> = batch.iter().map(|p| p.text.clone()).collect();
    let embeddings = embedder
        .embed(&texts)
        .await
        .map_err(|error| EmbedError::Embedder(error.to_string()))?;
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
        .map_err(|error| EmbedError::GraphWrite(error.to_string()))
        .map(|_| ())
}
