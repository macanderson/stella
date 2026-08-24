//! Live, in-process incremental re-index while a session runs: the
//! code-graph watcher is an in-process `notify` task alive only for the
//! length of a session — no daemon, no background network listener.
//! Filesystem events are debounced so an editor's save-storm collapses into
//! a single re-index batch.
//!
//! The watcher only *feeds paths*; the actual re-index is one transactional
//! [`crate::store::apply_changes`] batch (L-L1), run on the blocking pool so
//! it never stalls async workers.
//!
//! # Structure: event source vs. pipeline
//!
//! Event *delivery* (the `notify` OS watcher) is deliberately decoupled from
//! the *pipeline* (channel → debounce → transactional apply). [`spawn`] wires
//! the real watcher to [`start_pipeline`]; tests drive the identical pipeline
//! through a [`WatchInjector`] instead, so live-index behavior is covered
//! without depending on OS event timing (FSEvents coalescing, inotify
//! batching). Both entry points share [`is_watch_relevant`], so the
//! language/ignore filtering under test is the production filter.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::error::GraphError;
use crate::graph::Inner;
use crate::lang::Language;
use crate::store::IndexStats;
use crate::walk;
use crate::workspace_ignore::WorkspaceIgnore;

/// Debounce window: filesystem bursts within this interval coalesce into one
/// re-index batch.
pub(crate) const DEBOUNCE: Duration = Duration::from_millis(200);

/// Ceiling on how long one batch may keep absorbing events before it applies
/// anyway. Without it the drain loop restarts its [`DEBOUNCE`] timeout on every
/// arrival, so any workload emitting events faster than the debounce (a
/// `cargo build`, a large `git checkout`, a watch-mode bundler) keeps the
/// window open for the whole burst: no re-index lands, `pending` grows
/// unbounded, and consumers awaiting a batch tick never see progress. A few
/// seconds keeps the coalescing benefit while guaranteeing the index applies
/// during a sustained storm.
const MAX_BATCH_WINDOW: Duration = Duration::from_secs(2);

/// Failsafe bound on [`WatchInjector::applied_past`]. Assertions never depend
/// on it — it only turns a wedged pipeline into a fast test failure instead
/// of a hang (and under a paused tokio clock it is skipped instantly).
const APPLIED_FAILSAFE: Duration = Duration::from_secs(60);

/// The applied-batch feed: a monotonically increasing batch sequence number
/// plus the stats of the most recently applied batch.
type BatchTick = (u64, IndexStats);

/// What the watcher feeds the debounce loop.
///
/// Two kinds, because a HEAD move and a file save want different work: the
/// first re-asks git what changed, the second re-indexes a known path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Signal {
    /// A source file was created, modified, or removed.
    Path(PathBuf),
    /// The repository's ref state moved — a commit, merge, rebase, pull,
    /// checkout, or bisect step.
    RefsMoved,
}

/// Does this path mean the repository's committed history just moved (#3651)?
///
/// Three shapes, and all three are needed. `.git/HEAD` covers a checkout or a
/// detached-HEAD move. `.git/refs/**` covers a branch or tag update written
/// as a loose ref. `.git/packed-refs` covers the same update when git has
/// packed it instead, which it does routinely after a fetch or `gc` — watching
/// only the loose refs would miss exactly the pulls that touch the most.
///
/// **Known gap:** in a linked worktree `.git` is a *file* pointing at the real
/// git directory elsewhere, so none of these paths exist under `root` and no
/// event arrives. That costs latency and not correctness — the mount-time
/// reconcile still catches the move at the next session — which is the whole
/// point of the reconciliation shape (see [`crate::reconcile`]).
fn is_ref_state_path(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let mut parts = rel.components().map(|c| c.as_os_str());
    if parts.next().is_none_or(|first| first != ".git") {
        return false;
    }
    match parts.next() {
        Some(second) => second == "HEAD" || second == "packed-refs" || second == "refs",
        None => false,
    }
}

