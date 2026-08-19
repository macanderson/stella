//! [`CodeGraph`] — the public handle: mount a workspace, index it, and query
//! it for [`ContextFrame`]s.
//!
//! # Warm at mount (L-C1)
//!
//! [`CodeGraph::mount`] opens the store synchronously (so queries are
//! immediately callable) and kicks the incremental catch-up + the live
//! watcher as a **background** task — building the graph lazily on first query
//! added seconds to the first real prompt; warming at mount hides the cost in
//! startup slack.
//!
//! # Two connections, one WAL file
//!
//! Indexing (writes) and queries (reads) use separate SQLite connections to
//! the same WAL file, so a long catch-up transaction never blocks a query on
//! a mutex. During a catch-up the reader sees the last committed snapshot
//! (best-effort, non-blocking) until the batch commits — the L-C1 discipline
//! of never adding latency to a query.
//!
//! # Signal safety (L-L1)
//!
//! There are no `unwrap`/`panic` in the hot path; a poisoned connection mutex
//! is recovered rather than propagated. Because every index batch is one
//! transaction, an abrupt process kill mid-index commits nothing and reopening
//! finds a consistent store.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use contextgraph_types::{ContextFrame, ContextQuery};
use notify::RecommendedWatcher;
use rusqlite::Connection;
use tokio::task::JoinHandle;

use crate::error::GraphError;
use crate::frames;
use crate::import;
use crate::lease;
use crate::parse::Grammars;
use crate::reconcile;
use crate::store::{self, IndexStats};
use crate::vectors;
use crate::watch;

/// Shared interior of a [`CodeGraph`], reference-counted so the background
/// catch-up task and watcher can hold it independently of the public handle.
pub(crate) struct Inner {
    pub(crate) root: PathBuf,
    pub(crate) grammars: Grammars,
    write_conn: Mutex<Connection>,
    read_conn: Mutex<Connection>,
    watcher: Mutex<Option<RecommendedWatcher>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    pub(crate) shutdown: AtomicBool,
}

impl Inner {
    fn write_guard(&self) -> MutexGuard<'_, Connection> {
        self.write_conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn read_guard(&self) -> MutexGuard<'_, Connection> {
        self.read_conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Apply a watcher-detected set of changes in one transaction. Called from
    /// the debounce loop on the blocking pool.
    pub(crate) fn apply_changes_blocking(
        &self,
        changed: &[PathBuf],
    ) -> Result<IndexStats, GraphError> {
        let mut conn = self.write_guard();
        store::apply_changes(&mut conn, &self.root, &self.grammars, changed)
    }

    pub(crate) fn push_task(&self, handle: JoinHandle<()>) {
        self.tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(handle);
    }

    /// Install the live watcher — but only if `shutdown()` has not already
    /// run. The flag check and the store happen under the **same** watcher
    /// lock that `shutdown()` takes to set the flag and clear the slot, so the
    /// install serializes against shutdown and the mount-vs-shutdown TOCTOU
    /// window is closed: a watcher created by the background task after a
    /// concurrent `shutdown()` is dropped here (at end of scope) instead of
    /// being stored, so it can never leak past shutdown.
    fn set_watcher(&self, watcher: RecommendedWatcher) {
        let mut slot = self
            .watcher
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.shutdown.load(Ordering::Relaxed) {
            // `watcher` is dropped at end of scope → stops watching at once.
            return;
        }
        *slot = Some(watcher);
    }
}

/// A mounted code graph over a workspace root, backed by a SQLite store.
pub struct CodeGraph {
    inner: std::sync::Arc<Inner>,
}

/// One symbol in a [`FileNeighborhood`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NeighborhoodSymbol {
    pub name: String,
    /// The stored kind tag as persisted by the index (`"function"`,
    /// `"struct"`, `"class"`, `"table"`, …) — see [`crate::SymbolKind::tag`].
    /// An owned `String` so this public type round-trips through serde without
    /// a borrow lifetime.
    pub kind: String,
    pub start_line: u32,
}

/// One definition site of a named symbol with its exact source span — the
/// raw `(path, start..=end)` location behind [`CodeGraph::definitions`]'
/// rendered frames. Lines are 1-based and inclusive, matching
/// [`crate::Symbol`]; `kind` is the human citation keyword (`"fn"`,
/// `"struct"`, …) so callers can label a site the way a frame's citation
/// does (L-C4).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SymbolSpan {
    /// Root-relative forward-slash path of the defining file.
    pub path: String,
    pub name: String,
    pub kind: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// The structured neighborhood of one file: its symbols and its import
/// edges in both directions. Root-relative forward-slash paths throughout.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileNeighborhood {
    pub file: String,
    pub symbols: Vec<NeighborhoodSymbol>,
    /// What this file imports: resolved paths where resolution succeeded,
    /// raw specifiers (e.g. Rust `use` paths) otherwise.
    pub imports: Vec<String>,
    /// Files whose imports resolve to this file.
    pub importers: Vec<String>,
}

