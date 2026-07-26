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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::clock::{Clock, SystemClock};
use crate::embed::{Embedder, EmbedderFingerprint, HashEmbedder};
use crate::error::ContextError;

use contextgraph_types::FrameKind;

/// The current on-disk schema version, tracked in `PRAGMA user_version`.
const SCHEMA_VERSION: i64 = 3;

/// The v1 schema. Applied once, inside the migration transaction. Bi-temporal
/// columns (`valid_from`/`valid_to`/`recorded_at`/`superseded_at`) exist on both
/// `node` and `edge`, but only EDGES are actually versioned: `apply_fact`
/// closes an edge's interval on supersession, never deleting (`L-C3`), and
/// `facts_as_of` reads history back. NODES are mutable current-state —
/// `upsert_node` overwrites content in place, so their time columns stay
/// effectively unused and there is no point-in-time node reader. Fact history is
/// recoverable; node content history is not.
const MIGRATION_V1: &str = "\
CREATE TABLE node (
    id            INTEGER PRIMARY KEY,
    public_id     TEXT NOT NULL UNIQUE,
    kind          TEXT NOT NULL,
    display_name  TEXT NOT NULL,
    content       TEXT NOT NULL DEFAULT '',
    content_hash  TEXT NOT NULL,
    uri           TEXT,
    properties    TEXT NOT NULL DEFAULT '{}',
    valid_from    TEXT,
    valid_to      TEXT,
    recorded_at   TEXT NOT NULL,
    superseded_at TEXT
);
CREATE INDEX idx_node_kind ON node(kind);
CREATE INDEX idx_node_uri ON node(uri);
CREATE INDEX idx_node_content_hash ON node(content_hash);

CREATE TABLE edge (
    id            INTEGER PRIMARY KEY,
    public_id     TEXT NOT NULL,
    rel           TEXT NOT NULL,
    src_id        INTEGER NOT NULL REFERENCES node(id),
    dst_id        INTEGER NOT NULL REFERENCES node(id),
    weight        REAL NOT NULL DEFAULT 1.0,
    properties    TEXT NOT NULL DEFAULT '{}',
    valid_from    TEXT,
    valid_to      TEXT,
    recorded_at   TEXT NOT NULL,
    superseded_at TEXT,
    supersedes    INTEGER REFERENCES edge(id)
);
CREATE INDEX idx_edge_src ON edge(src_id);
CREATE INDEX idx_edge_dst ON edge(dst_id);
CREATE INDEX idx_edge_rel ON edge(rel);

CREATE TABLE embedding (
    content_hash  TEXT NOT NULL,
    fingerprint   TEXT NOT NULL,
    dims          INTEGER NOT NULL,
    vector        BLOB NOT NULL,
    recorded_at   TEXT NOT NULL,
    PRIMARY KEY (content_hash, fingerprint)
) WITHOUT ROWID;

CREATE TABLE episode (
    id            INTEGER PRIMARY KEY,
    public_id     TEXT NOT NULL UNIQUE,
    summary       TEXT NOT NULL,
    files_touched TEXT NOT NULL DEFAULT '[]',
    outcome       TEXT NOT NULL,
    salience      REAL NOT NULL DEFAULT 0.0,
    started_at    TEXT NOT NULL,
    ended_at      TEXT NOT NULL,
    recorded_at   TEXT NOT NULL
);

CREATE TABLE embedder_fingerprint (
    id            TEXT PRIMARY KEY,
    model_id      TEXT NOT NULL,
    revision      TEXT NOT NULL,
    dims          INTEGER NOT NULL,
    normalization TEXT NOT NULL,
    first_seen_at TEXT NOT NULL
);
";

/// The v2 schema: workspace **domains** as first-class tags, and
/// a **memory** record type (reflections). Domains are a normalized table plus
/// indexable junctions — never a JSON blob — so "everything in domain X" is a
/// key-lookup, not a scan. A domain tag rides node and edge/fact rows (and, via
/// their mirror nodes, episodes and memories). Reflection memories are their
/// own record with a `kind`, mirrored to a retrievable `Memory` node so recall
/// scores them by similarity + domain overlap + recency.
const MIGRATION_V2: &str = "\
CREATE TABLE domain (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    description  TEXT,
    recorded_at  TEXT NOT NULL
);

CREATE TABLE node_domains (
    node_id      INTEGER NOT NULL REFERENCES node(id),
    domain_id    INTEGER NOT NULL REFERENCES domain(id),
    PRIMARY KEY (node_id, domain_id)
) WITHOUT ROWID;
CREATE INDEX idx_node_domains_domain ON node_domains(domain_id);

CREATE TABLE edge_domains (
    edge_id      INTEGER NOT NULL REFERENCES edge(id),
    domain_id    INTEGER NOT NULL REFERENCES domain(id),
    PRIMARY KEY (edge_id, domain_id)
) WITHOUT ROWID;
CREATE INDEX idx_edge_domains_domain ON edge_domains(domain_id);

CREATE TABLE memory (
    id           INTEGER PRIMARY KEY,
    public_id    TEXT NOT NULL UNIQUE,
    kind         TEXT NOT NULL,
    content      TEXT NOT NULL,
    salience     REAL NOT NULL DEFAULT 0.0,
    recorded_at  TEXT NOT NULL
);
CREATE INDEX idx_memory_kind ON memory(kind);
";

/// V3 — evict the code graph's tables from `context.db`. Historically the
/// tree-sitter index shared this one file (`stella-graph`'s original
/// single-file design, prefixing its tables `code_graph_`); it now lives in its
/// own `.stella/private/codegraph.db`, which every consumer (`graph_query`, the CGP
/// `GraphProvider`) reads. Any `code_graph_*` tables still in `context.db` are
/// orphaned duplicates no code reads or updates — dropping them removes the
/// "two databases hold the code graph" duplication. Children (FK to
/// `code_graph_files`) are dropped first. `IF EXISTS` so a fresh store is a
/// no-op.
const MIGRATION_V3: &str = "\
DROP TABLE IF EXISTS code_graph_symbols;
DROP TABLE IF EXISTS code_graph_imports;
DROP TABLE IF EXISTS code_graph_files;
";

/// Typed node vocabulary. Stored as its `as_str` form; retrieval maps it onto
/// a `contextgraph_types::FrameKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A workspace file; surfaces as a `Snippet` frame.
    File,
    /// A code symbol — function, type, module; surfaces as a `Symbol` frame.
    Symbol,
    /// A named idea or entity. The general-purpose kind, and the one a stored
    /// `node.kind` this binary does not recognize reads back as.
    Concept,
    /// A fact reified as its own retrievable node. Assertions written through
    /// `FactAssertion` become `edge` rows, not nodes; this is for callers that
    /// want the fact itself to be a citable frame.
    Fact,
    /// The mirror node of an `episode` record — what makes a past turn
    /// recallable.
    Episode,
    /// A person (author, reviewer, teammate).
    Person,
    /// A produced artifact — a doc, report, or generated file; surfaces as a
    /// `Doc` frame.
    Artifact,
    /// A unit of work (issue, ticket, TODO).
    Task,
    /// A memory (e.g. a post-turn reflection). Mirrors a `memory` record so it
    /// is retrievable and domain-taggable through the normal node pipeline.
    Memory,
}

impl NodeKind {
    /// The canonical string stored in `node.kind`.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Symbol => "symbol",
            NodeKind::Concept => "concept",
            NodeKind::Fact => "fact",
            NodeKind::Episode => "episode",
            NodeKind::Person => "person",
            NodeKind::Artifact => "artifact",
            NodeKind::Task => "task",
            NodeKind::Memory => "memory",
        }
    }

    /// Parse a stored `node.kind` back into the enum.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "file" => NodeKind::File,
            "symbol" => NodeKind::Symbol,
            "concept" => NodeKind::Concept,
            "fact" => NodeKind::Fact,
            "episode" => NodeKind::Episode,
            "person" => NodeKind::Person,
            "artifact" => NodeKind::Artifact,
            "task" => NodeKind::Task,
            "memory" => NodeKind::Memory,
            _ => return None,
        })
    }

    /// Map onto the CGP frame kind a retrieved node surfaces as.
    pub fn to_frame_kind(self) -> FrameKind {
        match self {
            NodeKind::File => FrameKind::Snippet,
            NodeKind::Symbol => FrameKind::Symbol,
            NodeKind::Episode => FrameKind::Episode,
            NodeKind::Artifact => FrameKind::Doc,
            NodeKind::Memory => FrameKind::Memory,
            // Concept/Fact/Person/Task all read as facts to a consuming host.
            NodeKind::Concept | NodeKind::Fact | NodeKind::Person | NodeKind::Task => {
                FrameKind::Fact
            }
        }
    }
}

