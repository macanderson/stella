//! Embedding catch-up ("warming") and its cancellation contract.
//!
//! [`ContextStore::open_and_warm`](crate::ContextStore::open_and_warm) spawns
//! [`warm_index`] as a background tokio task at mount (`L-C1`: pay indexing
//! at mount, not on the first prompt) whose join handle the store owns —
//! joined by `await_warm`, aborted on `Drop` (see the cancellation contract
//! below). This module owns that task's body and the one flag that can stop
//! it early.
//!
//! # Why there is a cancel flag at all
//!
//! The task used to be genuinely detached: nothing aborted it and its batch
//! loop had no deadline, budget, or abort check, so a store dropped mid-warm
//! left a task embedding — and writing to a SQLite file — for a workspace no
//! one was using any more. On a large first index that is real CPU and real
//! WAL traffic after the session it belonged to has gone (#613).
//!
//! `ContextStore`'s `Drop` now sets [`WarmCancel`] and aborts the join handle.
//! Both are needed, and neither is redundant:
//!
//! - `abort()` only takes effect at an `.await` point. The task spends most of
//!   its time inside `Embedder::embed`, which is an await for a hosted
//!   backend but resolves immediately for the pure-Rust default — a large
//!   `HashEmbedder` warm can therefore run whole batches without yielding.
//! - The flag is checked between batches, so it stops a task that abort
//!   cannot reach, at a boundary where the store is already consistent: each
//!   batch is its own transaction (`L-L1`), so stopping early leaves committed
//!   batches durable and the remainder simply un-embedded — which is exactly
//!   the state warming exists to catch up on, and the next mount will.
//!
//! What is deliberately NOT here is a `tokio::spawn` from `Drop`. During
//! runtime shutdown — the case a Ctrl-C'd session is in — that spawn silently
//! does nothing, so it would look like teardown in review while leaking in
//! production. Setting an `AtomicBool` and calling `JoinHandle::abort` are
//! both synchronous and need no runtime.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::clock::Clock;
use crate::embed::Embedder;
use crate::error::ContextError;
use crate::store::{nodes_missing_embedding, open_connection, store_embedding};

/// How many nodes are embedded and committed per transaction. Batching bounds
/// what a kill mid-index loses (`L-L1`) and keeps a backend with a
/// request-size limit from being handed the whole corpus at once.
///
/// `pub(crate)` because [`crate::writeback`]'s upsert embeds against the same
/// backend and had the same unbounded-request problem; one width, not two
/// (#616 item 8).
pub(crate) const BATCH: usize = 64;

/// The stop flag shared by a [`crate::ContextStore`] and its background warm
/// task. Cloneable and cheap; the store holds one, the task the other.
///
/// Only `ContextStore::drop` ever raises it, so a live store's warm always
/// runs to completion.
#[derive(Clone, Default)]
pub(crate) struct WarmCancel(Arc<AtomicBool>);