impl CodeGraph {
    /// Open (or create) the store at `db_path` for the workspace at `root`,
    /// **without** starting background work. Use this for one-shot indexing,
    /// tests, or any caller that drives [`CodeGraph::index_all`] itself.
    ///
    /// `root` must exist; it is canonicalized so the workspace-root jail and
    /// relative-path computation are consistent.
    pub fn open(root: &Path, db_path: &Path) -> Result<CodeGraph, GraphError> {
        let root = root.canonicalize().map_err(|source| GraphError::Root {
            root: root.to_path_buf(),
            source,
        })?;
        // The writer's open migrates AND verifies the image (quarantining a
        // corrupt one); the read connection opens second, against a store the
        // line above has already proven, so it takes the cheap reader path
        // rather than paying the page walk twice.
        let write_conn = store::open(db_path)?;
        let read_conn = store::open_read(db_path)?;
        let grammars = Grammars::load()?;

        let inner = Inner {
            root,
            grammars,
            write_conn: Mutex::new(write_conn),
            read_conn: Mutex::new(read_conn),
            watcher: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
        };
        Ok(CodeGraph {
            inner: std::sync::Arc::new(inner),
        })
    }

    /// Open the store and kick incremental catch-up + the live watcher in the
    /// background (L-C1). Returns as soon as the store is open; the graph
    /// fills in behind the handle.
    pub async fn mount(root: &Path, db_path: &Path) -> Result<CodeGraph, GraphError> {
        let graph = CodeGraph::open(root, db_path)?;
        let inner = graph.inner.clone();

        let handle = tokio::spawn(async move {
            // 1) Reconcile against HEAD: if committed history moved since this
            //    store last looked — a merge, pull, rebase, checkout, or a
            //    clone someone handed us — index that commit range first. It
            //    is a small, bounded set, and doing it ahead of the walk is
            //    what makes those files the newest entries in the store and so
            //    the first ones the embedding passes pick up. Best-effort by
            //    construction: no repository, no git, or an unresolvable range
            //    all fall through to the full pass below, which is the
            //    behaviour this step is an accelerator on top of.
            let reconciled = inner.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let oracle = reconcile::GitCli::new(&reconciled.root);
                reconcile_inner(&reconciled, &oracle)
            })
            .await;

            if inner.shutdown.load(Ordering::Relaxed) {
                return;
            }
            // 2) Catch-up: diff stored hashes against the current tree. This
            //    is the correctness backstop — it catches every uncommitted
            //    edit, which no git range can see. Single-flight (#3650): a
            //    concurrent session or graph-tool open walking the same tree
            //    produces identical rows, so the second walk is waste.
            let catchup = inner.clone();
            let _ = tokio::task::spawn_blocking(move || walk_single_flight(&catchup)).await;

            if inner.shutdown.load(Ordering::Relaxed) {
                return;
            }
            // 3) Arm the live watcher. If it cannot be created, catch-up has
            // already run; live updates simply degrade to manual re-index.
            if let Ok(watcher) = watch::spawn(inner.clone(), watch::DEBOUNCE) {
                inner.set_watcher(watcher);
            }
        });
        graph.inner.push_task(handle);
        Ok(graph)
    }

    /// The canonicalized workspace root.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Test seam: start the live-index pipeline (debounce → transactional
    /// apply) **without** an OS filesystem watcher, returning a
    /// [`crate::WatchInjector`] that feeds synthetic events straight into the
    /// pipeline channel. Deterministic live-index tests write real files into
    /// the workspace, inject the paths, and await the injector's applied-batch
    /// signal — no dependence on OS event delivery timing.
    ///
    /// Hidden from docs: test-facing only, `pub` so integration tests in
    /// `tests/` can reach it. Must be called from within a tokio runtime.
    #[doc(hidden)]
    pub fn watch_pipeline_for_tests(&self, debounce: std::time::Duration) -> watch::WatchInjector {
        watch::spawn_injectable(self.inner.clone(), debounce)
    }

    /// Reconcile the index against the repository's current HEAD, indexing
    /// whatever committed history moved underneath it.
    ///
    /// This is the cheap, scoped half of a pass: on a merge, rebase, pull, or
    /// checkout it indexes exactly the files that commit range touched, and
    /// nothing else. A full [`index_all`](Self::index_all) is still the
    /// correctness backstop and still runs — see [`crate::reconcile`] for why
    /// git can only ever be an accelerator here — but running this first means
    /// the files a commit actually changed are re-indexed *before* the walk,
    /// so their `indexed_at` marks them as the newest work in the store and
    /// the embedding passes reach them first (`vectors::pending` orders on
    /// exactly that).
    ///
    /// Returns the [`Plan`](crate::reconcile::Plan) it acted on, so a caller
    /// can report what happened. Errors only on a store failure: every
    /// repository question that cannot be answered degrades to a full walk.
    pub fn reconcile_with_head(&self) -> Result<(reconcile::Plan, IndexStats), GraphError> {
        let oracle = reconcile::GitCli::new(&self.inner.root);
        self.reconcile_with(&oracle)
    }

    /// [`reconcile_with_head`](Self::reconcile_with_head) against an injected
    /// oracle — the seam that lets the wiring be tested without building a
    /// repository whose history has the shape under test.
    pub fn reconcile_with(
        &self,
        oracle: &dyn reconcile::RepoOracle,
    ) -> Result<(reconcile::Plan, IndexStats), GraphError> {
        reconcile_inner(&self.inner, oracle)
    }

    /// Run a full incremental index pass now (walk, re-parse only changed
    /// files, prune deleted). One transaction (L-L1). Synchronous — callers
    /// in an async context should wrap it in `spawn_blocking`.
    pub fn index_all(&self) -> Result<IndexStats, GraphError> {
        self.index_all_with_progress(&mut |_| {})
    }

    /// [`index_all`](Self::index_all) with a per-file progress callback
    /// (#3102): `progress` receives the running [`IndexStats`] after each
    /// file the pass visits, so a long build can be narrated while it
    /// happens instead of summarised after. The pass still runs as one
    /// transaction; the callback is display-only and cannot affect it.
    pub fn index_all_with_progress(
        &self,
        progress: &mut dyn FnMut(&IndexStats),
    ) -> Result<IndexStats, GraphError> {
        let mut conn = self.inner.write_guard();
        store::index_tree_with_progress(&mut conn, &self.inner.root, &self.inner.grammars, progress)
    }

    /// [`index_all`](Self::index_all), but skipped entirely when another pass
    /// is already walking this store (#3650). `None` means someone else holds
    /// the walk lease and this caller has nothing useful to add.
    ///
    /// This is for **opportunistic** walks — a session mounting, a graph tool
    /// opening the store — where a second concurrent walk produces identical
    /// rows and is pure waste. An explicit user command keeps calling
    /// [`index_all`](Self::index_all): someone who typed `stella init` is
    /// owed the pass, and answering "another process was busy" to a direct
    /// instruction is a worse failure than doing the work twice.
    pub fn index_all_single_flight(&self) -> Result<Option<IndexStats>, GraphError> {
        // The write connection, not the read one: taking a lease is a write,
        // and `store::open_read`'s whole contract is that the read path never
        // takes a write lock. Both holds are scoped so `index_all` can take
        // the same mutex between them.
        let lease = {
            let conn = self.inner.write_guard();
            match lease::acquire(&conn, lease::Purpose::IndexWalk) {
                lease::Acquired::Held(lease) => lease,
                lease::Acquired::Busy => return Ok(None),
            }
        };
        let outcome = self.index_all();
        // Released whether the pass succeeded or not: a failed walk holds no
        // claim on the next one, and leaving the lease behind would stall
        // indexing for the whole TTL over an error the caller already sees.
        {
            let conn = self.inner.write_guard();
            lease::release(&conn, &lease);
        }
        outcome.map(Some)
    }

    /// Take the single-flight lease for `purpose` (#3650), or `None` when
    /// another pass already holds it and this caller should stand down.
    ///
    /// The caller must [`release_lease`](Self::release_lease) when it is
    /// done — including on the failure path, since a lease left behind stalls
    /// the next pass for the whole TTL. Callers that own a pass end to end
    /// should prefer [`index_all_single_flight`](Self::index_all_single_flight),
    /// which handles both sides; this pair exists for a pass whose work lives
    /// in another crate, like the embedding passes in `stella-tools`.
    pub fn acquire_lease(&self, purpose: lease::Purpose) -> Option<lease::Lease> {
        let conn = self.inner.write_guard();
        match lease::acquire(&conn, purpose) {
            lease::Acquired::Held(lease) => Some(lease),
            lease::Acquired::Busy => None,
        }
    }

    /// Give up a lease taken by [`acquire_lease`](Self::acquire_lease).
    pub fn release_lease(&self, lease: &lease::Lease) {
        let conn = self.inner.write_guard();
        lease::release(&conn, lease);
    }

    /// Vectors stranded under fingerprints that are not `active` (#3652), as
    /// `(fingerprint, rows)` — see [`crate::vectors::retired_fingerprints`]
    /// for why these are reported rather than swept.
    pub fn retired_vector_fingerprints(
        &self,
        active: &str,
    ) -> Result<Vec<(String, usize)>, GraphError> {
        vectors::retired_fingerprints(&self.inner.read_guard(), active)
    }

    /// Delete every vector held under a retired `fingerprint`, returning the
    /// rows removed. Refuses to touch `active`.
    pub fn prune_vector_fingerprint(
        &self,
        fingerprint: &str,
        active: &str,
    ) -> Result<usize, GraphError> {
        vectors::prune_fingerprint(&mut self.inner.write_guard(), fingerprint, active)
    }

    /// Number of files currently in the index.
    pub fn file_count(&self) -> Result<usize, GraphError> {
        store::file_count(&self.inner.read_guard())
    }

    /// Total symbols across the whole index (the graph total, not a per-pass
    /// delta). Used by the startup summary line.
    pub fn symbol_count(&self) -> Result<usize, GraphError> {
        store::symbol_count(&self.inner.read_guard())
    }

    /// Total import edges across the whole index.
    pub fn import_count(&self) -> Result<usize, GraphError> {
        store::import_count(&self.inner.read_guard())
    }

    /// Frames for every definition of `name`.
    pub fn definitions(&self, name: &str) -> Result<Vec<ContextFrame>, GraphError> {
        frames::definitions(&self.inner.read_guard(), &self.inner.root, name)
    }

    /// Every definition site of `name` with its exact source span — the
    /// lookup behind `stella search`'s symbol enrichment, which needs the
    /// faithful `(path, start..=end)` range to read (a definition frame
    /// renders a truncated snippet, not an editable span).
    pub fn definition_spans(&self, name: &str) -> Result<Vec<SymbolSpan>, GraphError> {
        Ok(store::definitions(&self.inner.read_guard(), name)?
            .into_iter()
            .map(|row| SymbolSpan {
                path: row.path,
                name: row.name,
                kind: row.kind.keyword().to_string(),
                start_line: row.start_line,
                end_line: row.end_line,
            })
            .collect())
    }

    /// Best-effort textual reference frames for `name`.
    pub fn references(&self, name: &str) -> Result<Vec<ContextFrame>, GraphError> {
        frames::references(&self.inner.read_guard(), &self.inner.root, name)
    }

    /// One frame per definition of `name`, listing the call sites recorded
    /// inside that definition's line span (#335, B1). Honest but unresolved:
    /// real structural call sites, name-only — no receiver types, no
    /// cross-file identity.
    pub fn callees(&self, name: &str) -> Result<Vec<ContextFrame>, GraphError> {
        frames::callees(&self.inner.read_guard(), &self.inner.root, name)
    }

    /// Best-effort caller frames for `name`: recorded call sites whose
    /// callee name matches, labeled by their enclosing definition where the
    /// index holds one (#335, B1). A reverse *name* lookup — same-name
    /// methods conflate — scored in the same weakest band as
    /// [`CodeGraph::references`].
    pub fn callers(&self, name: &str) -> Result<Vec<ContextFrame>, GraphError> {
        frames::callers(&self.inner.read_guard(), &self.inner.root, name)
    }

    /// Total call sites across the whole index.
    pub fn call_count(&self) -> Result<usize, GraphError> {
        store::call_count(&self.inner.read_guard())
    }

    /// A frame describing the imports out of `file`.
    pub fn imports_of(&self, file: &Path) -> Result<Vec<ContextFrame>, GraphError> {
        let rel = self.resolve_rel(file);
        frames::imports_of(&self.inner.read_guard(), &self.inner.root, &rel)
    }

    /// A frame describing the files that import `file`.
    pub fn importers_of(&self, file: &Path) -> Result<Vec<ContextFrame>, GraphError> {
        let rel = self.resolve_rel(file);
        frames::importers_of(&self.inner.read_guard(), &self.inner.root, &rel)
    }

    /// The files whose imports resolve to `file` — raw root-relative
    /// forward-slash paths, not rendered frames. The reverse-dependency
    /// lookup for impacted-scope selection: a caller walks this relation
    /// transitively and needs the plain path list per hop rather than a
    /// prose frame.
    pub fn importer_paths(&self, file: &Path) -> Result<Vec<String>, GraphError> {
        let rel = self.resolve_rel(file);
        store::importers_of(&self.inner.read_guard(), &rel)
    }

    /// The immediate graph neighborhood of `file` (its symbols + edges).
    pub fn neighbors(&self, file: &Path) -> Result<Vec<ContextFrame>, GraphError> {
        let rel = self.resolve_rel(file);
        frames::neighbors(&self.inner.read_guard(), &self.inner.root, &rel)
    }

    /// The CGP-provider query entrypoint: budgeted, provenance-carrying
    /// frames, assembled in-process. Consumed at runtime by the CLI's CGP
    /// host (`stella-cli/src/contextgraph.rs`, `GraphProvider`), which fans
    /// recall out to this alongside the memory store on every turn.
    pub fn query(&self, q: &ContextQuery) -> Result<Vec<ContextFrame>, GraphError> {
        frames::query(&self.inner.read_guard(), &self.inner.root, q)
    }

    /// The best-connected file in the index (most symbols + import edges) —
    /// a UI's default focus when the caller hasn't picked a file. `None` on
    /// an empty index.
    pub fn busiest_file(&self) -> Result<Option<String>, GraphError> {
        store::busiest_file(&self.inner.read_guard())
    }

    /// Up to `limit` files nothing imports — binaries, scripts, tests, dead
    /// code: exactly the set worth reading first when orienting in an
    /// unfamiliar tree. Computed as one SQL anti-join, so the cost is
    /// independent of index size — callers need no file-count cap, unlike
    /// the per-file [`importers_of`] scan this replaces. Shallowest path
    /// first, then lexicographic; empty on an empty index.
    ///
    /// [`importers_of`]: CodeGraph::importers_of
    pub fn entry_points(&self, limit: usize) -> Result<Vec<String>, GraphError> {
        store::entry_points(&self.inner.read_guard(), limit)
    }

    /// Every indexed file path (root-relative, forward-slash), sorted. The
    /// deck's Graph tab lists these in its file picker so a user can re-root
    /// the neighborhood on any file, not only the [`busiest_file`] default.
    /// Empty on an empty index.
    ///
    /// [`busiest_file`]: CodeGraph::busiest_file
    pub fn all_files(&self) -> Result<Vec<String>, GraphError> {
        store::all_files(&self.inner.read_guard())
    }

    /// Up to `limit` indexed files with no current vector under `fingerprint`,
    /// rendered and ready to embed, plus how many unreadable files the scan
    /// stepped over to find them.
    ///
    /// An empty [`files`] means the pending set is exhausted, not merely that
    /// this window happened to land on unreadable rows — which is what lets a
    /// caller loop on it until it comes back empty.
    ///
    /// The embedding itself is deliberately **not** here: producing a vector
    /// is I/O against a model, and this crate holds no transport and no key.
    /// A caller pairs this with [`store_file_vectors`] around whatever
    /// [`stella_embed::Embedder`] it resolved, which is what keeps the network
    /// out of the indexer (invariant 1).
    ///
    /// [`store_file_vectors`]: CodeGraph::store_file_vectors
    /// [`files`]: vectors::PendingScan::files
    pub fn files_pending_embedding(
        &self,
        fingerprint: &str,
        limit: usize,
    ) -> Result<vectors::PendingScan, GraphError> {
        vectors::pending(
            &self.inner.read_guard(),
            &self.inner.root,
            fingerprint,
            limit,
        )
    }

    /// Persist vectors under `fingerprint`. Returns how many rows were
    /// written — a path the index has since dropped is skipped, not an error.
    pub fn store_file_vectors(
        &self,
        fingerprint: &str,
        rows: &[vectors::FileVector],
    ) -> Result<usize, GraphError> {
        vectors::store_vectors(&mut self.inner.write_guard(), fingerprint, rows)
    }

    /// Rank indexed files against a query vector, best first.
    ///
    /// `floor` drops candidates scoring below it; pass
    /// [`f32::NEG_INFINITY`] to keep every one. Only vectors stamped with
    /// `fingerprint` are considered, so a stored vector from a different
    /// embedder is invisible rather than silently comparable.
    pub fn rank_files_by_vector(
        &self,
        fingerprint: &str,
        query: &[f32],
        floor: f32,
        limit: usize,
    ) -> Result<Vec<stella_embed::rank::Scored>, GraphError> {
        vectors::rank(&self.inner.read_guard(), fingerprint, query, floor, limit)
    }

    /// How many files carry a vector under `fingerprint`. With
    /// [`file_count`] this is the honest "how much of this workspace can a
    /// semantic query see" answer a caller shows when a pass was capped.
    ///
    /// [`file_count`]: CodeGraph::file_count
    pub fn embedded_file_count(&self, fingerprint: &str) -> Result<usize, GraphError> {
        vectors::count(&self.inner.read_guard(), fingerprint)
    }

    /// Files whose **chunks** — symbols, markdown sections — are not fully
    /// embedded under `fingerprint`, rendered and hashed ready to embed
    /// (#3089).
    ///
    /// The embedding itself is deliberately not here, for the reason
    /// [`files_pending_embedding`] gives: producing a vector is I/O against a
    /// model, and this crate holds no transport and no key.
    ///
    /// [`files_pending_embedding`]: CodeGraph::files_pending_embedding
    pub fn chunks_pending_embedding(
        &self,
        fingerprint: &str,
        limit: usize,
    ) -> Result<vectors::chunks::PendingChunkScan, GraphError> {
        vectors::chunks::pending_chunks(
            &self.inner.read_guard(),
            &self.inner.root,
            fingerprint,
            limit,
        )
    }

    /// Persist one file's chunk vectors and sweep the chunks it no longer has.
    ///
    /// **One file per call**, because the sweep keys on `file_sha256`: writing
    /// half a file's chunks and then the other half would have the second call
    /// delete the first call's rows.
    pub fn store_chunk_vectors(
        &self,
        fingerprint: &str,
        path: &str,
        file_sha256: &str,
        chunks: &[vectors::chunks::ChunkVector],
    ) -> Result<usize, GraphError> {
        vectors::chunks::store_chunk_vectors(
            &mut self.inner.write_guard(),
            fingerprint,
            path,
            file_sha256,
            chunks,
        )
    }

    /// Rank indexed chunks against a query vector, best first.
    ///
    /// Same contract as [`rank_files_by_vector`] — `floor` drops candidates
    /// below it, and only vectors stamped with `fingerprint` are considered —
    /// over a corpus roughly twenty times larger and correspondingly sharper.
    ///
    /// [`rank_files_by_vector`]: CodeGraph::rank_files_by_vector
    pub fn rank_chunks_by_vector(
        &self,
        fingerprint: &str,
        query: &[f32],
        floor: f32,
        limit: usize,
    ) -> Result<Vec<vectors::chunks::ScoredChunk>, GraphError> {
        vectors::chunks::rank_chunks(&self.inner.read_guard(), fingerprint, query, floor, limit)
    }

    /// How many chunks carry a vector under `fingerprint`. With
    /// [`symbol_count`] this is the chunk-level answer to "how much of this
    /// workspace can a semantic query see".
    ///
    /// [`symbol_count`]: CodeGraph::symbol_count
    pub fn embedded_chunk_count(&self, fingerprint: &str) -> Result<usize, GraphError> {
        vectors::chunks::chunk_count(&self.inner.read_guard(), fingerprint)
    }

    /// How many indexed files still carry a symbol with no chunk vector under
    /// `fingerprint`. What an eager chunk-embedding pass reports as
    /// `remaining` — cheap on purpose, since it is checked after every pass
    /// without re-rendering or re-hashing anything.
    pub fn pending_chunk_file_count(&self, fingerprint: &str) -> Result<usize, GraphError> {
        vectors::chunks::pending_chunk_file_count(&self.inner.read_guard(), fingerprint)
    }

    /// The structured neighborhood of `file` — its symbols and import edges
    /// in both directions — for UI consumers (the deck's Graph tab). The
    /// frame methods above render prose for the model; this keeps the shape.
    pub fn file_neighborhood(&self, file: &Path) -> Result<FileNeighborhood, GraphError> {
        let rel = self.resolve_rel(file);
        let conn = self.inner.read_guard();
        let symbols = store::symbols_in_file(&conn, &rel)?
            .into_iter()
            .map(|row| NeighborhoodSymbol {
                name: row.name,
                kind: row.kind.tag().to_string(),
                start_line: row.start_line,
            })
            .collect();
        let imports = store::imports_from(&conn, &rel)?
            .into_iter()
            .map(|row| row.to_path.unwrap_or(row.specifier))
            .collect();
        let importers = store::importers_of(&conn, &rel)?;
        Ok(FileNeighborhood {
            file: rel,
            symbols,
            imports,
            importers,
        })
    }

    /// The assembled storage map: parsed structure from the index merged
    /// with `stella.storage.toml` meaning (spec §6). Best-effort — an
    /// unreadable store or malformed manifest yields whatever half works,
    /// never an error, matching [`CodeGraph::schema_names`]' posture.
    pub fn storage_snapshot(&self) -> crate::storage::StorageSnapshot {
        let rows = store::storage_rows(&self.inner.read_guard()).unwrap_or_default();
        // Still best-effort, but the reason a malformed manifest contributed
        // nothing rides along in `manifest_error` rather than being dropped.
        let (manifest, manifest_error) =
            match crate::manifest::StorageManifest::load(&self.inner.root) {
                Ok(manifest) => (manifest, None),
                Err(error) => (None, Some(error.to_string())),
            };
        let mut snapshot = crate::manifest::merge_snapshot(rows, manifest.as_ref());
        snapshot.manifest_error = manifest_error;
        snapshot
    }

    /// All known table, type, and view names (lowercased) from the index.
    /// Used by the schema gate to populate the known-schema set at session
    /// start. Returns empty sets if the index is empty or unreadable.
    pub fn schema_names(&self) -> (HashSet<String>, HashSet<String>, HashSet<String>) {
        let conn = self.inner.read_guard();
        let tables = store::names_of_kind(&conn, "table").unwrap_or_default();
        let types = store::names_of_kind(&conn, "schema_enum").unwrap_or_default();
        let views = store::names_of_kind(&conn, "view").unwrap_or_default();
        (tables, types, views)
    }

    /// Stop the watcher and background tasks. Idempotent. Dropping the watcher
    /// closes the event channel, so the debounce loop exits on its own; task
    /// handles are aborted as a backstop.
    pub fn shutdown(&self) {
        // Set the shutdown flag *and* clear the watcher slot under one hold of
        // the watcher lock. `set_watcher` re-checks the flag under this same
        // lock, so install and shutdown serialize: a watcher installed
        // concurrently by the mount background task is either cleared here (if
        // it stored first) or dropped by `set_watcher` (if it runs after us).
        // Invariant: after this returns, no watcher can be (re)installed.
        {
            let mut watcher = self
                .inner
                .watcher
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.inner.shutdown.store(true, Ordering::Relaxed);
            *watcher = None;
        }
        let handles: Vec<JoinHandle<()>> = self
            .inner
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain(..)
            .collect();
        for handle in handles {
            handle.abort();
        }
    }

    /// Resolve a caller-supplied path (absolute or already root-relative) to a
    /// forward-slash path relative to the workspace root.
    fn resolve_rel(&self, path: &Path) -> String {
        let root = &self.inner.root;
        if let Ok(rel) = path.strip_prefix(root) {
            return import::rel_to_slash(rel);
        }
        if let Ok(canonical) = path.canonicalize()
            && let Ok(rel) = canonical.strip_prefix(root)
        {
            return import::rel_to_slash(rel);
        }
        import::rel_to_slash(path)
    }
}

