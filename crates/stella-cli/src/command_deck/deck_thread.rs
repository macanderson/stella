//! The deck's own thread.
//!
//! A task on the shared runtime cannot promise that the screen never waits
//! on the driver. Tokio parks a task woken from a worker in that worker's
//! LIFO slot, and no other worker steals from that slot. So a driver task
//! that wakes the deck and then blocks its worker holds the deck for as long
//! as it blocks. On 2026-09-05 the block was a count over the code graph. It
//! took eleven seconds, and the embedding pass asked for it after every
//! batch. The keyboard was dead for six minutes. The reader thread was alive
//! and queueing keys the whole time, and nothing drained them.
//!
//! A thread keeps the promise. The deck runs its loop under a runtime of its
//! own, on a thread of its own. The driver cannot block that thread. The
//! deck's channels cross runtimes as any channel does. The tasks the deck
//! spawns for `!` shell commands and the clipboard live on its runtime and
//! end with it. The driver still waits for the deck through
//! [`DeckThread::join`], so a quit restores the terminal before the session's
//! own teardown runs.
//!
//! The driver's blocking call is a defect on its own (AGENTS.md architecture
//! rule 2 bans it), and it is fixed apart from this. This module is what lets
//! the keyboard survive the next one.

use std::future::Future;
use std::io;

use stella_tui::{DeckOptions, Inbound, WorkspaceInput, run_deck};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

/// The deck, running on its own thread. Dropping this does not stop the deck.
/// Closing its inbound channel does, as before.
pub(super) struct DeckThread {
    done: oneshot::Receiver<io::Result<()>>,
}

/// Start the deck on its own thread. This fails only when the OS refuses the
/// thread. Then there is no deck, and the session cannot start.
pub(super) fn spawn(
    opts: DeckOptions,
    inbound: UnboundedReceiver<Inbound>,
    submissions: UnboundedSender<WorkspaceInput>,
) -> io::Result<DeckThread> {
    let done = run_on_own_thread("stella-deck", run_deck(opts, inbound, submissions))?;
    Ok(DeckThread { done })
}

impl DeckThread {
    /// Wait for the deck to finish. `Err` means the thread ended without a
    /// report. Only a panic in the deck does that, and the terminal guard's
    /// panic hook has restored the screen by then.
    pub(super) async fn join(self) -> Result<io::Result<()>, DeckThreadGone> {
        self.done.await.map_err(|_| DeckThreadGone)
    }
}

/// The deck thread ended without a report.
#[derive(Debug)]
pub(super) struct DeckThreadGone;

impl std::fmt::Display for DeckThreadGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the deck thread ended without reporting")
    }
}

/// Drive `future` to its end on a new thread, under a current-thread runtime.
/// The output comes back on a channel. The receiver resolves when the future
/// finishes, and errors if the thread dies first.
fn run_on_own_thread<F>(name: &str, future: F) -> io::Result<oneshot::Receiver<F::Output>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let (done_tx, done_rx) = oneshot::channel();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => {
                    let output = runtime.block_on(future);
                    let _ = done_tx.send(output);
                }
                // The receiver reads the drop as the thread ending. The
                // caller reports it the way it reports a panic.
                Err(_) => drop(done_tx),
            }
        })?;
    Ok(done_rx)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use tokio::sync::{mpsc, oneshot};

    use super::run_on_own_thread;

    /// How long the driver-shaped task holds its worker.
    const HOLD: Duration = Duration::from_millis(500);

    /// One worker, so the task that wakes the probe and then blocks is the
    /// only worker there is. Returns how long the probe took to see the
    /// wake, measured from the moment it was sent.
    async fn probe_latency(host: impl FnOnce(ProbeFuture) -> ProbeHandle) -> Duration {
        let (wake_tx, mut wake_rx) = mpsc::unbounded_channel::<()>();
        let (seen_tx, seen_rx) = oneshot::channel::<Instant>();
        let probe: ProbeFuture = Box::pin(async move {
            wake_rx.recv().await;
            let _ = seen_tx.send(Instant::now());
        });
        let handle = host(probe);
        let sent = Instant::now();
        let blocker = tokio::spawn(async move {
            wake_tx.send(()).expect("probe is listening");
            std::thread::sleep(HOLD);
        });
        let seen = seen_rx.await.expect("probe reports");
        blocker.await.expect("blocker ends");
        handle.await;
        seen.duration_since(sent)
    }

    type ProbeFuture = std::pin::Pin<Box<dyn Future<Output = ()> + Send>>;
    type ProbeHandle = std::pin::Pin<Box<dyn Future<Output = ()> + Send>>;

    fn one_worker() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime")
    }

    /// **The witness for the thread.** A driver-shaped task wakes the deck
    /// and then blocks its worker. A deck on its own thread answers the wake
    /// at once, however long the worker stays blocked.
    #[test]
    fn a_blocked_runtime_worker_cannot_hold_a_deck_on_its_own_thread() {
        let latency = one_worker().block_on(probe_latency(|probe| {
            let done = run_on_own_thread("probe", probe).expect("thread");
            Box::pin(async move {
                let _ = done.await;
            })
        }));
        assert!(
            latency < HOLD / 2,
            "the deck waited {latency:?} on a worker it does not run on"
        );
    }

    /// The hazard the witness above guards against. It stays so the harness
    /// is shown to tell the two apart. The same probe, hosted as a task on
    /// the shared runtime, sits in the blocked worker's LIFO slot for the
    /// whole hold. This is the shape the keyboard freeze had.
    #[test]
    fn the_same_deck_as_a_runtime_task_waits_out_the_whole_hold() {
        let latency = one_worker().block_on(probe_latency(|probe| {
            let task = tokio::spawn(probe);
            Box::pin(async move {
                let _ = task.await;
            })
        }));
        assert!(
            latency >= HOLD,
            "a task-hosted deck answered in {latency:?}; the LIFO slot does not park it and \
             this test's premise has changed"
        );
    }
}