/// Whether a watcher-delivered path is worth re-indexing: a source file we
/// recognize, not inside an ignored directory. The single relevance filter
/// shared by the real `notify` callback and [`WatchInjector::inject`].
///
/// A path that no longer exists passes without the extension check: a remove
/// event for a DIRECTORY carries the directory's path, which no language
/// claims, and filtering it out would leave every indexed file under it
/// stale forever. [`crate::store::apply_changes`] prunes a vanished path by
/// prefix, so an irrelevant deletion (an editor swap file, say) costs one
/// no-op batch, never a wrong row.
///
/// Note this rejects everything under `.git` (via `rel_is_ignored`'s
/// dot-prefix rule), which is why [`is_ref_state_path`] is consulted *before*
/// this and not folded into it: a ref move is not a file to re-index, it is a
/// reason to re-ask git what changed.
fn is_watch_relevant(root: &Path, path: &Path, ignore: &WorkspaceIgnore) -> bool {
    if walk::rel_is_ignored(root, path, ignore) {
        return false;
    }
    if !path.exists() {
        return true;
    }
    Language::from_path(path).is_some() || crate::storage::indexes_without_language(path)
}

/// Spawn the consumption side of live indexing: an unbounded path channel
/// whose receiver runs [`debounce_loop`]. Returns the sender that feeds it
/// and a receiver of [`BatchTick`]s, published after every applied batch —
/// the deterministic completion signal (no wall-clock polling).
/// `ignore` is shared with the event source so a ref move can refresh it in
/// place — see [`SharedIgnore`].
fn start_pipeline(
    inner: Arc<Inner>,
    debounce: Duration,
    ignore: SharedIgnore,
) -> (
    mpsc::UnboundedSender<Signal>,
    tokio::sync::watch::Receiver<BatchTick>,
) {
    let (tx, rx) = mpsc::unbounded_channel::<Signal>();
    let (tick_tx, tick_rx) = tokio::sync::watch::channel((0, IndexStats::default()));
    let handle = tokio::spawn(debounce_loop(inner.clone(), rx, debounce, tick_tx, ignore));
    inner.push_task(handle);
    (tx, tick_rx)
}

/// The ignore rules the event filter consults, shared between the event
/// source and the debounce loop so the latter can refresh them (#3651).
///
/// Resolving per event was never an option — the filter runs on every saved
/// file and resolving means invoking `git`, so it would be one subprocess per
/// keystroke. Resolving exactly once was the previous answer, and its gap was
/// real: a merge that edits `.gitignore` leaves the watcher enforcing the old
/// rules for the rest of the session, so a file the merge *un*-ignored is
/// filtered out of every event until the next full walk.
///
/// A ref move is the one moment worth paying the resolve: it is rare, it is
/// exactly when `.gitignore` changes arrive, and the reconcile it triggers is
/// already doing subprocess work. The lock is uncontended in the common case
/// and costs nothing next to the SQLite batch it gates.
type SharedIgnore = Arc<std::sync::Mutex<WorkspaceIgnore>>;

/// Read the shared rules for one filter call, tolerating a poisoned lock —
/// a filter that refuses to answer would stop live indexing outright, which
/// is a far worse outcome than filtering against rules a panic left behind.
fn with_ignore<R>(ignore: &SharedIgnore, act: impl FnOnce(&WorkspaceIgnore) -> R) -> R {
    act(&ignore
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()))
}