/// A whole-tree walk that yields to any pass already doing one (#3650).
///
/// Over `&Inner` for the same reason [`reconcile_inner`] is — see its doc.
/// `Ok(None)` means another pass holds the walk lease.
fn walk_single_flight(inner: &Inner) -> Result<Option<IndexStats>, GraphError> {
    let lease = {
        let conn = inner.write_guard();
        match lease::acquire(&conn, lease::Purpose::IndexWalk) {
            lease::Acquired::Held(lease) => lease,
            lease::Acquired::Busy => return Ok(None),
        }
    };
    let outcome = {
        let mut conn = inner.write_guard();
        store::index_tree(&mut conn, &inner.root, &inner.grammars)
    };
    {
        let conn = inner.write_guard();
        lease::release(&conn, &lease);
    }
    outcome.map(Some)
}

/// The reconciliation pass, over the shared [`Inner`] rather than over a
/// [`CodeGraph`] handle.
///
/// It takes `&Inner` for a specific reason, and the shape is load-bearing:
/// [`CodeGraph`]'s `Drop` calls `shutdown`, on the documented assumption that
/// a handle going out of scope is the *last* public handle. Fabricating a
/// second `CodeGraph` from a cloned `Arc<Inner>` — which is what the mount
/// task would otherwise have to do to call a method — therefore tears the
/// whole graph down when that temporary drops: the shutdown flag latches, and
/// `set_watcher` then refuses to install the live watcher for the rest of the
/// session. A free function over `&Inner` cannot make that mistake.
pub(crate) fn reconcile_inner(
    inner: &Inner,
    oracle: &dyn reconcile::RepoOracle,
) -> Result<(reconcile::Plan, IndexStats), GraphError> {
    let mut conn = inner.write_guard();
    let stored = reconcile::read_head(&conn)?;
    let plan = reconcile::plan(stored.as_deref(), oracle);

    let paths = reconcile::absolute_priority_paths(&inner.root, &plan);
    let stats = if paths.is_empty() {
        IndexStats::default()
    } else {
        store::apply_changes(&mut conn, &inner.root, &inner.grammars, &paths)?
    };

    // Recorded only after the work committed. Stamping first and dying
    // mid-pass would leave the store claiming a commit it never indexed, and
    // the next reconciliation would see an unmoved HEAD and scope nothing —
    // the one way this design loses a file permanently rather than merely
    // late.
    if let Some(head) = plan.commit_to_record() {
        reconcile::record_head(&conn, head)?;
    }
    Ok((plan, stats))
}

