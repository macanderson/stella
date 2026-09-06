// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A pass on a thread of its own, under a runtime of its own.
//!
//! An embedding pass mixes SQLite reads and writes, file reads and hashing,
//! and the embedder's HTTP round trips. Run as a task on a shared runtime,
//! every synchronous step holds a worker. AGENTS.md architecture rule 2 bans
//! that, and the deck's keyboard once went dead for six minutes because of it.
//! `spawn_blocking` around each call would fix the calls that exist today and
//! miss the next one somebody adds. A thread fixes them all at once: the
//! pass runs under a current-thread runtime that nothing else shares, so
//! whatever it blocks on, it blocks only itself.
//!
//! Progress and the outcome cross back as messages. The caller's callbacks
//! then run on the caller's runtime, and the thread borrows nothing from it.
//! That is what lets the deck session forward readiness to the deck, and lets
//! `stella init` narrate through an emitter that is not `Send`.
//!
//! [`super::backfill`] hosts the session's pass this way, and
//! [`super::eager`] hosts `stella init`'s.

use std::future::Future;
use std::io;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

/// A pass running on its own thread. `P` is one progress message; `O` is the
/// outcome the pass ends with.
pub struct PassThread<P, O> {
    progress: UnboundedReceiver<P>,
    done: oneshot::Receiver<Result<O, io::Error>>,
}

impl<P, O> PassThread<P, O> {
    /// The next progress message, or `None` once the pass has stopped
    /// sending them.
    pub async fn progress(&mut self) -> Option<P> {
        self.progress.recv().await
    }

    /// Wait for the pass to end.
    pub async fn join(self) -> Result<O, PassFailure> {
        // Anything still queued is the pass's business. A caller that stopped
        // reading progress wants the outcome alone.
        drop(self.progress);
        match self.done.await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(error)) => Err(PassFailure::NoRuntime(error)),
            Err(_) => Err(PassFailure::Died),
        }
    }
}

/// Why a pass ended with no outcome.
#[derive(Debug)]
pub enum PassFailure {
    /// The thread could not build a runtime to run the pass under.
    NoRuntime(io::Error),
    /// The thread ended without reporting. Only a panic in the pass does that.
    Died,
}

impl std::fmt::Display for PassFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRuntime(error) => write!(f, "could not start a runtime for it: {error}"),
            Self::Died => f.write_str("ended without reporting"),
        }
    }
}

impl std::error::Error for PassFailure {}

/// Run `pass` on a new thread named `name`, under a current-thread runtime.
///
/// `pass` is given the sender for progress messages and builds the future to
/// run. Fails only when the OS refuses the thread; then there is no pass, and
/// the caller says so. A runtime that cannot be built reaches the caller
/// through [`PassThread::join`] as [`PassFailure::NoRuntime`].
///
/// The future may hold an HTTP client. Such a client pools connections on the
/// runtime that opened them, so the client must belong to the pass and be
/// dropped with its runtime, never lent from the caller's.
pub fn spawn<P, O, F, Fut>(name: &str, pass: F) -> io::Result<PassThread<P, O>>
where
    P: Send + 'static,
    O: Send + 'static,
    F: FnOnce(UnboundedSender<P>) -> Fut + Send + 'static,
    Fut: Future<Output = O>,
{
    let (progress_tx, progress) = tokio::sync::mpsc::unbounded_channel();
    let (done_tx, done) = oneshot::channel();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let outcome = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map(|runtime| runtime.block_on(pass(progress_tx)));
            // A dropped sender reads as the thread dying. A caller gone first
            // has nothing to be told.
            let _ = done_tx.send(outcome);
        })?;
    Ok(PassThread { progress, done })
}

/// The instruments the thread witnesses share: an embedder that blocks its
/// thread, and a one-worker runtime with a probe that must keep answering.
#[cfg(test)]
pub(super) mod probe {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use async_trait::async_trait;
    use stella_embed::{EmbedError, Embedder, EmbedderFingerprint, Embedding, SimilarityPosture};
    use stella_graph::CodeGraph;

    /// How long the blocking embedder holds whatever thread it runs on.
    pub(crate) const HOLD: Duration = Duration::from_millis(500);

    /// A future the harness runs as the pass: pinned, boxed, `Send`.
    pub(crate) type ProbeHandle = std::pin::Pin<Box<dyn Future<Output = ()> + Send>>;