/// Arm a recursive watcher on the workspace root and spawn the debounce loop.
/// Returns the watcher handle, which the caller must keep alive — dropping it
/// stops watching (and closes the channel, so the debounce task exits
/// cleanly, which is exactly how [`crate::graph::CodeGraph::shutdown`] tears
/// it down).
pub(crate) fn spawn(
    inner: Arc<Inner>,
    debounce: Duration,
) -> Result<RecommendedWatcher, GraphError> {
    let root = inner.root.clone();
    // The tick receiver is dropped: production has no consumer for the
    // completion signal (the loop publishes via `send_modify`, which never
    // fails without receivers). If watcher creation fails below, `tx` is
    // dropped with the closure and the pipeline task exits on channel close.
    // Resolved once here and refreshed on a ref move, never per event — see
    // [`SharedIgnore`] for why both halves of that are required.
    let event_ignore: SharedIgnore =
        Arc::new(std::sync::Mutex::new(WorkspaceIgnore::resolve(&root)));
    let (tx, _ticks) = start_pipeline(inner, debounce, event_ignore.clone());

    let event_root = root.clone();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
        let Ok(event) = result else {
            return;
        };
        for path in event.paths {
            // Ref state first: `.git/**` is rejected by the relevance filter's
            // dot-prefix rule, so asking in the other order would drop every
            // HEAD move on the floor (#3651).
            if is_ref_state_path(&event_root, &path) {
                let _ = tx.send(Signal::RefsMoved);
            } else if with_ignore(&event_ignore, |ignore| {
                is_watch_relevant(&event_root, &path, ignore)
            }) {
                // Only source files we index, and never inside ignored dirs.
                let _ = tx.send(Signal::Path(path));
            }
        }
    })
    .map_err(|e| GraphError::Watch(e.to_string()))?;

    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|e| GraphError::Watch(e.to_string()))?;

    Ok(watcher)
}

/// Start the live-index pipeline **without** an OS watcher and return the
/// injector that stands in for it. See [`WatchInjector`]. Must be called from
/// within a tokio runtime, like [`spawn`].
pub(crate) fn spawn_injectable(inner: Arc<Inner>, debounce: Duration) -> WatchInjector {
    let root = inner.root.clone();
    let ignore: SharedIgnore = Arc::new(std::sync::Mutex::new(WorkspaceIgnore::resolve(&root)));
    let (tx, ticks) = start_pipeline(inner, debounce, ignore.clone());
    WatchInjector {
        root,
        ignore,
        tx,
        ticks,
    }
}

/// Test seam: stands in for the OS watcher, feeding synthetic events into the
/// **real** debounce → transactional-apply pipeline. Injection replaces event
/// *delivery* only — injected paths are read from disk by
/// [`crate::store::apply_changes`] exactly as in production, so tests write
/// real files into the workspace and then inject their paths instead of
/// waiting on environment-dependent fs-event timing.
#[doc(hidden)]
pub struct WatchInjector {
    root: PathBuf,
    /// Shared with the pipeline exactly as the real watcher's is — the
    /// injector stands in for event *delivery* only, so it must apply the
    /// identical filter against the identical, identically-refreshed rules.
    ignore: SharedIgnore,
    tx: mpsc::UnboundedSender<Signal>,
    ticks: tokio::sync::watch::Receiver<BatchTick>,
}

impl WatchInjector {
    /// Inject `path` as if the OS watcher had delivered an event for it,
    /// applying the same relevance filter as the real `notify` callback.
    /// Returns whether the path passed the filter and was enqueued.
    pub fn inject(&self, path: &Path) -> bool {
        if is_ref_state_path(&self.root, path) {
            return self.tx.send(Signal::RefsMoved).is_ok();
        }
        if !with_ignore(&self.ignore, |ignore| {
            is_watch_relevant(&self.root, path, ignore)
        }) {
            return false;
        }
        self.tx.send(Signal::Path(path.to_path_buf())).is_ok()
    }

    /// Sequence number of the most recently applied batch (0 = none yet).
    pub fn batches_applied(&self) -> u64 {
        self.ticks.borrow().0
    }

    /// Wait until a batch with sequence number strictly greater than `seen`
    /// has been applied; returns that batch's `(seq, stats)`. `None` means
    /// the pipeline shut down or the failsafe elapsed — never a normal
    /// outcome for a healthy pipeline.
    pub async fn applied_past(&mut self, seen: u64) -> Option<BatchTick> {
        let wait = self.ticks.wait_for(|(seq, _)| *seq > seen);
        match tokio::time::timeout(APPLIED_FAILSAFE, wait).await {
            Ok(Ok(tick)) => Some(tick.clone()),
            _ => None,
        }
    }
}

