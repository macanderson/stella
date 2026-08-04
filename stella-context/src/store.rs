//! [`ContextStore`] — the one SQLite file, one engine that backs the context
//! plane (arch §3: SQLite everywhere, so there is one WAL, one backup story,
//! and one file format to reason about). It holds the bi-temporal property graph
//! (`node` + `edge`), the fingerprinted embedding index (`embedding`),
//! episodic memory (`episode`), and the embedder-fingerprint registry.
//!
//! Crash consistency (`L-L1`): every write batch is one transaction, so a
//! kill mid-index rolls back cleanly and reopening finds a consistent store
//! with no partial rows. Warming (`L-C1`): [`ContextStore::open_and_warm`]
//! kicks embedding catch-up as a background task at mount instead of paying it
//! lazily on the first real query.

mod ab_control;
mod compact;
mod domain;
mod edge;
mod embedding;
pub mod ledger;
mod node;
mod record;
mod schema;

// `pub(crate)` only so `crate::ann`'s migration test can reuse `open_legacy`
// rather than hand-rolling a second, subtly different v6 fixture — two fixture
// builders for one schema ladder is how a migration test ends up asserting
// against a shape the ladder never produces.
#[cfg(test)]
mod edge_id_tests;
#[cfg(test)]
pub(crate) mod tests;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::clock::{Clock, SystemClock};
use crate::embed::{Embedder, EmbedderFingerprint, HashEmbedder};
use crate::error::ContextError;

use schema::{migrate, register_fingerprint};

// The module was one 2,232-line file until #712 split it along the seams it
// already had. Everything below stays reachable as `crate::store::X`, which is
// the path every consumer uses, so the split is invisible outside this file.
pub use compact::{CompactionWatermark, ContextCompactPolicy, ContextCompactReport};
pub(crate) use domain::{
    domains_by_node, list_domains, node_ids_excluded_by_scope, tag_edge_domains, tag_node_domains,
    upsert_domain,
};
pub(crate) use edge::{
    close_edge, edges_as_of, end_world_validity, insert_edge, neighbors_valid_at, open_anchors,
};
pub(crate) use embedding::{
    blob_to_vector, embedding_exists, nodes_missing_embedding, store_embedding, vector_to_blob,
};
pub use node::{NodeInput, NodeKind, NodeRow};
pub(crate) use node::{
    map_node_row, node_by_id, node_exists_any_state, node_ids_for_uris, restore_node,
    supersede_node, upsert_node,
};
pub use record::MemoryRevision;
pub(crate) use record::{insert_episode, insert_memory, memory_kind, memory_revisions};
pub(crate) use schema::open_connection;
// Only a migration test asserts against it; a release build has no reader.
#[cfg(test)]
pub(crate) use schema::SCHEMA_VERSION;

/// A count of what the memory lineage layer holds (#712 deliverable 5).
///
/// `lineages == live` on a healthy store: every lineage has exactly one current
/// revision. `revisions - live` is history — the text memories used to carry,
/// kept because supersession never deletes (`L-C3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryLineageStats {
    /// Distinct memory lineages — the number of *memories*, as a user counts.
    pub lineages: usize,
    /// Rows across every lineage, current and historical.
    pub revisions: usize,
    /// Revisions that are current. One per lineage.
    pub live: usize,
    /// Revisions an edit closed. Readable, never recalled.
    pub superseded: usize,
}

