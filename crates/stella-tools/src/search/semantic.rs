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
pub const EMBED_BATCH: usize = 32;

/// The most files one eager (`stella init`) pass will embed.
///
/// Ten times the lazy per-query cap, and bounded for a different reason. The
/// lazy cap trades coverage for the latency of a query someone is waiting
/// on; this pass runs before any turn starts, so its budget is the user's
/// money and patience rather than a round trip. A repository larger than this
/// gets a **stated** partial index — the emitted line names how many files
/// were left — because a partial index that silently ranks a subset is worse
/// than one that says which subset it ranked.
pub const MAX_FILES_PER_EAGER_PASS: usize = 2_000;

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

/// What one session-start refresh decided to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// The pass ran.
    Refreshed(WarmOutcome),
    /// This workspace has no vectors under the active fingerprint, so it has
    /// never opted into semantic indexing (or has just changed embedder).
    /// Embedding a whole tree unasked spends the user's API budget on a
    /// capability they may not want — that is `stella init`'s decision to
    /// take, not a session start's.
    NotOptedIn,
    /// Another pass holds the embed lease (#3650). It is doing this work.
    Busy,
}

/// Top up embeddings at session start, bounded, for a workspace that already
/// uses semantic search (#3649).
///
/// This is the push half of git-aware reconciliation. `vectors::pending`
/// orders by change recency, so after a merge the files that commit touched
/// are already at the front of the queue — but nothing advanced the queue
/// except `stella init` and the lazy pass inside a search. A session that
/// merged and then only ran `bash` and `read_file` never embedded them at
/// all. One bounded pass at session start is what makes the ordering pay off.
///
/// Three guards, each load-bearing:
///
/// - **Opt-in.** A workspace with zero vectors under this fingerprint has
///   never run the eager pass, and starting one unasked spends real money.
/// - **Single-flight.** The embed lease, so this cannot race the lazy
///   per-query pass or a second `stella` process.
/// - **Bounded.** [`stella_graph::MAX_FILES_PER_PASS`] rather than the eager
///   cap: a session
///   start is not the moment to embed two thousand files, and the recency
///   ordering means one batch covers what actually changed.
pub async fn refresh_recent_vectors(
    root: &Path,
    embedder: &dyn Embedder,
    limit: usize,
) -> RefreshOutcome {
    let graph = match stella_store::workspace_private_sqlite_path(root, "codegraph.db")
        .map_err(|error| format!("cannot prepare the code graph store: {error}"))
        .and_then(|db_path| {
            stella_graph::CodeGraph::open(root, &db_path)
                .map_err(|error| format!("could not open the code graph: {error}"))
        }) {
        Ok(graph) => graph,
        Err(reason) => {
            return RefreshOutcome::Refreshed(WarmOutcome::Failed {
                embedded: 0,
                reason,
            });
        }
    };

    let fingerprint = embedder.fingerprint().id();
    // Zero is the honest opt-in signal: it cannot distinguish "never wanted
    // this" from "just switched embedder", and it does not need to — both
    // mean a whole-tree embed the user has not asked this session to pay for.
    if graph.embedded_file_count(&fingerprint).unwrap_or(0) == 0 {
        graph.shutdown();
        return RefreshOutcome::NotOptedIn;
    }

    let Some(lease) = graph.acquire_lease(stella_graph::lease::Purpose::Embed) else {
        graph.shutdown();
        return RefreshOutcome::Busy;
    };
    let outcome = warm_opened(&graph, embedder, limit, &mut |_| {}).await;
    // Released on the failure path too: a pass that died holds no claim on
    // the next one, and leaving the lease behind would stall embedding for
    // the whole TTL over an error the caller already has in hand.
    graph.release_lease(&lease);
    graph.shutdown();
    RefreshOutcome::Refreshed(outcome)
}

/// `progress` is generic rather than `&mut dyn FnMut(usize)` so each caller's
/// `Send`-ness is inferred instead of erased. The eager `stella init` pass
/// narrates through a callback that borrows a non-`Send` emitter; the session
/// refresh runs inside a `tokio::spawn`ed task and needs the whole future to
/// be `Send`. A trait object forces one answer on both, and it is the wrong
/// one for whichever caller did not pick it.
async fn warm_opened<P: FnMut(usize) + ?Sized>(
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

/// Embed every file still pending under `fingerprint`, up to the per-pass
/// cap. The search ladder's lazy catch-up, sharing `stella-graph`'s
/// pending-scan cursor with the eager pass above so the two warm one index
/// rather than two.
pub async fn catch_up_embeddings(
    graph: &stella_graph::CodeGraph,
    embedder: &dyn Embedder,
    fingerprint: &str,
) -> Result<(), EmbedError> {
    let scan = graph
        .files_pending_embedding(fingerprint, stella_graph::MAX_FILES_PER_PASS)
        .map_err(|error| EmbedError::GraphRead(error.to_string()))?;
    if scan.files.is_empty() {
        return Ok(());
    }
    for chunk in scan.files.chunks(EMBED_BATCH) {
        embed_batch(graph, embedder, fingerprint, chunk).await?;
    }
    Ok(())
}

/// Why a warm or catch-up pass stopped (invariant #5).
///
/// The split that matters to a caller is embedder-versus-index: an embedder
/// failure is usually configuration or a network hop and leaves the index
/// perfectly usable for the lexical rungs, while a graph read/write failure
/// means the index itself is the problem. `super::engine` degrades on the
/// first and reports the second, and it could not tell them apart while both
/// arrived as prose.
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