/// Collect a burst of changed paths, then apply them in one batch and publish
/// a [`BatchTick`]. The burst is bounded by [`MAX_BATCH_WINDOW`], so a
/// sustained event stream coalesces without starving the indexer. Exits when
/// the channel closes (watcher or injector dropped) or shutdown is requested.
async fn debounce_loop(
    inner: Arc<Inner>,
    mut rx: mpsc::UnboundedReceiver<Signal>,
    debounce: Duration,
    ticks: tokio::sync::watch::Sender<BatchTick>,
    ignore: SharedIgnore,
) {
    loop {
        let first = match rx.recv().await {
            Some(signal) => signal,
            None => break, // watcher dropped
        };
        let mut pending: HashSet<PathBuf> = HashSet::new();
        // Collapsed to a flag rather than counted: a merge writes HEAD,
        // packed-refs and several loose refs, and every one of them asks the
        // same question. One reconciliation answers all of them.
        let mut refs_moved = false;
        match first {
            Signal::Path(path) => {
                pending.insert(path);
            }
            Signal::RefsMoved => refs_moved = true,
        }

        // Drain everything that arrives within the debounce window, but never
        // past `MAX_BATCH_WINDOW` from the first event: each arrival restarts
        // the debounce timeout, so the deadline is the only thing that ends a
        // batch under a sustained event stream.
        let deadline = tokio::time::Instant::now() + MAX_BATCH_WINDOW;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break; // max batch window reached — apply what we have
            }
            match tokio::time::timeout(debounce.min(remaining), rx.recv()).await {
                Ok(Some(Signal::Path(path))) => {
                    pending.insert(path);
                }
                Ok(Some(Signal::RefsMoved)) => refs_moved = true,
                Ok(None) => break, // channel closed mid-burst
                Err(_) => break,   // window elapsed (debounce or deadline)
            }
        }

        if inner.shutdown.load(Ordering::Relaxed) {
            break;
        }

        // A ref move is reconciled BEFORE the batch's own paths (#3651). The
        // reconcile prunes what the commit deleted and indexes what it added,
        // using git's answer rather than whichever removal events the OS
        // happened to deliver during the churn; the path batch below then
        // covers the working-tree edits git cannot see. Doing it in the other
        // order would let a stale path event re-create a row the commit just
        // removed.
        if refs_moved {
            let reconcile_inner_arc = inner.clone();
            let refreshed_ignore = ignore.clone();
            let _ = tokio::task::spawn_blocking(move || {
                // Re-resolve the ignore rules first: a merge may have edited
                // `.gitignore`, and a file it just UN-ignored is invisible to
                // the event filter until this runs. Both calls shell out to
                // git, so they belong on the same blocking hop.
                let fresh = WorkspaceIgnore::resolve(&reconcile_inner_arc.root);
                *refreshed_ignore
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = fresh;
                let oracle = crate::reconcile::GitCli::new(&reconcile_inner_arc.root);
                crate::graph::reconcile_inner(&reconcile_inner_arc, &oracle)
            })
            .await;
            if inner.shutdown.load(Ordering::Relaxed) {
                break;
            }
        }

        let paths: Vec<PathBuf> = pending.into_iter().collect();
        let batch_inner = inner.clone();
        // SQLite work is blocking + CPU-bound (re-parse) → off the async pool.
        let outcome =
            tokio::task::spawn_blocking(move || batch_inner.apply_changes_blocking(&paths)).await;
        // A tick means "batch attempt finished", success or not (L-L1: one
        // bad batch must not wedge consumers awaiting the signal). Failures
        // publish default stats.
        let stats = match outcome {
            Ok(Ok(stats)) => stats,
            _ => IndexStats::default(),
        };
        ticks.send_modify(|(seq, last)| {
            *seq += 1;
            *last = stats;
        });
    }
}