/// The context plane's storage handle. Not `Clone` — the background warm
/// handle is owned by exactly one store so [`ContextStore::await_warm`] has a
/// single joiner. Share it with `Arc<ContextStore>`, which is what every
/// consumer does; the connection and embedder inside are already `Arc`s, so
/// nothing is duplicated by doing so.
///
/// # Retention
///
/// This plane forgets *and*, since [`Self::compact`], reclaims — but the two are
/// different mechanisms with different guarantees, and conflating them is how
/// `L-C3` gets broken.
///
/// **Forgetting** is [`Self::supersede_node`]: a tombstone, never a delete, with
/// [`Self::restore_node`] as its exact inverse. Nothing a point-in-time query
/// can see is affected.
///
/// **Reclaiming** is [`Self::compact`], and it touches only *derived index
/// entries whose owner is already gone*: embeddings no node in any state and no
/// live memory points at (every memory edit strands one), orphaned
/// `node_domains`/`edge_domains` rows, and — opt-in — vectors under an embedder
/// recall is forbidden to read. `edge` rows, `memory` revisions, and superseded
/// `node` rows are named exclusions; see the `store::compact` module docs for
/// each one's reason. `L-C3` is a guarantee about *queryability*, not bytes: a
/// row whose deletion cannot change the answer to "what did we believe at T" is
/// not what it protects. The authority for treating the index as disposable is
/// `docs/adr/0010-incremental-authority-transfer.md` decision point 6.
///
/// What compaction does **not** bound is the live row count. A node whose uri
/// changed — a renamed or deleted file — is still orphaned live forever, serving
/// its last-known content until something supersedes it, so the set a recall
/// *considers* still grows with the workspace's lifetime.
///
/// What that costs is bounded by the query rather than by that row count:
///
/// - The fused candidate list is cut to a multiple of the query's `max_frames`
///   before the MMR pass and before any frame is built
///   (`RecallTuning::mmr_candidate_multiple`, default
///   `retrieval::DEFAULT_MMR_CANDIDATE_MULTIPLE`), so the diversity fold is quadratic in
///   the budget, not in lifetime memory size.
/// - **No content body crosses the SQLite boundary for a non-candidate.** The
///   corpus-wide passes read a metadata projection — identity, time, hash, and
///   content *size* — and bodies are fetched by id after the cut.
/// - **No embedding vector is decoded for a non-candidate.** The similarity pass
///   scores each vector off its stored BLOB and keeps only `(id, cosine)`; the
///   candidates' vectors are re-read by id.
///
/// What remains linear in live rows is one metadata scan, one indexed vector
/// scan (`idx_embedding_fingerprint`), and — only on a weak-coverage turn — the
/// lexical scan, which streams instead of materializing the corpus. All of it
/// runs on the blocking pool rather than on the worker that awaited it, so the
/// timeouts callers wrap around recall can actually fire.
///
/// The vector scan is the one of those three that a workspace can now opt out
/// of: [`Self::index_ann`] builds an IVF index and
/// `context.retrieval.ann_enabled` lets recall probe it instead, which makes
/// that pass `O(√n)` rather than `O(n)`. It is off by default because it is
/// approximate — it changes which frames a turn can recall, not merely how fast
/// it recalls them — and every recall it serves says so on
/// `RecallResult::used_ann_index`. The metadata scan and the lexical fallback
/// remain linear and remain exact.
///
/// # Drop
///
/// Dropping the store **stops its background warm task** (#613): `Drop` raises
/// the cancel flag the batch loop checks and aborts the join handle. Warming is
/// no longer detached-until-done — if you need it finished, call
/// [`Self::await_warm`] first. Committed batches are unaffected (each is its own
/// transaction) and the remainder is caught up at the next mount, so the only
/// difference is that work for a store nobody holds stops. A caller that already
/// joined sees nothing: `await_warm` took the handle, so `Drop` finds none. See
/// the crate-private `warm` module for why this is a flag plus an abort, never
/// a spawn.
pub struct ContextStore {
    /// The DB path, kept so warming can open its own WAL connection.
    path: PathBuf,
    /// `Arc<Mutex<..>>` restores `Sync` (a bare `Connection` is `Send` only)
    /// so the store can implement the `Send + Sync` provider trait. All
    /// SQLite work happens inside the lock with no `await` held.
    conn: Arc<Mutex<Connection>>,
    embedder: Arc<dyn Embedder>,
    fingerprint: EmbedderFingerprint,
    clock: Arc<dyn Clock>,
    /// The background warm task, joinable via `await_warm`.
    warm: Mutex<Option<tokio::task::JoinHandle<Result<usize, ContextError>>>>,
    /// Raised by `Drop`, checked between warm batches.
    warm_cancel: crate::warm::WarmCancel,
    /// The retrieval knobs this store recalls with. Defaults to exactly the
    /// values that shipped as `const`s, so a host that configures nothing
    /// behaves identically (#712 deliverable 8).
    tuning: crate::retrieval::RecallTuning,
}