/// A node to write. `display_name` is mandatory and non-empty — it is the
/// human citation label (`L-C4`), enforced at write time so retrieval can
/// never later fail to cite.
#[derive(Debug, Clone)]
pub struct NodeInput {
    /// The typed vocabulary entry this node belongs to; also decides the
    /// `FrameKind` retrieval surfaces it as.
    pub kind: NodeKind,
    /// The human citation label (`L-C4`). Mandatory and non-empty —
    /// `upsert_node` rejects a blank one. It is also the identity key when no
    /// `uri` is set.
    pub display_name: String,
    /// The retrievable body: what gets embedded, hashed for the byte-compat
    /// skip (`L-C2`), and served as a frame's content. Leave it empty for a
    /// node that exists only as a graph endpoint — empty content is never
    /// embedded.
    pub content: String,
    /// Source uri (`file://…`, `memory://…`, `episode://…`). When present it is
    /// the node's identity key, so two writes with the same uri update one row,
    /// and it is what a query's `anchors` are matched against.
    pub uri: Option<String>,
    /// Free-form JSON stored on the node row for consumers reading the graph
    /// directly. Nothing in the retrieval path reads it back today.
    pub properties: serde_json::Value,
    /// Workspace domain tags (e.g. `["auth", "billing"]`). One or more; stored
    /// via the `node_domains` junction (indexable), never a JSON blob.
    pub domains: Vec<String>,
}

impl NodeInput {
    /// A node with the given kind and label, empty content, no uri, no domains.
    pub fn new(kind: NodeKind, display_name: impl Into<String>) -> Self {
        Self {
            kind,
            display_name: display_name.into(),
            content: String::new(),
            uri: None,
            properties: serde_json::json!({}),
            domains: Vec::new(),
        }
    }

    /// Attach retrievable content.
    // `#[must_use]` on every builder below: they consume and return `self`, so
    // a dropped result is silent data loss — content or domain tags that never
    // reach the store, with no error to notice.
    #[must_use]
    pub fn with_content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    /// Attach a source uri (used for anchor matching and provenance).
    #[must_use]
    pub fn with_uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = Some(uri.into());
        self
    }

    /// Tag with one or more workspace domains.
    #[must_use]
    pub fn with_domains<I, S>(mut self, domains: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.domains = domains.into_iter().map(Into::into).collect();
        self
    }

    /// The identity key: the uri when present, else the display name. Two
    /// writes with the same identity update the same node (content-on-touch).
    fn natural_key(&self) -> &str {
        match &self.uri {
            Some(u) if !u.is_empty() => u.as_str(),
            _ => self.display_name.as_str(),
        }
    }

    fn public_id(&self) -> String {
        node_public_id(self.kind, self.natural_key())
    }
}

/// A node read back from the store.
#[derive(Debug, Clone)]
pub struct NodeRow {
    /// The SQLite rowid. Store-internal: edges, the domain junctions, and the
    /// retrieval pipeline join on it, but it is never surfaced to a host —
    /// `public_id` is the identity that travels.
    pub id: i64,
    /// The stable `nod_…` identity, derived from kind + natural key. It is the
    /// frame id a host holds, reuses, and presents back to `context/verify`.
    pub public_id: String,
    /// The typed vocabulary entry, mapped onto a `FrameKind` at retrieval. An
    /// unrecognized stored kind reads back as [`NodeKind::Concept`].
    pub kind: NodeKind,
    /// The human citation label (`L-C4`) — it becomes the frame's title, and a
    /// blank one is a `MissingCitation` error rather than an unlabeled frame.
    pub display_name: String,
    /// The retrievable body, served as the frame's content and the basis of its
    /// declared `token_cost`.
    pub content: String,
    /// sha256 of `content`. Keys the embedding index together with the
    /// fingerprint (`L-C2`), and is what a frame declares as
    /// `sha256:<content_hash>` so a host can revalidate it without moving the
    /// body.
    pub content_hash: String,
    /// Source uri when the node has one — carried onto the frame and into its
    /// provenance chain.
    pub uri: Option<String>,
    /// Valid time: when the fact became true in the world — may precede
    /// `recorded_at` (observation), never follows it. `None` = unknown,
    /// treated as valid-since-observation.
    ///
    /// **Always `None` today.** Only EDGES are versioned (see `MIGRATION_V1`);
    /// `upsert_node` never writes `node.valid_from`, so this reads back the
    /// column's NULL for every row. It is kept because the column exists and a
    /// point-in-time node reader would populate it — not because anything
    /// currently sets it. Read it as "not yet wired", never as "unknown".
    pub valid_from: Option<String>,
    /// Transaction time the row was **first** written (RFC-3339). `upsert_node`
    /// updates content in place without touching it, so it is a creation time,
    /// not a modification time — which is exactly what recall's recency
    /// ranking sorts on.
    pub recorded_at: String,
}

/// The context plane's storage handle. Not `Clone` — the background warm
/// handle is owned by exactly one store so [`ContextStore::await_warm`] has a
/// single joiner. Share it with `Arc<ContextStore>`, which is what every
/// consumer does; the connection and embedder inside are already `Arc`s, so
/// nothing is duplicated by doing so.
///
/// # Retention
///
/// Nothing here forgets. `node.superseded_at` is never written (only edges are
/// versioned), there is no delete or tombstone path in this crate, and a node
/// whose uri changed — a renamed or deleted file — is orphaned live forever,
/// still serving its last-known content. Recall reads *every* live node and
/// *every* vector under the active fingerprint on each query, so both the
/// store's size and a recall's latency grow monotonically with the workspace's
/// lifetime. That is fine at CLI-local scale and is the plane's first scaling
/// wall; a forget/compaction path is the tracked follow-up, not an oversight.
///
/// `stella memory forget` is **not** that path, and reading it as one is the
/// mistake to avoid: its tombstone lives in `store.db`, and the CLI applies it
/// to the frames [`ContextStore::recall`] already returned. A forgotten memory
/// is still stored here, still embedded, still ranked, and still spends the
/// query's frame and token budget before the caller drops it.
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
        })
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

/// Open a connection with the plane's fixed pragmas: WAL for concurrent
/// reader/writer, `NORMAL` sync (durable enough with WAL, far cheaper than
/// `FULL`), foreign keys on, and a busy timeout so a warm-task write never
/// races the main connection into `SQLITE_BUSY`.
pub(crate) fn open_connection(path: &Path) -> Result<Connection, ContextError> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;\
         PRAGMA synchronous=NORMAL;\
         PRAGMA foreign_keys=ON;\
         PRAGMA busy_timeout=5000;",
    )?;
    // After the WAL pragma, so the `-wal`/`-shm` siblings SQLite creates for
    // this connection exist and get narrowed in the same pass.
    restrict_to_owner(path);
    Ok(conn)
}

/// Narrow `context.db` and its WAL/SHM siblings to `0600`.
///
/// SQLite creates all three at the process umask, which on a default `0022`
/// system means `0644` — world-readable. This file is not incidental state:
/// it holds recalled memories, episodes (verbatim copies of past user
/// prompts), and facts mined from workspace content, so whatever secrets the
/// workspace contains can end up inside it. These are files *we* create, and
/// the posture for those is owner-only from as early as we can manage.
///
/// Best-effort on purpose. A `chmod` failure does not fail the open: the
/// alternative is a workspace that cannot be opened at all because of a
/// filesystem that does not carry Unix modes (a `vfat`/`exfat` mount, some
/// network shares), which would be a hard regression in exchange for a
/// permission the filesystem was never enforcing anyway. Missing siblings are
/// likewise skipped — `-wal` only exists once something is written.
#[cfg(unix)]
fn restrict_to_owner(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    for suffix in ["", "-wal", "-shm"] {
        // SQLite names the siblings by appending to the WHOLE database
        // filename (`context.db-wal`), not by replacing an extension.
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let target = PathBuf::from(name);
        if target.exists() {
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600));
        }
    }
}