impl WarmCancel {
    /// Stop the warm at its next batch boundary. Idempotent.
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether the warm has been asked to stop.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Background embedding catch-up: embed every live node whose content lacks a
/// vector under the active fingerprint. Opens its own WAL connection (so it
/// never contends with the store's lock) and writes each batch as one
/// transaction (`L-L1` crash consistency). Returns the count embedded.
///
/// Returns `Ok(n)` — not an error — when `cancel` cuts it short: the batches
/// it did commit are durable and correct, and "how many vectors exist now" is
/// the honest answer to give a caller. There is no partial-write state to
/// report.
pub(crate) async fn warm_index(
    path: PathBuf,
    embedder: Arc<dyn Embedder>,
    fingerprint: String,
    clock: Arc<dyn Clock>,
    cancel: WarmCancel,
) -> Result<usize, ContextError> {
    // An in-memory store's second connection would be a different empty DB;
    // warming only makes sense for a file-backed store.
    if path.as_os_str() == ":memory:" {
        return Ok(0);
    }
    let conn = open_connection(&path)?;
    let mut embedded = 0usize;
    // One batch of bodies resident at a time: each committed batch drops out
    // of the missing-embedding predicate, so re-querying with a LIMIT is the
    // cursor — peak memory is bounded by [`BATCH`], not by the backlog a
    // large first index leaves behind.
    loop {
        // The abort check runs between batches, so the store is at a
        // transaction boundary and nothing is half-written.
        if cancel.is_cancelled() {
            break;
        }
        let pending = nodes_missing_embedding(&conn, &fingerprint, BATCH)?;
        if pending.is_empty() {
            break;
        }
        let texts: Vec<String> = pending.iter().map(|(_, c)| c.clone()).collect();
        let vectors = embedder.embed(&texts).await?;
        let now = clock.now_rfc3339();
        let tx = conn.unchecked_transaction()?;
        let mut stored = 0usize;
        for ((content_hash, _), emb) in pending.iter().zip(vectors.iter()) {
            if store_embedding(&tx, content_hash, &fingerprint, &emb.vector, &now)? {
                stored += 1;
            }
        }
        tx.commit()?;
        embedded += stored;
        // A backend that answers with fewer vectors than texts leaves the
        // shortfall matching the query forever; without this a zero-progress
        // batch would re-fetch the same rows in a tight loop.
        if stored == 0 {
            break;
        }
    }
    Ok(embedded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use crate::embed::{EmbedError, EmbedderFingerprint, Embedding, HashEmbedder};
    use crate::store::{ContextStore, NodeInput, NodeKind};
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    /// An embedder that parks inside the first batch until the test lets go,
    /// so a drop can be delivered while the warm is provably in flight.
    ///
    /// The gate receiver is *taken out* of the shared struct and awaited on
    /// the embed future's own stack. That is what makes the witness
    /// deterministic instead of timing-based: when the task is dropped, the
    /// receiver goes with it, and the test's `Sender::closed()` resolves.
    struct GatedEmbedder {
        inner: HashEmbedder,
        calls: AtomicUsize,
        started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    #[async_trait::async_trait]
    impl Embedder for GatedEmbedder {
        fn fingerprint(&self) -> EmbedderFingerprint {
            self.inner.fingerprint()
        }

        async fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, EmbedError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(tx) = self
                .started
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
            {
                let _ = tx.send(());
            }
            let gate = self.gate.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Some(rx) = gate {
                let _ = rx.await;
            }
            self.inner.embed(texts).await
        }
    }

    /// Seed `count` nodes whose vectors exist only under a *stale* embedder
    /// revision, which is what leaves work for a later warm to do: `upsert`
    /// embeds inline under its own store's fingerprint, so seeding with the
    /// same embedder would leave nothing pending.
    async fn seed(path: &std::path::Path, count: usize) {
        let store = ContextStore::open_with(
            path,
            Arc::new(HashEmbedder::with_revision("stale")),
            Arc::new(SystemClock),
        )
        .unwrap();
        let mut delta = crate::writeback::ContextDelta::new();
        for i in 0..count {
            delta = delta.with_node(
                NodeInput::new(NodeKind::Concept, format!("node-{i}"))
                    .with_content(format!("distinct content {i}")),
            );
        }
        store.upsert(delta).await.unwrap();
    }

    /// #613: dropping the store must stop its detached warm task. Before this,
    /// nothing aborted the handle and the batch loop had no abort check, so
    /// the task kept embedding for a store nobody held.
    ///
    /// The assertion is on the task's future being *dropped* (the gate sender
    /// observing its receiver disappear), which is exact — no sleep, no
    /// "wait and hope it didn't advance".
    #[tokio::test]
    async fn dropping_the_store_stops_the_background_warm() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context.db");
        seed(&path, BATCH * 3).await;

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (mut gate_tx, gate_rx) = tokio::sync::oneshot::channel();
        let embedder = Arc::new(GatedEmbedder {
            inner: HashEmbedder::default(),
            calls: AtomicUsize::new(0),
            started: Mutex::new(Some(started_tx)),
            gate: Mutex::new(Some(gate_rx)),
        });

        let store =
            ContextStore::open_and_warm(&path, embedder.clone(), Arc::new(SystemClock)).unwrap();
        started_rx.await.expect("the warm task never began a batch");
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);

        drop(store);

        // Resolves only when the receiver — which lives on the aborted task's
        // stack — is dropped. On the old code it never resolves, so the
        // timeout is the failure report rather than a hang.
        tokio::time::timeout(std::time::Duration::from_secs(10), gate_tx.closed())
            .await
            .expect("dropping the store must abort the in-flight warm task");
        assert_eq!(
            embedder.calls.load(Ordering::SeqCst),
            1,
            "no further batch may start after the store is dropped"
        );
    }

    /// The flag half, independent of `abort`: a warm already asked to stop
    /// does no work at all, and stops at a batch boundary rather than
    /// mid-transaction.
    #[tokio::test]
    async fn a_cancelled_warm_stops_at_a_batch_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context.db");
        seed(&path, BATCH * 2).await;

        let cancel = WarmCancel::default();
        cancel.cancel();
        assert!(cancel.is_cancelled());

        let embedder = Arc::new(HashEmbedder::default());
        let fingerprint = embedder.fingerprint().id();
        let embedded = warm_index(
            path.clone(),
            embedder.clone(),
            fingerprint.clone(),
            Arc::new(SystemClock),
            cancel,
        )
        .await
        .unwrap();
        assert_eq!(embedded, 0, "a cancelled warm must not embed a batch");

        // Un-cancelled, the same call embeds everything — so the zero above is
        // the flag, not an empty work list.
        let embedded = warm_index(
            path,
            embedder,
            fingerprint,
            Arc::new(SystemClock),
            WarmCancel::default(),
        )
        .await
        .unwrap();
        assert_eq!(embedded, BATCH * 2);
    }

    /// `await_warm`'s documented join-once semantics are unchanged by the new
    /// `Drop`: a caller that already joined sees the same answer it always
    /// did, and dropping afterwards aborts nothing (the handle is gone).
    #[tokio::test]
    async fn joining_before_the_drop_keeps_await_warm_s_contract() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context.db");
        seed(&path, 3).await;

        let store = ContextStore::open_and_warm(
            &path,
            Arc::new(HashEmbedder::default()),
            Arc::new(SystemClock),
        )
        .unwrap();
        assert_eq!(store.await_warm().await.unwrap(), 3);
        // Join-once: the handle was taken, so a second call is `Ok(0)`.
        assert_eq!(store.await_warm().await.unwrap(), 0);
        drop(store);

        // The work survived the drop, because it had already completed.
        let reopened = ContextStore::open(&path).unwrap();
        assert_eq!(reopened.warm_now().await.unwrap(), 0);
    }
}
