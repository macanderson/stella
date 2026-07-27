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
pub(crate) const SCHEMA_VERSION: i64 = 9;

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

/// V4 — index `embedding(fingerprint)`.
///
/// `embedding` is `WITHOUT ROWID` with `PRIMARY KEY (content_hash,
/// fingerprint)`, so `WHERE fingerprint = ?` matches no prefix of the primary
/// key and SQLite could only answer it by scanning the whole clustered index —
/// every blob of every fingerprint, including the stale rows a fingerprint bump
/// leaves behind. Recall runs exactly that predicate on every turn
/// ([`score_nodes_by_vector`]), so the scan was proportional to the store's
/// lifetime embedding count rather than to the active fingerprint's.
///
/// `IF NOT EXISTS` because the index is also created by a fresh v4 store that
/// applied V1..V4 in one transaction.
pub(crate) const MIGRATION_V4: &str = "\
CREATE INDEX IF NOT EXISTS idx_embedding_fingerprint ON embedding(fingerprint);
";

/// V5 — **memory identity moves to lineage** (#712 deliverable 5).
///
/// A memory's identity was the hash of its kind and its content, so changing a
/// word made a *different memory*. The old row stayed live, with its old text,
/// its own vector, and full participation in every future recall — and because
/// the mirror node's uri came from that same hash, the node duplicated too. Two
/// rows, one lesson, both cited.
///
/// `lineage_id` is the durable identity a revision belongs to; `public_id`
/// stays the identity of *this revision*. `superseded_at` closes the previous
/// revision when a new one lands, so a lineage has exactly one live row and the
/// history survives (`L-C3` — nothing is deleted).
///
/// **Lossless.** Every existing row is its own lineage's first revision, so the
/// backfill is `lineage_id = public_id`: no content read, no merge, no id
/// change. A new memory still seeds its lineage from its content hash, so the
/// ids minted before this migration are the ids minted after it and every
/// stored tombstone still resolves.
///
/// **Idempotent at the statement level, not merely at the version ladder.**
/// SQLite has no `ADD COLUMN IF NOT EXISTS`, and a store whose `user_version`
/// was rewound — which is exactly how the migration tests build their fixtures,
/// and what a partial restore looks like — would re-run this and fail on a
/// duplicate column. So the columns are added only if `table_info` says they
/// are absent, and the backfill touches only rows still carrying NULL.
///
/// Duplicate lineages that past edits already created are **not** repaired
/// here. They are indistinguishable from two genuinely different memories
/// without a similarity judgment, and a migration is the wrong place to make
/// one: it runs unattended, on the only copy a user has. `stella memory
/// validate` reports them instead (#711, decision 4).
pub(crate) fn migrate_v5(tx: &Connection) -> Result<(), ContextError> {
    let existing: std::collections::HashSet<String> = {
        let mut stmt = tx.prepare("PRAGMA table_info(memory)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>("name"))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if !existing.contains("lineage_id") {
        tx.execute_batch("ALTER TABLE memory ADD COLUMN lineage_id TEXT;")?;
    }
    if !existing.contains("superseded_at") {
        tx.execute_batch("ALTER TABLE memory ADD COLUMN superseded_at TEXT;")?;
    }
    tx.execute_batch(
        "UPDATE memory SET lineage_id = public_id WHERE lineage_id IS NULL;
         CREATE INDEX IF NOT EXISTS idx_memory_lineage ON memory(lineage_id);",
    )?;
    Ok(())
}

/// V6 — the **compaction watermark** ([`crate::store::compact`]).
///
/// A singleton row recording that this store has been compacted, when, and how
/// much each reclaim class took. It exists for the same reason
/// `compacted_through_execution_id` exists in `stella-store`'s export ledger:
/// once compaction deletes the rows that proved a fact, a scalar has to
/// remember in its place. The fact here is "this store's `embedding` count is
/// no longer its lifetime embedding count" — without the row, a reader
/// comparing vectors to nodes sees a shortfall it cannot tell from a broken
/// index, and `stella memory compact` cannot answer "has this ever run".
///
/// `compacted_at` is written `MAX(existing, new)` and the counters accumulate,
/// so every column can only rise. The timestamps are fixed-width RFC-3339, so
/// SQLite's string `MAX` is a chronological max with no parsing — the same
/// property the bi-temporal readers rely on.
///
/// `CHECK (singleton = 1)` makes "one row" a schema constraint rather than a
/// convention the writer has to keep. `IF NOT EXISTS` for the same reason V4
/// and V5 are statement-level idempotent: a store whose `user_version` was
/// rewound (a partial restore, or the migration fixtures below) must be able to
/// re-run this without failing on an existing table.
pub(crate) const MIGRATION_V6: &str = "\
CREATE TABLE IF NOT EXISTS context_compaction (
    singleton                    INTEGER PRIMARY KEY CHECK (singleton = 1),
    compacted_at                 TEXT NOT NULL,
    orphaned_embeddings          INTEGER NOT NULL DEFAULT 0,
    orphaned_node_domains        INTEGER NOT NULL DEFAULT 0,
    orphaned_edge_domains        INTEGER NOT NULL DEFAULT 0,
    stale_fingerprint_embeddings INTEGER NOT NULL DEFAULT 0
);
";