/// Non-Unix targets have no mode word to set; Windows expresses this through
/// ACLs, which are a different mechanism entirely. A no-op, never an error —
/// failing here would make the context plane unopenable on those platforms in
/// exchange for nothing.
#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) {}

/// Apply pending migrations inside a single transaction, bumping
/// `user_version` atomically with the DDL.
///
/// **Downgrades are rejected.** A store stamped by a *newer* binary
/// (`user_version > SCHEMA_VERSION`) is an error rather than an as-is open:
/// this file holds episodic memory and the bi-temporal fact graph, neither of
/// which is rebuildable, so an older stella writing into a schema it does not
/// know would silently violate whatever invariants the newer schema added. The
/// message mirrors the one `stella_store::Store::migrate` already writes — the
/// fault is an out-of-date binary, not a broken workspace.
///
/// **The version read is outside the transaction.** `unchecked_transaction` is
/// `DEFERRED`, so two processes opening the same fresh workspace at once (a
/// fleet run, or a `stella` session next to a `stella stats`) can both read
/// `user_version = 0` and both try to apply `MIGRATION_V1`; the loser reports
/// SQLITE_BUSY or "table node already exists" instead of the "already migrated"
/// no-op it should. Re-reading `user_version` inside a `BEGIN IMMEDIATE` closes
/// the window — an audit note, not a fix, because the fix belongs with a test
/// that opens the same fresh path from two threads.
fn migrate(conn: &Connection) -> Result<(), ContextError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(ContextError::SchemaTooNew(format!(
            "context.db is at schema version {version}, but this build only \
             knows {SCHEMA_VERSION} — your stella binary is out of date, not \
             the workspace. Upgrade with `brew upgrade stella`, re-run \
             install.sh, or grab a newer build from \
             https://github.com/macanderson/stella/releases, then reopen \
             this workspace."
        )));
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    if version < 1 {
        tx.execute_batch(MIGRATION_V1)?;
    }
    if version < 2 {
        tx.execute_batch(MIGRATION_V2)?;
    }
    if version < 3 {
        tx.execute_batch(MIGRATION_V3)?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// Record the active fingerprint (idempotent). Its presence in the registry is
/// what lets a later `status` command report which embedder the index was
/// built with.
fn register_fingerprint(
    conn: &Connection,
    fp: &EmbedderFingerprint,
    now: &str,
) -> Result<(), ContextError> {
    conn.execute(
        "INSERT INTO embedder_fingerprint (id, model_id, revision, dims, normalization, first_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO NOTHING",
        params![
            fp.id(),
            fp.model_id,
            fp.revision,
            fp.dims as i64,
            fp.normalization,
            now
        ],
    )?;
    Ok(())
}

// Low-level accessors (pub(crate)) shared by retrieval.rs and writeback.rs.
// All take a `&Connection` (a `&Transaction` derefs to one), so a caller can
// batch many of them inside a single transaction.

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

fn node_public_id(kind: NodeKind, natural_key: &str) -> String {
    let mut h = Sha256::new();
    h.update(kind.as_str().as_bytes());
    h.update([0u8]);
    h.update(natural_key.as_bytes());
    let hex = to_hex(&h.finalize());
    format!("nod_{}", &hex[..24])
}

/// Encode a vector as a little-endian f32 BLOB.
pub(crate) fn vector_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Decode a little-endian f32 BLOB back to a vector. A length that isn't a
/// multiple of 4 is corruption, reported loudly rather than truncated.
pub(crate) fn blob_to_vector(blob: &[u8]) -> Result<Vec<f32>, ContextError> {
    if !blob.len().is_multiple_of(4) {
        return Err(ContextError::Corruption(format!(
            "embedding blob length {} is not a multiple of 4",
            blob.len()
        )));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

/// Upsert a node by identity, updating content-on-touch. Returns its rowid.
/// Rejects an empty display name (`L-C4` — a node must be citable).
pub(crate) fn upsert_node(
    conn: &Connection,
    node: &NodeInput,
    now: &str,
) -> Result<i64, ContextError> {
    if node.display_name.trim().is_empty() {
        return Err(ContextError::InvalidInput(
            "node display_name must be non-empty (every node must be humanly citable, L-C4)".into(),
        ));
    }
    let content_hash = sha256_hex(&node.content);
    let props = serde_json::to_string(&node.properties)?;
    let id: i64 = conn.query_row(
        "INSERT INTO node (public_id, kind, display_name, content, content_hash, uri, properties, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(public_id) DO UPDATE SET
             display_name = excluded.display_name,
             content      = excluded.content,
             content_hash = excluded.content_hash,
             uri          = excluded.uri,
             properties   = excluded.properties
         RETURNING id",
        params![
            node.public_id(),
            node.kind.as_str(),
            node.display_name,
            node.content,
            content_hash,
            node.uri,
            props,
            now
        ],
        |r| r.get(0),
    )?;
    Ok(id)
}

fn map_node_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeRow> {
    let kind_str: String = row.get("kind")?;
    Ok(NodeRow {
        id: row.get("id")?,
        public_id: row.get("public_id")?,
        // An unrecognized kind reads as `Concept` rather than failing the
        // whole query: one unknown row must not blind recall to every other
        // row. `migrate` rejects a store stamped by a newer stella, so a newer
        // binary's node kinds no longer reach here — what is left is a
        // hand-edited or partially restored db. The node stays retrievable and
        // citable; only its `FrameKind` is coarsened.
        kind: NodeKind::parse(&kind_str).unwrap_or(NodeKind::Concept),
        display_name: row.get("display_name")?,
        content: row.get("content")?,
        content_hash: row.get("content_hash")?,
        uri: row.get("uri")?,
        valid_from: row.get("valid_from")?,
        recorded_at: row.get("recorded_at")?,
    })
}

/// Every live node (for recency scoring, lexical fallback, and warm scanning).
pub(crate) fn live_nodes(conn: &Connection) -> Result<Vec<NodeRow>, ContextError> {
    let mut stmt = conn.prepare(
        "SELECT id, public_id, kind, display_name, content, content_hash, uri, valid_from, recorded_at
         FROM node WHERE superseded_at IS NULL",
    )?;
    let rows = stmt.query_map([], map_node_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Live node ids whose uri matches one of `uris` (anchor resolution).
pub(crate) fn node_ids_for_uris(
    conn: &Connection,
    uris: &[String],
) -> Result<Vec<i64>, ContextError> {
    let mut out = Vec::new();
    let mut stmt = conn.prepare("SELECT id FROM node WHERE uri = ?1 AND superseded_at IS NULL")?;
    for uri in uris {
        let ids = stmt.query_map(params![uri], |r| r.get::<_, i64>(0))?;
        for id in ids {
            out.push(id?);
        }
    }
    Ok(out)
}

/// (node_id, vector) for every live node with an embedding under `fingerprint`.
/// The join on `content_hash` is what enforces "never mix fingerprints"
/// structurally — a vector under any other fingerprint is simply not selected.
pub(crate) fn vectors_for_fingerprint(
    conn: &Connection,
    fingerprint: &str,
) -> Result<Vec<(i64, Vec<f32>)>, ContextError> {
    let mut stmt = conn.prepare(
        "SELECT n.id, e.vector
         FROM embedding e
         JOIN node n ON n.content_hash = e.content_hash
         WHERE e.fingerprint = ?1 AND n.superseded_at IS NULL",
    )?;
    let rows = stmt.query_map(params![fingerprint], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (id, blob) = r?;
        out.push((id, blob_to_vector(&blob)?));
    }
    Ok(out)
}

/// Fetch a single node by rowid.
pub(crate) fn node_by_id(conn: &Connection, id: i64) -> Result<Option<NodeRow>, ContextError> {
    let row = conn
        .query_row(
            "SELECT id, public_id, kind, display_name, content, content_hash, uri, valid_from, recorded_at
             FROM node WHERE id = ?1",
            params![id],
            map_node_row,
        )
        .optional()?;
    Ok(row)
}

/// 1-hop neighbors of `seeds` over currently-believed fact edges, as
/// `(neighbor_id, edge_weight)`. `as_of` (transaction time) pins which beliefs
/// are visible: `None` = currently believed (`superseded_at IS NULL`).
pub(crate) fn neighbors(
    conn: &Connection,
    seeds: &[i64],
    as_of: Option<&str>,
) -> Result<Vec<(i64, f64)>, ContextError> {
    let mut out = Vec::new();
    // Undirected 1-hop: a seed on either endpoint pulls in the other.
    let sql = match as_of {
        None => {
            "SELECT CASE WHEN src_id = ?1 THEN dst_id ELSE src_id END AS other, weight
             FROM edge
             WHERE (src_id = ?1 OR dst_id = ?1) AND superseded_at IS NULL"
        }
        Some(_) => {
            "SELECT CASE WHEN src_id = ?1 THEN dst_id ELSE src_id END AS other, weight
             FROM edge
             WHERE (src_id = ?1 OR dst_id = ?1)
               AND recorded_at <= ?2
               AND (superseded_at IS NULL OR superseded_at > ?2)"
        }
    };
    let map = |r: &rusqlite::Row<'_>| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?));
    let mut stmt = conn.prepare(sql)?;
    for &seed in seeds {
        // Each arm's `MappedRows` is a distinct closure type, so consume it
        // fully inside the arm rather than binding it across the `match`.
        match as_of {
            None => {
                for r in stmt.query_map(params![seed], map)? {
                    out.push(r?);
                }
            }
            Some(t) => {
                for r in stmt.query_map(params![seed, t], map)? {
                    out.push(r?);
                }
            }
        }
    }
    Ok(out)
}

/// Whether a vector already exists for `(content_hash, fingerprint)` — the
/// byte-compat skip that makes re-indexing cheap (`L-C2`).
pub(crate) fn embedding_exists(
    conn: &Connection,
    content_hash: &str,
    fingerprint: &str,
) -> Result<bool, ContextError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM embedding WHERE content_hash = ?1 AND fingerprint = ?2",
        params![content_hash, fingerprint],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Insert a vector (idempotent under the composite primary key). Returns
/// whether a new row was written (`false` = it already existed = reused).
pub(crate) fn store_embedding(
    conn: &Connection,
    content_hash: &str,
    fingerprint: &str,
    vector: &[f32],
    now: &str,
) -> Result<bool, ContextError> {
    let changed = conn.execute(
        "INSERT INTO embedding (content_hash, fingerprint, dims, vector, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(content_hash, fingerprint) DO NOTHING",
        params![
            content_hash,
            fingerprint,
            vector.len() as i64,
            vector_to_blob(vector),
            now
        ],
    )?;
    Ok(changed > 0)
}

/// A fact edge read back for point-in-time queries. Endpoints are node rowids
/// the caller resolves to human labels (`L-C4`).
#[derive(Debug, Clone)]
pub(crate) struct EdgeView {
    pub rel: String,
    pub src_id: i64,
    pub dst_id: i64,
    pub recorded_at: String,
    pub superseded_at: Option<String>,
}

/// Sequence disambiguating two facts asserted within the same clock second,
/// which would otherwise hash to the same edge `public_id`.
///
/// It is **process-local**, so two stella processes writing the same workspace
/// db concurrently can still mint the same `edg_…` id; `edge.public_id` carries
/// no UNIQUE constraint, so those rows coexist silently. Nothing reads an edge
/// by public id today, which is why this is tolerable rather than a bug — a
/// reader would need the id made collision-proof first (see the audit note on
/// `insert_edge`).
static EDGE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Insert a fact edge. `supersedes` links to the edge this one replaced (the
/// `SUPERSEDES` relation of), or `None` for a
/// fresh assertion. Returns the new edge's rowid.
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_edge(
    conn: &Connection,
    rel: &str,
    src_id: i64,
    dst_id: i64,
    weight: f64,
    properties: &serde_json::Value,
    valid_from: Option<&str>,
    valid_to: Option<&str>,
    now: &str,
    supersedes: Option<i64>,
) -> Result<i64, ContextError> {
    let seq = EDGE_SEQ.fetch_add(1, Ordering::Relaxed);
    let public_id = format!(
        "edg_{}",
        &sha256_hex(&format!("{rel}:{src_id}:{dst_id}:{now}:{seq}"))[..24]
    );
    let props = serde_json::to_string(properties)?;
    let id: i64 = conn.query_row(
        "INSERT INTO edge (public_id, rel, src_id, dst_id, weight, properties, valid_from, valid_to, recorded_at, supersedes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         RETURNING id",
        params![public_id, rel, src_id, dst_id, weight, props, valid_from, valid_to, now, supersedes],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// Close an edge's intervals: set `superseded_at` (transaction time) and, if
/// not already ended, `valid_to` (world time). **Never deletes** (`L-C3`) — the
/// row survives so "what did we believe at T1" still answers.
pub(crate) fn close_edge(
    conn: &Connection,
    edge_id: i64,
    superseded_at: &str,
    valid_to: &str,
) -> Result<(), ContextError> {
    conn.execute(
        "UPDATE edge SET superseded_at = ?2, valid_to = COALESCE(valid_to, ?3) WHERE id = ?1",
        params![edge_id, superseded_at, valid_to],
    )?;
    Ok(())
}

/// Fact edges as believed at a transaction-time instant. `as_of = None` means
/// "currently believed" (`superseded_at IS NULL`); `Some(t)` reconstructs the
/// belief set at `t` — the bi-temporal audit query (`L-C3`).
pub(crate) fn edges_as_of(
    conn: &Connection,
    as_of: Option<&str>,
) -> Result<Vec<EdgeView>, ContextError> {
    let map = |r: &rusqlite::Row<'_>| {
        Ok(EdgeView {
            rel: r.get(0)?,
            src_id: r.get(1)?,
            dst_id: r.get(2)?,
            recorded_at: r.get(3)?,
            superseded_at: r.get(4)?,
        })
    };
    let mut out = Vec::new();
    match as_of {
        None => {
            let mut stmt = conn.prepare(
                "SELECT rel, src_id, dst_id, recorded_at, superseded_at
                 FROM edge WHERE superseded_at IS NULL",
            )?;
            for r in stmt.query_map([], map)? {
                out.push(r?);
            }
        }
        Some(t) => {
            let mut stmt = conn.prepare(
                "SELECT rel, src_id, dst_id, recorded_at, superseded_at
                 FROM edge
                 WHERE recorded_at <= ?1 AND (superseded_at IS NULL OR superseded_at > ?1)",
            )?;
            for r in stmt.query_map(params![t], map)? {
                out.push(r?);
            }
        }
    }
    Ok(out)
}

/// Insert or update an episode (idempotent by `public_id`). Returns rowid.
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_episode(
    conn: &Connection,
    public_id: &str,
    summary: &str,
    files_touched: &serde_json::Value,
    outcome: &str,
    salience: f64,
    started_at: &str,
    ended_at: &str,
    now: &str,
) -> Result<i64, ContextError> {
    let files = serde_json::to_string(files_touched)?;
    let id: i64 = conn.query_row(
        "INSERT INTO episode (public_id, summary, files_touched, outcome, salience, started_at, ended_at, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(public_id) DO UPDATE SET
             summary = excluded.summary,
             files_touched = excluded.files_touched,
             outcome = excluded.outcome,
             salience = excluded.salience
         RETURNING id",
        params![public_id, summary, files, outcome, salience, started_at, ended_at, now],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// Insert or update a memory record (idempotent by `public_id`). Returns rowid.
pub(crate) fn insert_memory(
    conn: &Connection,
    public_id: &str,
    kind: &str,
    content: &str,
    salience: f64,
    now: &str,
) -> Result<i64, ContextError> {
    let id: i64 = conn.query_row(
        "INSERT INTO memory (public_id, kind, content, salience, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(public_id) DO UPDATE SET
             kind = excluded.kind,
             content = excluded.content,
             salience = excluded.salience
         RETURNING id",
        params![public_id, kind, content, salience, now],
        |r| r.get(0),
    )?;
    Ok(id)
}

// Domains: a normalized table + indexable junctions. Tagging is
// idempotent; unknown domain names are auto-created (the workspace's `stella
// init` domains arrive as data, so a write may reference a name before an
// explicit definition with a description exists).

/// Insert a domain by name (idempotent), optionally setting/refreshing its
/// description. Returns its rowid.
pub(crate) fn upsert_domain(
    conn: &Connection,
    name: &str,
    description: Option<&str>,
    now: &str,
) -> Result<i64, ContextError> {
    if name.trim().is_empty() {
        return Err(ContextError::InvalidInput(
            "domain name must be non-empty".into(),
        ));
    }
    // COALESCE keeps an existing description when a later tag-only write passes
    // None, but lets an explicit definition set or update it.
    let id: i64 = conn.query_row(
        "INSERT INTO domain (name, description, recorded_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(name) DO UPDATE SET
             description = COALESCE(excluded.description, domain.description)
         RETURNING id",
        params![name, description, now],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// Tag a node with domains (auto-creating unknown ones). Returns the number of
/// new tag associations written.
pub(crate) fn tag_node_domains(
    conn: &Connection,
    node_id: i64,
    domains: &[String],
    now: &str,
) -> Result<usize, ContextError> {
    let mut added = 0;
    for name in domains {
        let domain_id = upsert_domain(conn, name, None, now)?;
        added += conn.execute(
            "INSERT INTO node_domains (node_id, domain_id) VALUES (?1, ?2)
             ON CONFLICT(node_id, domain_id) DO NOTHING",
            params![node_id, domain_id],
        )?;
    }
    Ok(added)
}

/// Tag an edge/fact with domains (auto-creating unknown ones). Returns the
/// number of new tag associations written.
pub(crate) fn tag_edge_domains(
    conn: &Connection,
    edge_id: i64,
    domains: &[String],
    now: &str,
) -> Result<usize, ContextError> {
    let mut added = 0;
    for name in domains {
        let domain_id = upsert_domain(conn, name, None, now)?;
        added += conn.execute(
            "INSERT INTO edge_domains (edge_id, domain_id) VALUES (?1, ?2)
             ON CONFLICT(edge_id, domain_id) DO NOTHING",
            params![edge_id, domain_id],
        )?;
    }
    Ok(added)
}

/// Every LIVE node's domain names in one scan, sorted per node for stable
/// citation display — the batched form of the old per-node query. Recall
/// runs this once per prompt; one statement per live node was an N+1 whose
/// cost grew with lifetime memory size. Superseded nodes are filtered in
/// SQL (same liveness predicate as [`live_nodes`]): recall only looks up
/// live candidates, so loading dead nodes' tags made the scan grow with
/// historical store size for no reader.
pub(crate) fn domains_by_node(
    conn: &Connection,
) -> Result<std::collections::HashMap<i64, Vec<String>>, ContextError> {
    let mut stmt = conn.prepare(
        "SELECT nd.node_id, d.name FROM node_domains nd
         JOIN domain d ON d.id = nd.domain_id
         JOIN node n ON n.id = nd.node_id
         WHERE n.superseded_at IS NULL
         ORDER BY nd.node_id, d.name",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut out: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
    for r in rows {
        let (id, name) = r?;
        out.entry(id).or_default().push(name);
    }
    Ok(out)
}

/// Live node ids that carry a domain tag but NONE in `scope` — exactly the
/// set a scoped recall must exclude. Untagged nodes are never returned (they
/// stay candidates): most memories — reflections whose lessons name no domain,
/// episodes from turns that touched no taxonomy-covered file — are untagged,
/// and a scope filter that dropped them would silence recall entirely the
/// moment `stella init` writes a taxonomy. An empty `scope` returns an empty
/// set (nothing is out of scope).
///
/// The out-of-scope test runs in one SQL statement (an anti-join against the
/// in-scope tag set) rather than materializing every tagged id in memory and
/// differencing in Rust — a large initialized workspace can carry many tags
/// unrelated to the active scope.
pub(crate) fn node_ids_excluded_by_scope(
    conn: &Connection,
    scope: &[String],
) -> Result<std::collections::HashSet<i64>, ContextError> {
    if scope.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let placeholders = std::iter::repeat_n("?", scope.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT DISTINCT nd.node_id FROM node_domains nd
         JOIN node n ON n.id = nd.node_id
         WHERE n.superseded_at IS NULL
           AND nd.node_id NOT IN (
             SELECT nd2.node_id FROM node_domains nd2
             JOIN domain d ON d.id = nd2.domain_id
             WHERE d.name IN ({placeholders})
           )"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut set = std::collections::HashSet::new();
    for r in stmt.query_map(rusqlite::params_from_iter(scope.iter()), |r| {
        r.get::<_, i64>(0)
    })? {
        set.insert(r?);
    }
    Ok(set)
}

/// All defined domains as `(name, description)`, for status/inspection.
pub(crate) fn list_domains(
    conn: &Connection,
) -> Result<Vec<(String, Option<String>)>, ContextError> {
    let mut stmt = conn.prepare("SELECT name, description FROM domain ORDER BY name")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Live nodes lacking a vector under `fingerprint`, as `(content_hash, content)`.
/// Deduplicated by content hash so identical content is embedded once.
pub(crate) fn nodes_missing_embedding(
    conn: &Connection,
    fingerprint: &str,
) -> Result<Vec<(String, String)>, ContextError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT n.content_hash, n.content
         FROM node n
         WHERE n.superseded_at IS NULL
           AND n.content <> ''
           AND NOT EXISTS (
               SELECT 1 FROM embedding e
               WHERE e.content_hash = n.content_hash AND e.fingerprint = ?1
           )",
    )?;
    let rows = stmt.query_map(params![fingerprint], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp_store() -> (TempDir, ContextStore) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("context.db");
        let store = ContextStore::open(&path).expect("open");
        (dir, store)
    }

    #[test]
    fn open_creates_a_consistent_store_at_the_current_schema_version() {
        let (_dir, store) = tmp_store();
        store.integrity_check().expect("integrity");
        let conn = store.conn();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    /// `context.db` carries recalled memories and verbatim past prompts, so
    /// it must not land at the process umask (`0644` on a default system).
    /// The WAL/SHM siblings hold the same bytes and are checked too — a
    /// tightened database with a world-readable `-wal` beside it would be the
    /// exact false assurance this change exists to remove.
    #[cfg(unix)]
    #[test]
    fn opening_narrows_the_database_and_its_wal_siblings_to_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        let store = ContextStore::open(&path).unwrap();
        // Force a WAL write so `-wal` definitely exists alongside `-shm`.
        upsert_node(
            &store.conn(),
            &NodeInput::new(NodeKind::Concept, "secret-bearing content"),
            "2026-01-01T00:00:00Z",
        )
        .unwrap();

        for suffix in ["", "-wal", "-shm"] {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            let target = PathBuf::from(name);
            if !target.exists() {
                continue;
            }
            let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o600,
                "{} must be owner-only, found {mode:04o}",
                target.display()
            );
        }
    }

    #[test]
    fn reopening_is_idempotent_and_does_not_remigrate() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        {
            let s = ContextStore::open(&path).unwrap();
            let conn = s.conn();
            upsert_node(
                &conn,
                &NodeInput::new(NodeKind::Concept, "keep me"),
                "2026-01-01T00:00:00Z",
            )
            .unwrap();
        }
        let s2 = ContextStore::open(&path).unwrap();
        assert_eq!(s2.node_count().unwrap(), 1, "data survives reopen");
        s2.integrity_check().unwrap();
    }

    #[test]
    fn opening_drops_orphaned_code_graph_tables_from_context_db() {
        // A legacy context.db that still carries the code graph's tables (from
        // the era when the tree-sitter index shared this one file) must have
        // them dropped on open — the graph now lives in codegraph.db, and
        // leaving duplicates here is the "two DBs hold the code graph" defect.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATION_V1).unwrap();
            conn.execute_batch(MIGRATION_V2).unwrap();
            // Simulate the orphaned graph tables + a pre-V3 schema version.
            conn.execute_batch(
                "CREATE TABLE code_graph_files (id INTEGER PRIMARY KEY, path TEXT);\
                 CREATE TABLE code_graph_symbols (id INTEGER PRIMARY KEY, file_id INTEGER);\
                 CREATE TABLE code_graph_imports (id INTEGER PRIMARY KEY, from_file_id INTEGER);",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 2i64).unwrap();
        }
        // Reopen through the store: the V3 migration must evict the orphans.
        let store = ContextStore::open(&path).unwrap();
        let conn = store.conn();
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name LIKE 'code_graph_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "orphaned code_graph_* tables must be dropped");
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn upsert_node_updates_content_on_touch_keeping_identity() {
        let (_dir, store) = tmp_store();
        let conn = store.conn();
        let a = upsert_node(
            &conn,
            &NodeInput::new(NodeKind::File, "src/lib.rs")
                .with_uri("file:///repo/src/lib.rs")
                .with_content("v1"),
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        let b = upsert_node(
            &conn,
            &NodeInput::new(NodeKind::File, "src/lib.rs")
                .with_uri("file:///repo/src/lib.rs")
                .with_content("v2"),
            "2026-01-02T00:00:00Z",
        )
        .unwrap();
        assert_eq!(a, b, "same identity → same rowid");
        let node = node_by_id(&conn, a).unwrap().unwrap();
        assert_eq!(node.content, "v2");
    }

    #[test]
    fn memory_nodes_and_public_id_lookup_serve_the_inspection_surface() {
        let (_dir, store) = tmp_store();
        {
            let conn = store.conn();
            upsert_node(
                &conn,
                &NodeInput::new(NodeKind::Memory, "prefer rg over grep")
                    .with_content("prefer rg over grep in this workspace"),
                "2026-01-01T00:00:00Z",
            )
            .unwrap();
            upsert_node(
                &conn,
                &NodeInput::new(NodeKind::Memory, "tests live next to code")
                    .with_content("tests live next to code, not in tests/"),
                "2026-01-02T00:00:00Z",
            )
            .unwrap();
            // A non-memory node must never surface in the memory listing.
            upsert_node(
                &conn,
                &NodeInput::new(NodeKind::Concept, "budgeting").with_content("c"),
                "2026-01-03T00:00:00Z",
            )
            .unwrap();
        }
        let memories = store.memory_nodes().unwrap();
        assert_eq!(memories.len(), 2, "only Memory-kind nodes");
        assert_eq!(
            memories[0].display_name, "tests live next to code",
            "newest first"
        );
        assert!(memories.iter().all(|m| m.kind == NodeKind::Memory));

        let looked_up = store
            .node_by_public_id(&memories[0].public_id)
            .unwrap()
            .expect("public id resolves");
        assert_eq!(looked_up.content, "tests live next to code, not in tests/");
        assert!(store.node_by_public_id("nod_missing").unwrap().is_none());
    }

    #[test]
    fn empty_display_name_is_rejected() {
        let (_dir, store) = tmp_store();
        let conn = store.conn();
        let err = upsert_node(
            &conn,
            &NodeInput::new(NodeKind::Concept, "  "),
            "2026-01-01T00:00:00Z",
        )
        .unwrap_err();
        assert!(matches!(err, ContextError::InvalidInput(_)));
    }

    #[test]
    fn vector_blob_roundtrips_little_endian() {
        let v = vec![1.0f32, -2.5, 3.25, 0.0];
        let blob = vector_to_blob(&v);
        assert_eq!(blob.len(), 16);
        assert_eq!(blob_to_vector(&blob).unwrap(), v);
    }

    #[test]
    fn odd_length_blob_is_reported_as_corruption() {
        assert!(matches!(
            blob_to_vector(&[0u8, 1, 2]),
            Err(ContextError::Corruption(_))
        ));
    }

    #[test]
    fn embedding_store_is_idempotent_and_reports_reuse() {
        let (_dir, store) = tmp_store();
        let conn = store.conn();
        let first =
            store_embedding(&conn, "hashA", "fp", &[0.1, 0.2], "2026-01-01T00:00:00Z").unwrap();
        let second =
            store_embedding(&conn, "hashA", "fp", &[0.1, 0.2], "2026-01-01T00:00:00Z").unwrap();
        assert!(first, "first insert writes a row");
        assert!(
            !second,
            "second insert is a no-op (byte-compat reuse, L-C2)"
        );
        assert!(embedding_exists(&conn, "hashA", "fp").unwrap());
        assert!(!embedding_exists(&conn, "hashA", "other-fp").unwrap());
    }

    #[test]
    fn kill_mid_index_rolls_back_to_a_consistent_store() {
        // L-L1: a batch dropped without commit must leave no partial rows.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        {
            let s = ContextStore::open(&path).unwrap();
            let conn = s.conn();
            // Commit one durable node so the file has real content.
            upsert_node(
                &conn,
                &NodeInput::new(NodeKind::Concept, "committed"),
                "2026-01-01T00:00:00Z",
            )
            .unwrap();
        }
        {
            // Start a batch, write several rows, then DROP without commit —
            // the stand-in for a kill mid-index.
            let s = ContextStore::open(&path).unwrap();
            let conn = s.conn();
            let tx = conn.unchecked_transaction().unwrap();
            for i in 0..10 {
                upsert_node(
                    &tx,
                    &NodeInput::new(NodeKind::Concept, format!("partial-{i}")),
                    "2026-01-02T00:00:00Z",
                )
                .unwrap();
            }
            drop(tx); // rollback
        }
        let s = ContextStore::open(&path).unwrap();
        s.integrity_check()
            .expect("store must be consistent after a torn write");
        assert_eq!(
            s.node_count().unwrap(),
            1,
            "only the committed node survives; no partial rows"
        );
    }

    /// An embedder that counts how many texts it was asked to embed, wrapping
    /// the real hashing projection. Lets tests prove where embedding work
    /// happens (`L-C1`: never inline on query) and that identical content is
    /// not re-embedded (`L-C2`).
    struct CountingEmbedder {
        inner: crate::embed::HashEmbedder,
        embedded: std::sync::atomic::AtomicUsize,
    }

    impl CountingEmbedder {
        fn new(revision: &str) -> Arc<Self> {
            Arc::new(Self {
                inner: crate::embed::HashEmbedder::with_revision(revision),
                embedded: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn count(&self) -> usize {
            self.embedded.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl crate::embed::Embedder for CountingEmbedder {
        fn fingerprint(&self) -> EmbedderFingerprint {
            self.inner.fingerprint()
        }
        async fn embed(
            &self,
            texts: &[String],
        ) -> Result<Vec<crate::embed::Embedding>, crate::embed::EmbedError> {
            self.embedded.fetch_add(texts.len(), Ordering::SeqCst);
            self.inner.embed(texts).await
        }
    }

    #[tokio::test]
    async fn recall_never_embeds_stored_content_inline_only_the_query() {
        // L-C1: the first query does not pay indexing. Seed content (embedded
        // once at upsert), reset the counter, then a recall must embed exactly
        // ONE text — the query itself — and nothing stored.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        let embedder = CountingEmbedder::new("1");
        let store =
            ContextStore::open_with(&path, embedder.clone(), Arc::new(SystemClock)).unwrap();
        store
            .upsert(
                crate::writeback::ContextDelta::new()
                    .with_node(NodeInput::new(NodeKind::Concept, "a").with_content("alpha content"))
                    .with_node(NodeInput::new(NodeKind::Concept, "b").with_content("beta content")),
            )
            .await
            .unwrap();
        let before = embedder.count();
        let q = contextgraph_types::ContextQuery {
            goal: "find alpha".into(),
            query_text: Some("alpha content".into()),
            embedding: None,
            kinds: vec![],
            anchors: vec![],
            max_frames: 10,
            max_tokens: 4000,
            as_of: None,
            representation_preferences: vec![],
        };
        store.recall(&q).await.unwrap();
        assert_eq!(
            embedder.count() - before,
            1,
            "recall embeds only the query text, never stored content"
        );
    }

    #[tokio::test]
    async fn open_and_warm_catches_up_embeddings_in_the_background() {
        // L-C1: warm at mount. Seed content under fingerprint rev-1, then mount
        // with a rev-2 embedder whose index is empty for this content. The
        // background warm task embeds it; after joining, a query is
        // vector-grounded (not lexical fallback) — proving warm did the work.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        {
            let a = ContextStore::open_with(
                &path,
                Arc::new(crate::embed::HashEmbedder::with_revision("1")),
                Arc::new(SystemClock),
            )
            .unwrap();
            a.upsert(
                crate::writeback::ContextDelta::new()
                    .with_node(NodeInput::new(NodeKind::Concept, "warmable").with_content(
                        "content that must be re-embedded under the new fingerprint",
                    )),
            )
            .await
            .unwrap();
        }
        let embedder = CountingEmbedder::new("2");
        let store =
            ContextStore::open_and_warm(&path, embedder.clone(), Arc::new(SystemClock)).unwrap();
        let warmed = store.await_warm().await.unwrap();
        assert_eq!(
            warmed, 1,
            "the background task embedded the stale-fingerprint node"
        );
        assert!(
            embedder.count() >= 1,
            "warm did real embedding work off the query path"
        );

        let q = contextgraph_types::ContextQuery {
            goal: "find it".into(),
            query_text: Some("content that must be re-embedded under the new fingerprint".into()),
            embedding: None,
            kinds: vec![],
            anchors: vec![],
            max_frames: 5,
            max_tokens: 2000,
            as_of: None,
            representation_preferences: vec![],
        };
        let result = store.recall(&q).await.unwrap();
        assert!(
            !result.used_lexical_fallback,
            "after warm, retrieval is vector-grounded"
        );
        assert!(!result.frames.is_empty());
    }

    #[tokio::test]
    async fn scoped_recall_keeps_untagged_nodes_and_drops_out_of_scope_ones() {
        // Regression: the post-`stella init` failure mode. A workspace
        // taxonomy makes every recall domain-scoped; most memories are
        // written untagged (reflections with no domain, episodes touching no
        // covered file). The scope must keep those and exclude only nodes
        // tagged exclusively out of scope — the old hard filter returned
        // zero frames forever once a taxonomy existed.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        let store = ContextStore::open_with(
            &path,
            Arc::new(crate::embed::HashEmbedder::default()),
            Arc::new(SystemClock),
        )
        .unwrap();
        store
            .upsert(
                crate::writeback::ContextDelta::new()
                    .with_node(
                        NodeInput::new(NodeKind::Concept, "untagged-lesson")
                            .with_content("prefer rg over grep in shell commands"),
                    )
                    .with_node(
                        NodeInput::new(NodeKind::Concept, "in-scope")
                            .with_content("billing retries use exponential backoff")
                            .with_domains(["billing".to_string()]),
                    )
                    .with_node(
                        NodeInput::new(NodeKind::Concept, "out-of-scope")
                            .with_content("frontend uses tailwind for styling")
                            .with_domains(["frontend".to_string()]),
                    ),
            )
            .await
            .unwrap();

        let q = contextgraph_types::ContextQuery {
            goal: "recall everything".into(),
            query_text: Some("prefer rg over grep billing retries".into()),
            embedding: None,
            kinds: vec![],
            anchors: vec![],
            max_frames: 10,
            max_tokens: 4000,
            as_of: None,
            representation_preferences: vec![],
        };
        let result = store
            .recall_scoped(&q, &["billing".to_string()])
            .await
            .unwrap();
        let titles: Vec<&str> = result.frames.iter().map(|f| f.title.as_str()).collect();
        assert!(
            titles.contains(&"untagged-lesson"),
            "untagged nodes must survive a domain scope: {titles:?}"
        );
        assert!(
            !titles.contains(&"out-of-scope"),
            "nodes tagged exclusively out of scope must be excluded: {titles:?}"
        );
    }

    #[tokio::test]
    async fn warm_now_embeds_only_content_missing_under_the_active_fingerprint() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        let store = ContextStore::open(&path).unwrap();
        {
            let conn = store.conn();
            upsert_node(
                &conn,
                &NodeInput::new(NodeKind::Concept, "thing").with_content("real content here"),
                "2026-01-01T00:00:00Z",
            )
            .unwrap();
        }
        let n = store.warm_now().await.unwrap();
        assert_eq!(n, 1, "one node embedded");
        // A second warm is a no-op — the vector already exists.
        assert_eq!(store.warm_now().await.unwrap(), 0);
    }

    // ==================================================================
    // Phase 0 characterization: `ContextQuery.as_of` bitemporal semantics
    // ==================================================================
    //
    // The adaptive-context plan forbids *assuming* what `as_of` means
    // (knowledge cutoff, world validity, or both). These tests PIN the
    // observed contract of the two temporal readers that back it —
    // `edges_as_of` (the public `facts_as_of`) and `neighbors` (the only
    // consumer of `ContextQuery.as_of` inside `recall`). The characterized
    // contract, which any future bitemporal API must preserve or migrate
    // deliberately:
    //
    //   `as_of` filters on TRANSACTION / BELIEF time ONLY — the half-open
    //   interval `[recorded_at, superseded_at)`:
    //     * `as_of = None`      -> currently believed (`superseded_at IS NULL`)
    //     * `as_of = Some(t)`   -> `recorded_at <= t AND
    //                              (superseded_at IS NULL OR superseded_at > t)`
    //   The start is INCLUSIVE (`t == recorded_at` is visible); the end is
    //   EXCLUSIVE (`t == superseded_at` is NOT visible).
    //
    //   `as_of` DOES NOT consult world-validity (`valid_from` / `valid_to`).
    //   An edge whose world-validity window has closed in the past is still
    //   returned as long as it remains believed. This is the decisive
    //   discriminator between "transaction time" and "world validity / both".

    // Fixed, equal-length UTC instants so lexicographic order over the TEXT
    // columns matches chronological order (the store compares timestamps as
    // strings).
    const T0: &str = "2026-01-01T00:00:00Z"; // before anything is recorded
    const T1: &str = "2026-02-01T00:00:00Z"; // edge recorded (== recorded_at)
    const T1_5: &str = "2026-02-15T00:00:00Z"; // strictly inside [T1, T2)
    const T2: &str = "2026-03-01T00:00:00Z"; // edge superseded (== superseded_at)
    const T3: &str = "2026-04-01T00:00:00Z"; // after supersession / world-validity

    fn concept(conn: &Connection, name: &str) -> i64 {
        upsert_node(conn, &NodeInput::new(NodeKind::Concept, name), T1).unwrap()
    }

    /// Sorted `(src_id, dst_id)` pairs visible to `edges_as_of` at `as_of`.
    fn edge_pairs(conn: &Connection, as_of: Option<&str>) -> Vec<(i64, i64)> {
        let mut v: Vec<(i64, i64)> = edges_as_of(conn, as_of)
            .unwrap()
            .into_iter()
            .map(|e| (e.src_id, e.dst_id))
            .collect();
        v.sort_unstable();
        v
    }

    /// Sorted neighbor ids of `seed` visible to `neighbors` at `as_of`.
    fn neighbor_ids(conn: &Connection, seed: i64, as_of: Option<&str>) -> Vec<i64> {
        let mut v: Vec<i64> = neighbors(conn, &[seed], as_of)
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn as_of_none_returns_only_currently_believed_edges() {
        let (_dir, store) = tmp_store();
        let conn = store.conn();
        let (a, b, c) = (
            concept(&conn, "a"),
            concept(&conn, "b"),
            concept(&conn, "c"),
        );
        let props = serde_json::json!({});
        // Two beliefs recorded at T1: a->b and a->c.
        let e_ab =
            insert_edge(&conn, "relates_to", a, b, 1.0, &props, None, None, T1, None).unwrap();
        insert_edge(&conn, "relates_to", a, c, 1.0, &props, None, None, T1, None).unwrap();
        // a->b is later corrected away (superseded at T2). Never deleted.
        close_edge(&conn, e_ab, T2, T2).unwrap();

        // `None` == "currently believed": a->b is gone, a->c remains.
        assert_eq!(edge_pairs(&conn, None), vec![(a, c)]);
        assert_eq!(neighbor_ids(&conn, a, None), vec![c]);
        // Undirected: c still sees a; b no longer does.
        assert_eq!(neighbor_ids(&conn, c, None), vec![a]);
        assert_eq!(neighbor_ids(&conn, b, None), Vec::<i64>::new());
    }

    #[test]
    fn as_of_reconstructs_half_open_transaction_interval() {
        let (_dir, store) = tmp_store();
        let conn = store.conn();
        let (a, b) = (concept(&conn, "a"), concept(&conn, "b"));
        let props = serde_json::json!({});
        // a->b believed from T1, superseded at T2: valid transaction interval
        // is the half-open [T1, T2).
        let e = insert_edge(&conn, "relates_to", a, b, 1.0, &props, None, None, T1, None).unwrap();
        close_edge(&conn, e, T2, T2).unwrap();

        // Before it was recorded: not yet believed.
        assert_eq!(edge_pairs(&conn, Some(T0)), Vec::<(i64, i64)>::new());
        assert_eq!(neighbor_ids(&conn, a, Some(T0)), Vec::<i64>::new());

        // At exactly recorded_at (T1): INCLUSIVE start -> visible.
        assert_eq!(edge_pairs(&conn, Some(T1)), vec![(a, b)]);
        assert_eq!(neighbor_ids(&conn, a, Some(T1)), vec![b]);

        // Strictly inside [T1, T2): visible.
        assert_eq!(edge_pairs(&conn, Some(T1_5)), vec![(a, b)]);
        assert_eq!(neighbor_ids(&conn, a, Some(T1_5)), vec![b]);

        // At exactly superseded_at (T2): EXCLUSIVE end -> NOT visible.
        // This is the line that pins the interval as half-open, not closed.
        assert_eq!(edge_pairs(&conn, Some(T2)), Vec::<(i64, i64)>::new());
        assert_eq!(neighbor_ids(&conn, a, Some(T2)), Vec::<i64>::new());

        // After supersession: still not visible.
        assert_eq!(edge_pairs(&conn, Some(T3)), Vec::<(i64, i64)>::new());
        assert_eq!(neighbor_ids(&conn, a, Some(T3)), Vec::<i64>::new());
    }

    #[test]
    fn as_of_ignores_world_validity_valid_from_valid_to() {
        // THE DISCRIMINATOR. An edge whose world-validity window closed in the
        // past (`valid_to = T1`) but which is still BELIEVED (`superseded_at IS
        // NULL`) must remain visible to every `as_of` query at or after its
        // `recorded_at`. If `as_of` consulted world validity (or "both"), a
        // query at T3 > valid_to would hide it — it does not. Proves the filter
        // is transaction/belief time only.
        let (_dir, store) = tmp_store();
        let conn = store.conn();
        let (a, b) = (concept(&conn, "a"), concept(&conn, "b"));
        let props = serde_json::json!({});
        // Recorded at T1, world-valid only across [T0, T1], never superseded.
        insert_edge(
            &conn,
            "relates_to",
            a,
            b,
            1.0,
            &props,
            Some(T0), // valid_from
            Some(T1), // valid_to — world-validity ends in the past
            T1,       // recorded_at
            None,     // supersedes -> superseded_at stays NULL (still believed)
        )
        .unwrap();

        // Still believed now, despite a closed world-validity window.
        assert_eq!(edge_pairs(&conn, None), vec![(a, b)]);
        assert_eq!(neighbor_ids(&conn, a, None), vec![b]);

        // And still visible as-of T3, long after valid_to (T1): as_of never
        // reads valid_to.
        assert_eq!(edge_pairs(&conn, Some(T3)), vec![(a, b)]);
        assert_eq!(neighbor_ids(&conn, a, Some(T3)), vec![b]);

        // Only transaction time gates it: before recorded_at it is invisible.
        assert_eq!(edge_pairs(&conn, Some(T0)), Vec::<(i64, i64)>::new());
    }

    // ============================================================
    // Phase 0 fixtures: schema-version migration is a lossless,
    // idempotent replay carrying representative rows
    // ============================================================
    //
    // `open`-ing a legacy `context.db` must migrate it to `SCHEMA_VERSION`
    // without losing data, and re-opening must be a no-op. These fixtures carry
    // the SAME representative rows the `as_of_*` tests characterize (a
    // superseded edge, an edge whose world-validity closed in the past, a
    // memory) so the migration is shown to preserve both the data AND the
    // transaction-time semantics — not just an empty schema.

    /// A raw connection migrated up to `version` with `user_version` stamped,
    /// so `ContextStore::open` sees a legacy db to upgrade. The `node`/`edge`
    /// tables are identical across v1..v3, so the crate's writers apply to any
    /// version >= 1; the `memory` table requires v2.
    fn open_legacy(path: &std::path::Path, version: i64) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(MIGRATION_V1).unwrap();
        if version >= 2 {
            conn.execute_batch(MIGRATION_V2).unwrap();
        }
        conn.pragma_update(None, "user_version", version).unwrap();
        conn
    }

    /// A store stamped by a *newer* stella must be refused, not opened as-is:
    /// episodic memory and the fact graph are not rebuildable, so an older
    /// binary writing into a schema it does not know is unrecoverable data
    /// loss. The message must name the real fault — an out-of-date binary.
    #[test]
    fn rejects_a_context_db_written_by_a_newer_stella() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        {
            // The full current schema, stamped one version past this build —
            // exactly what a newer stella leaves behind.
            let conn = open_legacy(&path, SCHEMA_VERSION + 1);
            conn.execute_batch(MIGRATION_V3).unwrap();
        }

        let Err(err) = ContextStore::open(&path) else {
            panic!("a store stamped by a newer stella must not open");
        };
        assert!(
            matches!(err, ContextError::SchemaTooNew(_)),
            "unexpected error: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains(&(SCHEMA_VERSION + 1).to_string()), "{msg}");
        assert!(msg.contains("binary is out of date"), "{msg}");
    }

    #[test]
    fn migrates_v1_context_db_preserving_bitemporal_edges() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        let (a, b, c) = {
            let conn = open_legacy(&path, 1);
            let props = serde_json::json!({});
            let a = upsert_node(&conn, &NodeInput::new(NodeKind::Concept, "a"), T1).unwrap();
            let b = upsert_node(&conn, &NodeInput::new(NodeKind::Concept, "b"), T1).unwrap();
            let c = upsert_node(&conn, &NodeInput::new(NodeKind::Concept, "c"), T1).unwrap();
            // A belief a->b that was later corrected away (superseded at T2).
            let e_ab =
                insert_edge(&conn, "relates_to", a, b, 1.0, &props, None, None, T1, None).unwrap();
            close_edge(&conn, e_ab, T2, T2).unwrap();
            // A still-believed belief a->c whose world-validity window closed in
            // the past (valid_to = T1) — the `as_of` discriminator row.
            insert_edge(
                &conn,
                "relates_to",
                a,
                c,
                1.0,
                &props,
                Some(T0),
                Some(T1),
                T1,
                None,
            )
            .unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, 1, "fixture really is a v1 db before open");
            (a, b, c)
        };

        // Open through the store: migrate v1 -> SCHEMA_VERSION.
        let store = ContextStore::open(&path).unwrap();
        store.integrity_check().unwrap();
        {
            let conn = store.conn();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, SCHEMA_VERSION, "v1 db upgraded to current");
            // Currently believed = only a->c (a->b superseded; a->c's past
            // valid_to is ignored by transaction-time queries).
            assert_eq!(edge_pairs(&conn, None), vec![(a, c)]);
            // Both edges still physically present, reconstructable as-of T1.
            assert_eq!(edge_pairs(&conn, Some(T1)), vec![(a, b), (a, c)]);
        }

        // Re-open: replay is a no-op — same version, same data.
        drop(store);
        let store2 = ContextStore::open(&path).unwrap();
        let conn = store2.conn();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(edge_pairs(&conn, None), vec![(a, c)]);
    }

    #[test]
    fn migrates_v2_context_db_preserving_memories() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        let mem_public = "mem_phase0fixturememory0001";
        {
            let conn = open_legacy(&path, 2);
            // Production writes a memory as a canonical `memory` row plus a
            // retrievable mirror `node` in one transaction; reproduce both.
            insert_memory(
                &conn,
                mem_public,
                "reflection",
                "prefer rg over grep",
                0.5,
                T1,
            )
            .unwrap();
            upsert_node(
                &conn,
                &NodeInput::new(NodeKind::Memory, "prefer rg over grep")
                    .with_content("prefer rg over grep")
                    .with_uri(format!("memory://{mem_public}")),
                T1,
            )
            .unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, 2, "fixture really is a v2 db before open");
        }

        let store = ContextStore::open(&path).unwrap();
        store.integrity_check().unwrap();
        {
            let conn = store.conn();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, SCHEMA_VERSION, "v2 db upgraded to current");
            // The canonical memory row survived the migration.
            let content: String = conn
                .query_row(
                    "SELECT content FROM memory WHERE public_id = ?1",
                    params![mem_public],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(content, "prefer rg over grep");
        }
        // And it is still retrievable through the mirror node (the recall
        // surface behind `stella memory`).
        let mem_nodes = store.memory_nodes().unwrap();
        assert_eq!(mem_nodes.len(), 1);
        assert_eq!(mem_nodes[0].content, "prefer rg over grep");
    }
}
