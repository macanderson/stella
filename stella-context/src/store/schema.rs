//! Schema, migrations, and the connection pragmas — everything that decides
//! what the file on disk looks like before any row is read or written.
//!
//! Split out of the store module unchanged (#712 deliverable 1): the DDL, the
//! version ladder, the owner-only permission narrowing, and the fingerprint
//! registry are one seam, and they are the part a reader consults when asking
//! "what is actually in this database".

use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

use crate::embed::EmbedderFingerprint;
use crate::error::ContextError;

/// The current on-disk schema version, tracked in `PRAGMA user_version`.
pub(crate) const SCHEMA_VERSION: i64 = 3;

/// The v1 schema. Applied once, inside the migration transaction. Bi-temporal
/// columns (`valid_from`/`valid_to`/`recorded_at`/`superseded_at`) exist on both
/// `node` and `edge`, but only EDGES are actually versioned: `apply_fact`
/// closes an edge's interval on supersession, never deleting (`L-C3`), and
/// `facts_as_of` reads history back. NODES are mutable current-state —
/// `upsert_node` overwrites content in place, so their time columns stay
/// effectively unused and there is no point-in-time node reader. Fact history is
/// recoverable; node content history is not.
pub(crate) const MIGRATION_V1: &str = "\
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
pub(crate) const MIGRATION_V2: &str = "\
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
pub(crate) const MIGRATION_V3: &str = "\
DROP TABLE IF EXISTS code_graph_symbols;
DROP TABLE IF EXISTS code_graph_imports;
DROP TABLE IF EXISTS code_graph_files;
";

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
pub(crate) fn migrate(conn: &Connection) -> Result<(), ContextError> {
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
pub(crate) fn register_fingerprint(
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
