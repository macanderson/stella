// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella init`'s embedding pass, on a thread of its own.
//!
//! The eager pass runs the same two rungs as the session's backfill: whole-file
//! vectors, then chunks. It takes no embed lease. A person who typed
//! `stella init` is owed the pass. "Another session was busy" is a worse
//! answer to a direct order than doing the work twice.
//! `CodeGraph::index_all_single_flight` makes the same argument for the walk.
//!
//! It runs on its own thread for the reason [`super::own_thread`] gives.
//! `stella init` narrates as it goes, and its emitter is not `Send`. So the
//! pass reports over a channel, and the narration stays on the caller's side.
//! Each rung's progress is its own event. The file rung's outcome is sent when
//! that rung ends. So the caller can print the file summary before the chunk
//! headline, in the order the pass ran them.

use std::io;
use std::path::PathBuf;

use stella_embed::Embedder;

use super::engine::{ChunkWarmOutcome, NO_CHUNK_FILE_CEILING, warm_chunk_vectors_with_progress};
use super::own_thread::{self, PassThread};
use super::semantic::{NO_FILE_CEILING, WarmOutcome, warm_file_vectors_with_progress};

/// One report from the eager pass. They arrive in this order: file rung
/// progress, the file rung's outcome, then chunk rung progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EagerEvent {
    /// Files the file rung has embedded so far.
    FilesEmbedded(usize),
    /// The file rung ended. The chunk rung starts next.
    FilesFinished(WarmOutcome),
    /// Files whose chunks the chunk rung has embedded so far.
    ChunkFilesEmbedded(usize),
}

/// What the whole pass did, one outcome per rung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EagerOutcome {
    pub files: WarmOutcome,
    pub chunks: ChunkWarmOutcome,
}

/// Run both rungs over the workspace at `root` on a new thread. Each
/// [`EagerEvent`] is sent as it happens.
///
/// Fails only when the OS refuses the thread. The thread owns the embedder
/// and drops it, for the reason [`own_thread::spawn`] gives.
pub fn spawn_on_own_thread(
    root: PathBuf,
    embedder: Box<dyn Embedder>,
) -> io::Result<PassThread<EagerEvent, EagerOutcome>> {
    own_thread::spawn("stella-embed-init", move |events| async move {
        let files = warm_file_vectors_with_progress(
            &root,
            embedder.as_ref(),
            NO_FILE_CEILING,
            // A caller that stopped listening loses nothing it wanted. The
            // pass runs on.
            &mut |embedded| {
                let _ = events.send(EagerEvent::FilesEmbedded(embedded));
            },
        )
        .await;
        let _ = events.send(EagerEvent::FilesFinished(files.clone()));
        let chunks = warm_chunk_vectors_with_progress(
            &root,
            embedder.as_ref(),
            NO_CHUNK_FILE_CEILING,
            &mut |embedded| {
                let _ = events.send(EagerEvent::ChunkFilesEmbedded(embedded));
            },
        )
        .await;
        EagerOutcome { files, chunks }
    })
}

#[cfg(test)]
mod tests {
    use super::super::own_thread::probe::{HOLD, indexed_private_fixture, probe_latency};
    use super::*;

    /// **The witness for the thread.** The init pass on its own thread blocks
    /// its own thread and nothing else. A task on the command's runtime
    /// answers a wake at once while the pass sits in a synchronous call.
    #[test]
    fn the_init_pass_on_its_own_thread_cannot_hold_a_runtime_worker() {
        let latency = probe_latency(|root, embedder| {
            let pass = spawn_on_own_thread(root, embedder).expect("thread");
            Box::pin(async move {
                let outcome = pass.join().await.expect("the pass reports");
                assert!(
                    matches!(outcome.files, WarmOutcome::Warmed { .. }),
                    "{outcome:?}"
                );
            })
        });
        assert!(
            latency < HOLD / 2,
            "the probe waited {latency:?} on a worker the pass does not run on"
        );
    }

    /// The hazard the witness above guards against. Kept so the harness is
    /// shown to tell the two apart. The file rung hosted as a task on the
    /// runtime holds the one worker for the whole synchronous call. This is
    /// the shape `stella init` had.
    #[test]
    fn the_same_rungs_as_a_runtime_task_hold_the_worker_for_the_whole_call() {
        let latency = probe_latency(|root, embedder| {
            let task = tokio::spawn(async move {
                warm_file_vectors_with_progress(
                    &root,
                    embedder.as_ref(),
                    NO_FILE_CEILING,
                    &mut |_| {},
                )
                .await
            });
            Box::pin(async move {
                let _ = task.await;
            })
        });
        assert!(
            latency >= HOLD,
            "a task-hosted pass let the probe answer in {latency:?}; the worker was not held and \
             this test's premise has changed"
        );
    }

    /// Events arrive in the order the rungs ran. The file rung's outcome sits
    /// between the two progress streams. The outcome carries both rungs. That
    /// order is what lets `stella init` print each summary under its own
    /// headline.
    #[test]
    fn events_arrive_in_rung_order_and_the_outcome_carries_both_rungs() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let root = workspace.path().canonicalize().expect("canonicalize");
        indexed_private_fixture(&root, 3);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            let embedder: Box<dyn Embedder> =
                Box::new(super::super::backfill::tests::CountingEmbedder::default());
            let mut pass = spawn_on_own_thread(root, embedder).expect("thread");
            let mut events = Vec::new();
            while let Some(event) = pass.progress().await {
                events.push(event);
            }
            let finished = events
                .iter()
                .position(|event| matches!(event, EagerEvent::FilesFinished(_)))
                .expect("the file rung reports its end");
            assert!(
                events[..finished]
                    .iter()
                    .all(|event| matches!(event, EagerEvent::FilesEmbedded(_))),
                "only file progress precedes the file outcome: {events:?}"
            );
            assert!(
                events[finished + 1..]
                    .iter()
                    .all(|event| matches!(event, EagerEvent::ChunkFilesEmbedded(_))),
                "only chunk progress follows the file outcome: {events:?}"
            );
            assert!(
                matches!(
                    events[..finished].last(),
                    Some(EagerEvent::FilesEmbedded(3))
                ),
                "three files embed: {events:?}"
            );
            let outcome = pass.join().await.expect("the pass reports");
            assert!(
                matches!(outcome.files, WarmOutcome::Warmed { embedded: 3, .. }),
                "{outcome:?}"
            );
            assert!(
                matches!(
                    outcome.chunks,
                    ChunkWarmOutcome::Warmed {
                        files_embedded: 3,
                        ..
                    }
                ),
                "{outcome:?}"
            );
        });
    }
}