impl Drop for ContextStore {
    fn drop(&mut self) {
        self.warm_cancel.cancel();
        // `get_mut` rather than `lock`: `&mut self` already proves exclusive
        // access, so there is no lock to contend or poison here.
        if let Some(handle) = self
            .warm
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            handle.abort();
        }
    }
}

impl ContextStore {
    /// Open (creating if absent) the store at `path` with the default
    /// [`HashEmbedder`] and system clock. Runs migrations and registers the
    /// embedder fingerprint. Does **not** warm — see [`Self::open_and_warm`].
    ///
    /// **Opening is a write.** Creating the file, replaying migrations and
    /// registering the fingerprint all happen here, so there is no read-only
    /// open: an inspection-only surface (`stella stats`, the command deck)
    /// still dirties the db, and a read-only mount fails at `open` rather than
    /// degrading to "no hits".
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ContextError> {
        Self::open_with(
            path,
            Arc::new(HashEmbedder::default()),
            Arc::new(SystemClock),
        )
    }

    /// Open with an explicit embedder and clock (the injectable form used by
    /// tests and by callers that pin a specific embedder/time source).
    pub fn open_with(
        path: impl AsRef<Path>,
        embedder: Arc<dyn Embedder>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ContextError> {
        let path = path.as_ref().to_path_buf();
        let conn = open_connection(&path)?;
        migrate(&conn)?;
        let fingerprint = embedder.fingerprint();
        register_fingerprint(&conn, &fingerprint, &clock.now_rfc3339())?;
        Ok(Self {
            path,
            conn: Arc::new(Mutex::new(conn)),
            embedder,
            fingerprint,
            clock,
            warm: Mutex::new(None),
            warm_cancel: crate::warm::WarmCancel::default(),
            tuning: crate::retrieval::RecallTuning::default(),
        })
    }

    /// Apply retrieval tuning, replacing the defaults.
    ///
    /// A builder rather than a constructor argument because every existing
    /// caller wants the defaults, and because the values reach here from a
    /// settings file — a surface that must not be able to make a store
    /// unopenable. Out-of-range knobs are clamped, never rejected: failing a
    /// turn over a typo in a tuning value is a worse answer than ignoring it.
    #[must_use]
    pub fn with_tuning(mut self, tuning: crate::retrieval::RecallTuning) -> Self {
        self.tuning = tuning.sanitized();
        self
    }

    /// The retrieval knobs in force for this store.
    pub(crate) fn tuning(&self) -> crate::retrieval::RecallTuning {
        self.tuning
    }

    // ── The lifecycle ledger (#714) ──────────────────────────────────────
    //
    // Append-only, immutable, hashed records — adaptive-context spec §6.3,
    // born canonical per ADR 0010. These are thin, synchronous wrappers: the
    // ledger is written on the reflection path, which must never block or fail
    // a user's turn, so there is nothing async to await and nothing to batch.

    /// Append one lifecycle record. Idempotent for a byte-identical replay;
    /// a byte-identical repeat is a success, not an error: extraction replays,
    /// and it cannot be exactly-once across a crash between the record write
    /// and its cursor advance.
    pub fn append_record(
        &self,
        record: ledger::LedgerAppend<'_>,
    ) -> Result<ledger::AppendOutcome, ContextError> {
        let now = self.clock.now_rfc3339();
        ledger::append(&lock(&self.conn), record, &now)
    }

    /// One lifecycle record by its revision id.
    pub fn record_by_id(
        &self,
        record_id: &str,
    ) -> Result<Option<ledger::LedgerRecord>, ContextError> {
        ledger::record_by_id(&lock(&self.conn), record_id)
    }

    /// Lifecycle records of one kind, oldest first.
    pub fn records_of_kind(
        &self,
        record_kind: &str,
        limit: usize,
    ) -> Result<Vec<ledger::LedgerRecord>, ContextError> {
        ledger::records_of_kind(&lock(&self.conn), record_kind, limit)
    }

    /// Lifecycle records of one kind in the order the ledger accepted them —
    /// the authority for "what happened last", which `observed_at` is not.
    pub fn records_of_kind_in_append_order(
        &self,
        record_kind: &str,
        limit: usize,
    ) -> Result<Vec<ledger::LedgerRecord>, ContextError> {
        ledger::records_of_kind_in_append_order(&lock(&self.conn), record_kind, limit)
    }

    /// The newest `limit` records of one kind, oldest-first — the recency
    /// window a "current state" fold needs. Unlike [`Self::records_of_kind`]
    /// (the oldest `limit`), this never freezes as the append-only log grows
    /// past the bound.
    pub fn records_of_kind_newest(
        &self,
        record_kind: &str,
        limit: usize,
    ) -> Result<Vec<ledger::LedgerRecord>, ContextError> {
        ledger::records_of_kind_newest(&lock(&self.conn), record_kind, limit)
    }

    /// The newest `limit` records of one kind in append order — the recency
    /// counterpart of [`Self::records_of_kind_in_append_order`], for a
    /// last-write-wins fold that must not freeze at the oldest `limit`.
    pub fn records_of_kind_newest_in_append_order(
        &self,
        record_kind: &str,
        limit: usize,
    ) -> Result<Vec<ledger::LedgerRecord>, ContextError> {
        ledger::records_of_kind_newest_in_append_order(&lock(&self.conn), record_kind, limit)
    }

    /// Every revision in one lineage, oldest first — a single record's audit
    /// trail.
    pub fn records_for_lineage(
        &self,
        lineage_id: &str,
    ) -> Result<Vec<ledger::LedgerRecord>, ContextError> {
        ledger::records_for_lineage(&lock(&self.conn), lineage_id)
    }

    /// How many records of each kind the ledger holds.
    pub fn record_counts(&self) -> Result<Vec<(String, u64)>, ContextError> {
        ledger::record_counts(&lock(&self.conn))
    }

    /// How far `source` has been consumed by observation extraction, if ever.
    pub fn extraction_cursor(&self, source: &str) -> Result<Option<String>, ContextError> {
        ledger::extraction_cursor(&lock(&self.conn), source)
    }

    /// Advance `source`'s extraction cursor.
    pub fn set_extraction_cursor(&self, source: &str, position: &str) -> Result<(), ContextError> {
        let now = self.clock.now_rfc3339();
        ledger::set_extraction_cursor(&lock(&self.conn), source, position, &now)
    }

    /// Claim this turn's number in `experiment`'s durable schedule, returning
    /// the 1-based count of turns that have consulted it in this workspace
    /// (#1221; the `store::ab_control` module holds the why).
    ///
    /// Every caller is a *claim*: the number is consumed by the caller that
    /// receives it, across processes as well as within one, which is what lets
    /// one-turn-per-process surfaces take part in a schedule at all. Use
    /// [`Self::ab_control_turns_so_far`] to read the count without moving it.
    pub fn next_ab_control_turn(&self, experiment: &str) -> Result<u64, ContextError> {
        let now = self.clock.now_rfc3339();
        ab_control::next_turn(&lock(&self.conn), experiment, &now)
    }

    /// How many turns `experiment` has counted in this workspace, without
    /// claiming one. `0` when it has never run here.
    pub fn ab_control_turns_so_far(&self, experiment: &str) -> Result<u64, ContextError> {
        ab_control::turns_so_far(&lock(&self.conn), experiment)
    }

    /// Open and immediately kick embedding catch-up as a background tokio task
    /// (`L-C1`: warm at mount, don't pay indexing on the first prompt). Must be
    /// called inside a tokio runtime; if none is running the store is returned
    /// un-warmed (catch-up can still be driven explicitly via [`Self::warm_now`]).
    /// Join the task with [`Self::await_warm`].
    pub fn open_and_warm(
        path: impl AsRef<Path>,
        embedder: Arc<dyn Embedder>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ContextError> {
        let store = Self::open_with(path, embedder, clock)?;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let path = store.path.clone();
            let embedder = store.embedder.clone();
            let fingerprint = store.fingerprint.id();
            let clock = store.clock.clone();
            let cancel = store.warm_cancel.clone();
            let task = handle.spawn(async move {
                crate::warm::warm_index(path, embedder, fingerprint, clock, cancel).await
            });
            *lock(&store.warm) = Some(task);
        }
        Ok(store)
    }

    /// Join the background warm task if one was spawned, returning the number
    /// of vectors it computed.
    ///
    /// The handle is taken, so this is join-once: a second call returns `Ok(0)`
    /// — the same answer as "warming never started" (no runtime at
    /// [`Self::open_and_warm`], or an in-memory store). Read `Ok(0)` as "there
    /// is no warm left to wait for", never as "the index was already complete".
    ///
    /// Unchanged by the `Drop` added in #613 — taking the handle is exactly
    /// what makes `Drop` a no-op for a caller who already joined. But a caller
    /// who never joins no longer gets a detached warm that finishes on its
    /// own: see the type's `# Drop` section.
    pub async fn await_warm(&self) -> Result<usize, ContextError> {
        let handle = lock(&self.warm).take();
        match handle {
            Some(h) => h
                .await
                .map_err(|e| ContextError::Corruption(format!("warm task failed to join: {e}")))?,
            None => Ok(0),
        }
    }

    /// Drive embedding catch-up to completion synchronously (awaitable).
    /// Reused by the background warm task; exposed for callers/tests that want
    /// a deterministic, joined warm without a spawn.
    ///
    /// It shares the store's cancel flag, which is inert here: only `Drop`
    /// raises it, and `&self` keeps the store alive for the whole call.
    pub async fn warm_now(&self) -> Result<usize, ContextError> {
        crate::warm::warm_index(
            self.path.clone(),
            self.embedder.clone(),
            self.fingerprint.id(),
            self.clock.clone(),
            self.warm_cancel.clone(),
        )
        .await
    }

    /// The active embedder fingerprint. Retrieval compares only vectors under
    /// this fingerprint (`L-C2`).
    pub fn fingerprint(&self) -> &EmbedderFingerprint {
        &self.fingerprint
    }

    /// The embedder, for pipelines that need to embed the query text.
    pub(crate) fn embedder(&self) -> &Arc<dyn Embedder> {
        &self.embedder
    }

    pub(crate) fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Run `PRAGMA integrity_check`; `Err(Corruption)` if not `"ok"`. The
    /// kill-during-index consistency test asserts this holds after a torn
    /// write (`L-L1`).
    pub fn integrity_check(&self) -> Result<(), ContextError> {
        let conn = lock(&self.conn);
        let result: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if result == "ok" {
            Ok(())
        } else {
            Err(ContextError::Corruption(result))
        }
    }

    /// Lock the connection for a synchronous unit of work. Poison-tolerant:
    /// a panic in one section never wedges the store for the rest.
    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        lock(&self.conn)
    }

