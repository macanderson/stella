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

mod candidate;
mod domain;
mod edge;
mod embedding;
mod node;
mod record;
mod schema;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::clock::{Clock, SystemClock};
use crate::embed::{Embedder, EmbedderFingerprint, HashEmbedder};
use crate::error::ContextError;

use node::map_node_row;
use schema::{migrate, register_fingerprint};

// The module was one 2,224-line file until #712 split it along the seams it
// already had. Everything below stays reachable as `crate::store::X`, which is
// the path every consumer uses, so the split is invisible outside this file.
#[cfg(test)]
pub(crate) use candidate::{CONTENT_BYTES_READ, CONTENT_ROWS_READ};
pub(crate) use candidate::{
    NodeMeta, domain_ranked_ids, domains_for_nodes, lexical_node_meta, node_meta_for_ids,
    nodes_by_ids, recent_node_meta,
};
pub(crate) use domain::{
    list_domains, node_ids_excluded_by_scope, tag_edge_domains, tag_node_domains, upsert_domain,
};
pub(crate) use edge::{close_edge, edges_as_of, insert_edge, neighbors};
pub(crate) use embedding::{
    embedding_exists, nodes_missing_embedding, store_embedding, vectors_for_fingerprint,
};
pub use node::{NodeInput, NodeKind, NodeRow};
pub(crate) use node::{
    node_by_id, node_exists_any_state, node_ids_for_uris, restore_node, supersede_node, upsert_node,
};
pub(crate) use record::{insert_episode, insert_memory};
pub(crate) use schema::open_connection;

/// The context plane's storage handle. Not `Clone` — the background warm
/// handle is owned by exactly one store so [`ContextStore::await_warm`] has a
/// single joiner. Share it with `Arc<ContextStore>`, which is what every
/// consumer does; the connection and embedder inside are already `Arc`s, so
/// nothing is duplicated by doing so.
///
/// # Retention
///
/// This plane forgets. `node.superseded_at` is written by
/// [`Self::supersede_node`] — for a memory that was edited into a new revision,
/// and for one a person forgot — and every candidate reader filters on it, so
/// suppression takes effect at the SQL boundary rather than after a budget has
/// already been spent on the suppressed row. Supersession never deletes
/// (`L-C3`): the row survives, so [`Self::restore_node`] is an exact inverse
/// and a point-in-time query can still see what was believed before.
///
/// What is *not* retention-managed: a node whose uri changed — a renamed or
/// deleted file — is still orphaned live, serving its last-known content until
/// something supersedes it. Compaction of superseded rows is a later phase;
/// nothing here reclaims space.
///
/// # What a recall costs
///
/// Every signal is `LIMIT`-bounded at the SQL boundary and every bound derives
/// from the query's `max_frames`, so per-turn cost is set by what the caller
/// asked for rather than by how long the workspace has been alive. Node bodies
/// are read for packed survivors only — the ranking runs over metadata. The one
/// remaining full-corpus pass is the cosine scan over the vector index, which
/// reads ids and vectors and never content; an ANN accelerator is the tracked
/// follow-up that would bound it too.
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

    /// Alias for [`Self::open_and_warm`] matching the spec's `mount()` verb.
    pub fn mount(
        path: impl AsRef<Path>,
        embedder: Arc<dyn Embedder>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ContextError> {
        Self::open_and_warm(path, embedder, clock)
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
            "SELECT id, public_id, kind, display_name, content, content_hash, uri, valid_from, recorded_at
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
                "SELECT id, public_id, kind, display_name, content, content_hash, uri, valid_from, recorded_at
                 FROM node WHERE public_id = ?1 AND superseded_at IS NULL",
                params![public_id],
                map_node_row,
            )
            .optional()?;
        Ok(row)
    }
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