impl Drop for CodeGraph {
    fn drop(&mut self) {
        // Best-effort teardown if the caller never called `shutdown`.
        // Unconditional: `CodeGraph` is not `Clone`, so this drop IS the last
        // public handle going away — the remaining `Arc` holders are the
        // background catch-up task and debounce loop, which hold clones for
        // their whole lives. Gating on strong-count == 1 (the previous
        // behavior) could therefore never fire after a successful `mount`,
        // leaking the OS watcher and both loops until process exit.
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A parked `notify` watcher, not armed on any path — enough to exercise
    /// the install path without depending on OS event delivery.
    fn make_watcher() -> RecommendedWatcher {
        notify::recommended_watcher(|_res: notify::Result<notify::Event>| {}).unwrap()
    }

    fn open_graph() -> (CodeGraph, TempDir, TempDir) {
        let ws = TempDir::new().unwrap();
        let dbdir = TempDir::new().unwrap();
        let graph = CodeGraph::open(ws.path(), &dbdir.path().join("context.db")).unwrap();
        (graph, ws, dbdir)
    }

    fn watcher_is_installed(graph: &CodeGraph) -> bool {
        graph
            .inner
            .watcher
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    /// #803: the reference scan visits a bounded corpus — a miss costs a
    /// bounded read, never a full synchronous sweep of every indexed file.
    #[test]
    fn reference_scan_visits_a_bounded_corpus() {
        let ws = TempDir::new().unwrap();
        let dbdir = TempDir::new().unwrap();
        for name in ["a.rs", "b.rs", "c.rs"] {
            std::fs::write(
                ws.path().join(name),
                "pub fn uses() { bounded_needle(); }\n",
            )
            .unwrap();
        }
        let graph = CodeGraph::open(ws.path(), &dbdir.path().join("context.db")).unwrap();
        graph.index_all().unwrap();

        let conn = graph.inner.read_guard();
        let frames =
            crate::frames::references_bounded(&conn, ws.path(), "bounded_needle", 2).unwrap();
        let mut files: Vec<&str> = frames
            .iter()
            .filter_map(|f| f.id.split(':').nth(2))
            .collect();
        files.sort();
        files.dedup();
        assert_eq!(
            files.len(),
            2,
            "only the first two indexed files may be visited: {files:?}"
        );
    }

    /// Reproduces the mount-vs-shutdown race deterministically: `shutdown()`
    /// runs first (flag set, slot cleared), then the racing mount task — having
    /// already passed its pre-install shutdown check — tries to install its
    /// watcher. The guarded `set_watcher` must drop it, not store it, so the
    /// OS watcher cannot leak past shutdown.
    #[test]
    fn set_watcher_after_shutdown_drops_the_watcher() {
        let (graph, _ws, _dbdir) = open_graph();

        graph.shutdown();
        graph.inner.set_watcher(make_watcher());

        assert!(
            !watcher_is_installed(&graph),
            "a watcher installed after shutdown must be dropped, not retained"
        );
    }

    /// Control: with no shutdown, the normal install path stores the watcher.
    #[test]
    fn set_watcher_before_shutdown_stores_the_watcher() {
        let (graph, _ws, _dbdir) = open_graph();

        graph.inner.set_watcher(make_watcher());
        assert!(
            watcher_is_installed(&graph),
            "a watcher installed before shutdown must be retained"
        );

        // And a subsequent shutdown clears it back out.
        graph.shutdown();
        assert!(
            !watcher_is_installed(&graph),
            "shutdown must clear an already-installed watcher"
        );
    }

    /// `all_files` surfaces every indexed file (root-relative, sorted) so the
    /// deck's Graph tab can list them — the file picker's data source.
    #[test]
    fn all_files_lists_every_indexed_file_sorted() {
        let ws = TempDir::new().unwrap();
        let dbdir = TempDir::new().unwrap();
        std::fs::write(ws.path().join("zeta.rs"), "pub fn z() {}\n").unwrap();
        std::fs::write(ws.path().join("alpha.rs"), "pub fn a() {}\n").unwrap();
        let graph = CodeGraph::open(ws.path(), &dbdir.path().join("context.db")).unwrap();
        graph.index_all().unwrap();

        assert_eq!(
            graph.all_files().unwrap(),
            vec!["alpha.rs".to_string(), "zeta.rs".to_string()],
            "every indexed file, root-relative and sorted"
        );
    }

    #[test]
    fn all_files_is_empty_on_an_empty_index() {
        let (graph, _ws, _dbdir) = open_graph();
        assert!(graph.all_files().unwrap().is_empty());
    }

    /// `importer_paths` answers the raw reverse-dependency question — which
    /// files' imports resolve to this one — as plain root-relative paths,
    /// the same relation the recall plane's importer frames are built from.
    #[test]
    fn importer_paths_lists_files_whose_imports_resolve_here() {
        let ws = TempDir::new().unwrap();
        let dbdir = TempDir::new().unwrap();
        std::fs::create_dir_all(ws.path().join("src")).unwrap();
        std::fs::write(ws.path().join("src/x.ts"), "export const x = 1;\n").unwrap();
        std::fs::write(
            ws.path().join("a.test.ts"),
            "import { x } from './src/x';\n",
        )
        .unwrap();
        std::fs::write(ws.path().join("b.test.ts"), "export const b = 2;\n").unwrap();
        let graph = CodeGraph::open(ws.path(), &dbdir.path().join("context.db")).unwrap();
        graph.index_all().unwrap();

        assert_eq!(
            graph.importer_paths(Path::new("src/x.ts")).unwrap(),
            vec!["a.test.ts".to_string()],
            "only the file whose import resolves to src/x.ts"
        );
        assert!(
            graph
                .importer_paths(Path::new("b.test.ts"))
                .unwrap()
                .is_empty(),
            "nothing imports b.test.ts"
        );
    }

    /// `definition_spans` reports each site's exact 1-based inclusive range —
    /// the raw span `read_symbol` reads, not a rendered/truncated snippet.
    #[test]
    fn definition_spans_carry_the_exact_source_range() {
        let ws = TempDir::new().unwrap();
        let dbdir = TempDir::new().unwrap();
        std::fs::write(
            ws.path().join("lib.rs"),
            "fn alpha() {}\n\nfn target() {\n    let x = 1;\n    let y = 2;\n}\n",
        )
        .unwrap();
        let graph = CodeGraph::open(ws.path(), &dbdir.path().join("context.db")).unwrap();
        graph.index_all().unwrap();

        let spans = graph.definition_spans("target").unwrap();
        assert_eq!(spans.len(), 1, "{spans:?}");
        let span = &spans[0];
        assert_eq!(span.path, "lib.rs");
        assert_eq!(span.name, "target");
        assert_eq!(span.kind, "fn");
        assert_eq!(
            (span.start_line, span.end_line),
            (3, 6),
            "1-based inclusive span of the whole definition"
        );
        assert!(graph.definition_spans("no_such_symbol").unwrap().is_empty());
    }
}