    /// Write `files` one-symbol fixture files under `root` and index them at
    /// the path a workspace's own pass opens (`.stella/private/codegraph.db`).
    /// Closed on return: the pass opens its own handle.
    pub(crate) fn indexed_private_fixture(root: &Path, files: usize) {
        for index in 0..files {
            std::fs::write(
                root.join(format!("file_{index}.rs")),
                format!("pub fn thing_{index}() -> usize {{ {index} }}\n"),
            )
            .expect("write a fixture file");
        }
        let db_path = stella_store::workspace_private_sqlite_path(root, "codegraph.db")
            .expect("the private store path");
        let graph = CodeGraph::open(root, &db_path).expect("open the graph");
        graph.index_all().expect("index the fixture");
        graph.shutdown();
    }

    /// A backend that **blocks** its thread rather than awaiting. On its first
    /// call it wakes the probe, then sleeps with `std::thread::sleep`. It
    /// stands in for every synchronous step in a pass, with a hold long
    /// enough to measure.
    #[derive(Debug)]
    pub(crate) struct BlockingEmbedder {
        wake: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Instant>>>,
    }

    #[async_trait]
    impl Embedder for BlockingEmbedder {
        fn fingerprint(&self) -> EmbedderFingerprint {
            EmbedderFingerprint {
                model_id: "blocking".into(),
                revision: "1".into(),
                dims: 2,
                normalization: "l2".into(),
            }
        }

        async fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, EmbedError> {
            if let Some(wake) = self.wake.lock().expect("the wake slot").take() {
                let _ = wake.send(Instant::now());
                std::thread::sleep(HOLD);
            }
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

    /// One worker, so if the pass blocks a worker it blocks the only one. A
    /// probe task waits for the embedder's wake and answers. Returns how long
    /// the answer took from the wake.
    ///
    /// `host` gets a workspace root with two indexed files and the blocking
    /// embedder, and returns the future that waits for the pass to end.
    pub(crate) fn probe_latency(
        host: impl FnOnce(PathBuf, Box<dyn Embedder>) -> ProbeHandle,
    ) -> Duration {
        let workspace = tempfile::tempdir().expect("tempdir");
        let root = workspace.path().canonicalize().expect("canonicalize");
        indexed_private_fixture(&root, 2);
        let (wake_tx, wake_rx) = tokio::sync::oneshot::channel();
        let embedder: Box<dyn Embedder> = Box::new(BlockingEmbedder {
            wake: std::sync::Mutex::new(Some(wake_tx)),
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            let probe = tokio::spawn(async move {
                let woken = wake_rx.await.expect("the embedder wakes the probe");
                woken.elapsed()
            });
            let pass = host(root, embedder);
            let latency = probe.await.expect("the probe answers");
            pass.await;
            latency
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::probe::HOLD;
    use super::*;

    /// **The witness for the primitive.** A pass that blocks its thread wakes
    /// a probe on a one-worker runtime, and the probe answers at once.
    #[test]
    fn a_blocking_pass_on_its_own_thread_cannot_hold_a_runtime_worker() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        let latency = runtime.block_on(async {
            let (wake_tx, wake_rx) = oneshot::channel::<Instant>();
            let probe = tokio::spawn(async move {
                let woken = wake_rx.await.expect("the pass wakes the probe");
                woken.elapsed()
            });
            let mut pass = spawn("probe", move |progress: UnboundedSender<u8>| async move {
                let _ = wake_tx.send(Instant::now());
                std::thread::sleep(HOLD);
                let _ = progress.send(1);
                "done"
            })
            .expect("thread");
            let latency = probe.await.expect("the probe answers");
            assert_eq!(pass.progress().await, Some(1));
            assert_eq!(
                pass.progress().await,
                None,
                "the sender drops with the pass"
            );
            assert_eq!(pass.join().await.expect("the pass reports"), "done");
            latency
        });
        assert!(
            latency < HOLD / 2,
            "the probe waited {latency:?} on a worker the pass does not run on"
        );
    }

    /// A pass that panics ends the thread with no report, and `join` says so
    /// rather than hanging or returning a made-up outcome.
    #[test]
    fn a_pass_that_dies_is_reported_as_dead() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let pass = spawn("dies", |_progress: UnboundedSender<()>| async {
                // Conditional so the block's output type is `()`, not `!`.
                if std::hint::black_box(true) {
                    std::panic::resume_unwind(Box::new("the pass dies"));
                }
            })
            .expect("thread");
            match pass.join().await {
                Err(PassFailure::Died) => {}
                other => panic!("a dead pass must report as dead: {other:?}"),
            }
        });
    }
}
