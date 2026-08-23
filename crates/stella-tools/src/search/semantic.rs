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

/// Whole-file embedding requests kept in flight at once.
///
/// The file rung batched correctly and issued strictly serially: one request,
/// awaited, then the next scan. On this repository 1716 files at
/// [`EMBED_BATCH`] is 54 round trips end to end, paid at session start and in
/// `stella init` while nothing overlaps — and this rung runs *before* the
/// chunk rung, so its latency is additive to the pass the first prompt waits
/// on (#4190).
///
/// The same number as the chunk rung's `CHUNK_EMBED_CONCURRENCY`
/// (`super::engine`) and for the same reason: it bounds how much of a
/// provider's rate limit one background pass may claim, and every embedding
/// endpoint documents a concurrency well above it. It bears on cost only
/// through wall clock — every file is still embedded and stored exactly once,
/// in scan order, whatever this number is.
const FILE_EMBED_CONCURRENCY: usize = 8;

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
/// The background pass ([`super::backfill`]) covers a session; this covers
/// the single-turn run that has no session to amortise an index over, because
/// `stella init` is the one command whose job is to make the workspace ready.
/// Same render, same table, same `(file_id, fingerprint)` keying — the work is
/// free from every later session's perspective.
///
/// Windowed rather than one big pass: each round asks for at most
/// [`FILE_EMBED_CONCURRENCY`] requests' worth of pending files, so the
/// blocking file reads stay bounded between awaits and a pass killed halfway
/// has committed every request that had already landed.
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
        // One scan per *window*, not per request: the pending set is re-asked
        // only once every request from the previous window has settled, so a
        // file cannot be handed out twice and paid for twice. The window is
        // what the loop can hold in flight, and asking for less than that
        // would leave the concurrency below unfillable.
        let want = (EMBED_BATCH * FILE_EMBED_CONCURRENCY).min(limit - embedded);
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
        // Committed per request rather than per window, so `embedded` is what
        // actually reached the store even when a later request in the same
        // window fails. The loop then re-scans and the abandoned files come
        // back as pending — which is only true because a batch is stored
        // whole or not at all.
        let mut committed = 0usize;
        let outcome =
            embed_window(graph, embedder, &fingerprint, &scan.files, &mut committed).await;
        embedded += committed;
        progress(embedded);
        if let Err(error) = outcome {
            return WarmOutcome::Failed {
                embedded,
                reason: error.to_string(),
            };
        }
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

/// Embed one window of pending files and commit each request's rows as they
/// land — the single place a whole-file vector comes into existence, shared by
/// the eager `init` pass and the background one, so there is one render, one
/// table and one fingerprint discipline.
///
/// `window` is cut into [`EMBED_BATCH`]-sized requests and up to
/// [`FILE_EMBED_CONCURRENCY`] of them are in flight at once (#4190). Unlike
/// the chunk rung ([`super::engine`]), one file is one vector, so the batches
/// were already full before this — what was missing was only the overlap.
///
/// **Each response carries the ordinal of the request it answers.**
/// `buffer_unordered` yields results as they *complete*, so pairing the Nth
/// completion with the Nth batch would file one request's vectors against
/// whichever files happened to be in a slower one. Nothing would fail: every
/// file still gets a well-formed vector of the right width, and the only
/// symptom is a semantic index that answers with the wrong files. It is the
/// same misattribution `HttpEmbedder::embed` refuses one layer down when it
/// routes rows by their `index` rather than by arrival order (#4189).
///
/// `committed` counts the files whose vectors reached the store, and is
/// advanced as they do rather than at the end: a request that fails must not
/// erase the durable work of the ones that already succeeded, and the caller's
/// re-scan finds the rest still pending.
async fn embed_window(
    graph: &stella_graph::CodeGraph,
    embedder: &dyn Embedder,
    fingerprint: &str,
    window: &[stella_graph::PendingEmbed],
    committed: &mut usize,
) -> Result<(), EmbedError> {
    use futures_util::stream::{self, StreamExt};

    // Each request owns its texts rather than borrowing `window`. A borrow
    // travelling through these futures puts a higher-ranked lifetime on the
    // background pass's `tokio::spawn` in `stella-cli`, where it fails as
    // "implementation of `Send` is not general enough" pointing at a spawn
    // that mentions none of this — the same reason the chunk rung's
    // `NeededChunk` owns its text.
    let requests: Vec<(usize, Vec<String>)> = window
        .chunks(EMBED_BATCH)
        .enumerate()
        .map(|(ordinal, batch)| (ordinal, batch.iter().map(|p| p.text.clone()).collect()))
        .collect();

    let mut in_flight = stream::iter(requests.into_iter().map(|(ordinal, texts)| async move {
        embedder
            .embed(&texts)
            .await
            .map(|embeddings| (ordinal, embeddings))
    }))
    .buffer_unordered(FILE_EMBED_CONCURRENCY);

    while let Some(result) = in_flight.next().await {
        let (ordinal, embeddings) =
            result.map_err(|error| EmbedError::Embedder(error.to_string()))?;

        // The ordinal names the request, so this is the same slice the texts
        // were cut from — the one arithmetic that has to agree with
        // `requests` above, and it agrees by construction.
        let start = ordinal * EMBED_BATCH;
        let batch = &window[start..(start + EMBED_BATCH).min(window.len())];

        // `zip` would silently drop the tail of a short response, leaving
        // those files pending forever while the pass reported success — the
        // caller's loop would re-scan them and pay for them again every round.
        // The trait's contract is one vector per text; naming the breach beats
        // spinning on it.
        if embeddings.len() != batch.len() {
            return Err(EmbedError::Embedder(format!(
                "the embedder returned {} vectors for {} files",
                embeddings.len(),
                batch.len()
            )));
        }

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
            .map_err(|error| EmbedError::GraphWrite(error.to_string()))?;
        *committed += batch.len();
    }

    Ok(())
}