    /// An owned handle to the same connection, for a `'static` unit of work.
    ///
    /// [`Self::conn`] borrows from `&self`, which cannot cross a
    /// `spawn_blocking` boundary — and recall's whole pipeline is blocking SQLite
    /// on the first-token path, so it must. Cloning the `Arc` shares the one
    /// connection; it does not open a second.
    pub(crate) fn conn_handle(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }

    /// Count of currently-live nodes (`superseded_at IS NULL`).
    pub fn node_count(&self) -> Result<usize, ContextError> {
        let conn = lock(&self.conn);
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM node WHERE superseded_at IS NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// All workspace domains as `(name, description)` — for a `context status`
    /// surface. The domains themselves are produced by `stella init` and arrive
    /// through the write path as data.
    pub fn domains(&self) -> Result<Vec<(String, Option<String>)>, ContextError> {
        let conn = lock(&self.conn);
        list_domains(&conn)
    }

    /// Every live Memory-kind node, newest first — the inspection surface
    /// behind `stella memory` (its citation stats join on `public_id`, the
    /// same stable id recalled frames carry).
    pub fn memory_nodes(&self) -> Result<Vec<NodeRow>, ContextError> {
        self.nodes_of_kinds(&["memory"])
    }

    /// Every live node a recall can actually inject into a prompt: memories
    /// *and* episodes, newest first.
    ///
    /// [`Self::memory_nodes`] shows only `memory`, which left a blind spot.
    /// An episode is a verbatim copy of a past user prompt; it is recalled and
    /// injected exactly like a memory, yet no command could display one — so a
    /// stale instruction that kept surfacing in unrelated runs could not even
    /// be named, let alone forgotten (`stella memory forget` resolves ids
    /// through [`Self::node_by_public_id`], which was never the limitation).
    pub fn recallable_nodes(&self) -> Result<Vec<NodeRow>, ContextError> {
        self.nodes_of_kinds(&["memory", "episode"])
    }

    fn nodes_of_kinds(&self, kinds: &[&str]) -> Result<Vec<NodeRow>, ContextError> {
        let conn = lock(&self.conn);
        // Kinds are crate-internal literals, never user input, but the query
        // is parameterized rather than formatted all the same.
        let placeholders = vec!["?"; kinds.len()].join(", ");
        let mut stmt = conn.prepare(&format!(
            "SELECT id, public_id, kind, display_name, content, content_hash, uri, valid_from, recorded_at, recall_tier
             FROM node WHERE kind IN ({placeholders}) AND superseded_at IS NULL
             ORDER BY recorded_at DESC, id DESC",
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(kinds), map_node_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// What the lineage layer currently holds — the report behind "what did the
    /// migration do", and the check that an edit did what it said.
    ///
    /// Derived on every call from the immutable rows, never stored state, so it
    /// cannot drift from the data it describes. It says how many lineages exist
    /// and how many revisions are live or superseded; it deliberately does not
    /// claim to know which rows a *particular* migration touched, because that
    /// is not recoverable from the result and inventing it would be worse than
    /// leaving it out.
    pub fn memory_lineage_stats(&self) -> Result<MemoryLineageStats, ContextError> {
        let conn = self.conn();
        let (lineages, revisions, live) = conn.query_row(
            "SELECT COUNT(DISTINCT lineage_id), COUNT(*),
                    COUNT(*) FILTER (WHERE superseded_at IS NULL)
             FROM memory",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )?;
        Ok(MemoryLineageStats {
            lineages: lineages as usize,
            revisions: revisions as usize,
            live: live as usize,
            superseded: (revisions - live) as usize,
        })
    }

    /// The lineage a memory node belongs to — the durable identity an edit
    /// revises, resolved from the `nod_…` id a recalled frame carries.
    ///
    /// A memory's mirror node is keyed `memory://<lineage>`, so this is a
    /// parse rather than a join. `None` for a node that is not a memory, or
    /// whose uri predates the scheme.
    pub fn memory_lineage(&self, public_id: &str) -> Result<Option<String>, ContextError> {
        let Some(node) = self.node_by_public_id(public_id)? else {
            return Ok(None);
        };
        Ok(node
            .uri
            .as_deref()
            .and_then(|uri| uri.strip_prefix("memory://"))
            .map(str::to_string))
    }

    /// The kind of a lineage's current revision — what an edit carries forward.
    pub fn memory_kind(&self, lineage_id: &str) -> Result<Option<String>, ContextError> {
        memory_kind(&self.conn(), lineage_id)
    }

    /// Every revision of a memory lineage, newest first.
    ///
    /// Exactly one revision is current; the rest are what the memory used to
    /// say. That history exists because an edit supersedes rather than
    /// overwrites (`L-C3`).
    pub fn memory_revisions(&self, lineage_id: &str) -> Result<Vec<MemoryRevision>, ContextError> {
        memory_revisions(&self.conn(), lineage_id)
    }

    /// Suppress a node in the plane that owns it: mark it superseded so every
    /// candidate reader stops offering it, immediately and before any budget is
    /// spent (#712 deliverable 4).
    ///
    /// Returns whether a live row changed — `false` means it was already
    /// suppressed, which is a success, not a failure.
    ///
    /// This is the write that makes `node.superseded_at` mean something. The
    /// column has been in the v1 DDL since the beginning and was never written,
    /// so every reader's `superseded_at IS NULL` filter was vacuously true and
    /// suppression had to happen at the CLI, on frames a budget had already
    /// paid for. A quarantined memory therefore won a slot against `max_frames`
    /// and was then discarded, silently giving that turn four frames instead of
    /// five.
    ///
    /// Nothing is deleted (`L-C3`): [`Self::restore_node`] is an exact inverse.
    pub fn supersede_node(&self, public_id: &str) -> Result<bool, ContextError> {
        let now = self.clock.now_rfc3339();
        supersede_node(&self.conn(), public_id, &now)
    }

    /// Lift a suppression, making the node a candidate again. The exact
    /// inverse of [`Self::supersede_node`]; returns whether anything was
    /// lifted.
    pub fn restore_node(&self, public_id: &str) -> Result<bool, ContextError> {
        restore_node(&self.conn(), public_id)
    }

    /// Whether any node carries this public id, superseded or not.
    ///
    /// Every other lookup here hides superseded rows, which is right for recall
    /// and wrong for a restore: the row a restore targets is exactly the one
    /// they hide. This lets a caller tell "no such memory" from "that memory is
    /// currently suppressed".
    pub fn node_exists(&self, public_id: &str) -> Result<bool, ContextError> {
        node_exists_any_state(&self.conn(), public_id)
    }

    /// A live node by its stable public id (`nod_…`) — how `stella memory
    /// promote` resolves a cited id back to the memory's content.
    pub fn node_by_public_id(&self, public_id: &str) -> Result<Option<NodeRow>, ContextError> {
        let conn = lock(&self.conn);
        let row = conn
            .query_row(
                "SELECT id, public_id, kind, display_name, content, content_hash, uri, valid_from, recorded_at, recall_tier
                 FROM node WHERE public_id = ?1 AND superseded_at IS NULL",
                params![public_id],
                map_node_row,
            )
            .optional()?;
        Ok(row)
    }
}

/// [`lock`] over a connection reached through [`ContextStore::conn_handle`], with
/// the same poison tolerance as [`ContextStore::conn`].
pub(crate) fn lock_conn(m: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
    lock(m)
}

/// Lock a mutex, recovering the guard even if a previous holder panicked. This
/// keeps the store usable after a panic in one operation (no `unwrap` on the
/// poison error, which the house style forbids outside tests).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// Hashing shared by every seam below: node ids, edge ids, and the content
// hash that keys the embedding index (`L-C2`).

/// Lowercase hex of raw bytes. Replaces `format!("{:x}", digest)`: digest
/// 0.11 (sha2 0.11) returns an `Output` array that no longer implements
/// `LowerHex`. Byte-for-byte identical to the old rendering — these hashes
/// are persisted stable ids, so the encoding must not drift.
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// sha256 hex of a string — the content hash keying embeddings (`L-C2`).
pub(crate) fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    to_hex(&h.finalize())
}