/// V7 — the **IVF (inverted file) similarity index** ([`crate::ann`]).
///
/// Three tables, all keyed by fingerprint so a re-embed neither reads nor
/// invalidates another embedder's index (`L-C2` again, one level up).
///
/// - `ann_centroid` holds the cluster representatives a query scores against.
///   `posting_count` is carried on the row so choosing a probe width costs `k`
///   integers rather than a `GROUP BY` over every assignment — the probe must
///   not have to read the postings to decide how many postings to read.
/// - `ann_assignment` is the posting list, keyed exactly like `embedding`
///   itself: `(content_hash, fingerprint)`. `idx_ann_assignment_probe` on
///   `(fingerprint, centroid_id)` is what makes a probe an index range scan
///   instead of a table scan — without it the accelerator would be slower than
///   the scan it replaces.
/// - `ann_index_state` is the build watermark: one row per fingerprint saying
///   when the index was built and over how many vectors, which is what lets a
///   reader tell "never indexed" from "indexed and drifted".
///
/// **Liveness is deliberately absent from all three.** A posting is a row about
/// *content*, not about belief: `supersede_node` and `restore_node` flip a
/// node's liveness without touching `embedding`, and an `as_of` query changes
/// the live set with no vector change at all. Because the postings join back
/// through `node` under the shared [`crate::candidates::NODE_AS_OF`] predicate,
/// all three of those operations need **zero index maintenance** — which is the
/// entire reason this is an IVF and not a graph index, where each of them would
/// be an invalidation.
///
/// `IF NOT EXISTS` for the reason V4–V6 have it: a store whose `user_version`
/// was rewound (a partial restore, or the migration fixtures) must be able to
/// re-run this without failing on an existing table.
pub(crate) const MIGRATION_V7: &str = "\
CREATE TABLE IF NOT EXISTS ann_centroid (
    fingerprint   TEXT NOT NULL,
    centroid_id   INTEGER NOT NULL,
    vector        BLOB NOT NULL,
    posting_count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (fingerprint, centroid_id)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS ann_assignment (
    content_hash  TEXT NOT NULL,
    fingerprint   TEXT NOT NULL,
    centroid_id   INTEGER NOT NULL,
    PRIMARY KEY (content_hash, fingerprint)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_ann_assignment_probe
    ON ann_assignment(fingerprint, centroid_id);

CREATE TABLE IF NOT EXISTS ann_index_state (
    fingerprint    TEXT PRIMARY KEY,
    built_at       TEXT NOT NULL,
    vector_count   INTEGER NOT NULL,
    centroid_count INTEGER NOT NULL
);
";

/// V8 — **the lifecycle ledger** (#714, adaptive-context Phase 3), plus the
/// `episode.lineage_id` column ADR 0010 point 6 calls for and Phase 1 did not
/// land.
///
/// ## `context_records` — born canonical, never migrated
///
/// ADR 0010 splits ADR 0005's model from its delivery: `context_records` is the
/// canonical local authority **for the records it owns**, and that set grows
/// monotonically. Observations, proposals, and promotion events have no legacy
/// counterpart anywhere, so they are written here from birth — immutable, JCS-
/// hashed per ADR 0004 — and no migration exists for them because they were
/// never anywhere else. Not one legacy row is rewritten by this migration.
///
/// It lives in `context.db` rather than a new file because ADR 0005 says extend,
/// do not replace: retention policy cannot live in a different database from the
/// data it governs, and the tombstones, memories, and episodes these records
/// reason about are all here.
///
/// **Immutability is enforced by the database, not by convention.** Two triggers
/// abort any `UPDATE` or `DELETE` against the table. Append-only is the entire
/// value of a ledger — a "reconstruct what was believed at time T" answer read
/// out of a table something could have quietly rewritten is worth nothing — and
/// an invariant that holds only as long as every future caller remembers it is
/// not an invariant. A correction is a new revision carrying `supersedes`, which
/// is an INSERT and therefore permitted.
///
/// `record_id` is the identity of one revision; `lineage_id` is the durable
/// identity the revision belongs to, exactly as on `memory`. `body` holds the
/// record's canonical JSON, and `record_hash` is the ADR 0004 hash over it, so
/// any row can be re-verified against its own content without the writer being
/// trusted.
///
/// ## `context_extraction_cursor` — what makes extraction replay-idempotent
///
/// The reflection log is append-only and re-read in full every turn. Without a
/// cursor, every pass would re-extract every line and mint duplicate
/// observations forever. The cursor records how far each evidence source has
/// been consumed; combined with the deterministic, content-derived `record_id`,
/// re-running an extraction over already-consumed evidence is a no-op rather
/// than a duplicate.
///
/// ## `episode.lineage_id` — closing a documented/actual mismatch
///
/// ADR 0010 point 6 (ratified 2026-07-26) says `lineage_id` lands on **`memory`
/// and `episode`**, and the plan document says Phase 1 delivered exactly that.
/// It did not: `migrate_v5` altered `memory` alone, and `episode` has no
/// lineage column anywhere in the tree.
///
/// Resolved in favor of the ratified ADR rather than by amending it down to
/// match the code. The ADR's reasoning — that `memory` and `episode` are the two
/// record kinds that transfer authority, while `node`/`edge`/`embedding` are a
/// disposable index — is unaffected by the omission, and re-opening a ratified
/// decision to legalize an implementation gap is the wrong direction. The
/// backfill is the same lossless one v5 used for `memory`: every existing row is
/// its own lineage's first revision, so `lineage_id = public_id`. No content is
/// read, nothing is merged, and no id changes.
///
/// Statement-level idempotent for the same reason `migrate_v5` is: SQLite has no
/// `ADD COLUMN IF NOT EXISTS`, and a store whose `user_version` was rewound —
/// how the migration tests build fixtures, and what a partial restore looks like
/// — would otherwise fail on a duplicate column.
fn migrate_v8(tx: &Connection) -> Result<(), ContextError> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS context_records (
            record_id      TEXT PRIMARY KEY,
            lineage_id     TEXT NOT NULL,
            record_kind    TEXT NOT NULL,
            record_hash    TEXT NOT NULL,
            schema_version TEXT NOT NULL,
            body           TEXT NOT NULL,
            observed_at    TEXT NOT NULL,
            recorded_at    TEXT NOT NULL,
            supersedes     TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_context_records_kind
            ON context_records(record_kind);
        CREATE INDEX IF NOT EXISTS idx_context_records_lineage
            ON context_records(lineage_id);
        CREATE INDEX IF NOT EXISTS idx_context_records_observed
            ON context_records(observed_at);

        CREATE TRIGGER IF NOT EXISTS context_records_are_immutable_update
        BEFORE UPDATE ON context_records BEGIN
            SELECT RAISE(ABORT, 'context_records is append-only: supersede with a new revision instead of updating');
        END;
        CREATE TRIGGER IF NOT EXISTS context_records_are_immutable_delete
        BEFORE DELETE ON context_records BEGIN
            SELECT RAISE(ABORT, 'context_records is append-only: records are never deleted');
        END;

        CREATE TABLE IF NOT EXISTS context_extraction_cursor (
            source     TEXT PRIMARY KEY,
            position   TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );",
    )?;

    let existing: std::collections::HashSet<String> = {
        let mut stmt = tx.prepare("PRAGMA table_info(episode)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>("name"))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if !existing.contains("lineage_id") {
        tx.execute_batch("ALTER TABLE episode ADD COLUMN lineage_id TEXT;")?;
    }
    if !existing.contains("superseded_at") {
        tx.execute_batch("ALTER TABLE episode ADD COLUMN superseded_at TEXT;")?;
    }
    tx.execute_batch(
        "UPDATE episode SET lineage_id = public_id WHERE lineage_id IS NULL;
         CREATE INDEX IF NOT EXISTS idx_episode_lineage ON episode(lineage_id);",
    )?;
    Ok(())
}

/// V9 — **`node.recall_tier`**, the precedence band a node competes in once
/// the recall budget binds ([`crate::retrieval::RecallTier`]).
///
/// The reflection lifecycle mines two very different things and, until now,
/// stored them identically: durable facts about the codebase, and notes about
/// how the agent went about its turn. A five-frame budget against a corpus that
/// only grows means the two compete for the same slots, and a note that is true
/// of one turn was winning slots from a convention that is true next week.
///
/// The tier lives on `node` rather than on `memory` because `node` is the table
/// retrieval actually ranks. `memory.kind` already exists and is already
/// indexed, but nothing on the recall path reads it: the ranking runs over
/// [`crate::candidates::live_node_metas`], a whole-corpus scan, and reaching
/// `memory` from there is a join per turn against a table recall otherwise
/// never touches. A column on the row being scanned is one more value in a
/// `SELECT` that already runs.
///
/// **`DEFAULT 0` is the whole migration.** `RecallTier::Normal` is 0 and is the
/// enum's `Default`, so every node written before this — every symbol, every
/// episode, every memory — keeps competing on rank exactly as it did, and there
/// is no backfill to get wrong. Only a writer that explicitly asks for
/// `Deferred` is treated differently, and nothing is ever *promoted* by a tier.
///
/// Statement-level idempotent for the reason `migrate_v5` and `migrate_v8` are:
/// SQLite has no `ADD COLUMN IF NOT EXISTS`, and a store whose `user_version`
/// was rewound — how the migration tests build their fixtures, and what a
/// partial restore looks like — would otherwise fail on a duplicate column.
fn migrate_v9(tx: &Connection) -> Result<(), ContextError> {
    let existing: std::collections::HashSet<String> = {
        let mut stmt = tx.prepare("PRAGMA table_info(node)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>("name"))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if !existing.contains("recall_tier") {
        tx.execute_batch("ALTER TABLE node ADD COLUMN recall_tier INTEGER NOT NULL DEFAULT 0;")?;
    }
    Ok(())
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
/// **The version is read inside a `BEGIN IMMEDIATE` transaction**, and that is
/// load-bearing rather than incidental. Read outside — or inside a `DEFERRED`
/// transaction, which takes its snapshot before it takes the write lock — two
/// processes opening the same fresh workspace at once (a fleet run, or a
/// `stella` session next to a `stella stats`) could both read
/// `user_version = 0` and both try to apply `MIGRATION_V1`; the loser reported
/// SQLITE_BUSY or "table node already exists" instead of the "already migrated"
/// no-op it should (#617 item 8, matching `stella_store::migrations`).
pub(crate) fn migrate(conn: &Connection) -> Result<(), ContextError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
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
    if version < 1 {
        tx.execute_batch(MIGRATION_V1)?;
    }
    if version < 2 {
        tx.execute_batch(MIGRATION_V2)?;
    }
    if version < 3 {
        tx.execute_batch(MIGRATION_V3)?;
    }
    if version < 4 {
        tx.execute_batch(MIGRATION_V4)?;
    }
    if version < 5 {
        migrate_v5(&tx)?;
    }
    if version < 6 {
        tx.execute_batch(MIGRATION_V6)?;
    }
    if version < 7 {
        tx.execute_batch(MIGRATION_V7)?;
    }
    if version < 8 {
        migrate_v8(&tx)?;
    }
    if version < 9 {
        migrate_v9(&tx)?;
    }
    // ── APPEND POINT — RESERVED SLOT ────────────────────────────────────
    // This is an ordered `if version < N` ladder and `SCHEMA_VERSION` is its
    // high-water mark. Two branches that each add "the next step" merge
    // cleanly — git sees two additions to adjacent lines — and produce a
    // ladder with two steps numbered the same, or a `SCHEMA_VERSION` that
    // skips one. Nothing in CI catches that: both files compile, and the
    // mis-numbering only shows up as a corrupt context.db on a user's
    // machine.
    //
    // Adaptive context is being built on two branches in parallel, so the
    // slot is reserved here in advance:
    //
    //   v6: adaptive-context Phase 3 (#714) — TAKEN, see `migrate_v6`
    //
    // If you are not that phase, take v7 and add your own line here.
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

// Low-level accessors (pub(crate)) shared by retrieval.rs and writeback.rs.
// All take a `&Connection` (a `&Transaction` derefs to one), so a caller can
// batch many of them inside a single transaction.
