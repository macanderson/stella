//! SQLite storage for the code graph.
//!
//! The graph lives in its own file (`.stella/private/codegraph.db` by
//! convention) — it used to share `stella-context`'s `context.db`, which is
//! why every table here is still prefixed `code_graph_` and why
//! `stella-context` carries a migration that evicts those tables from a
//! legacy `context.db`. The prefix stays: it costs nothing and keeps the two
//! schemas non-colliding if they ever share a file again. The store is opened
//! against a caller-supplied path, so nothing in this crate hard-codes the
//! location.
//!
//! Durability contract (L-L1, "a kill during indexing leaves a consistent
//! store"): WAL journal mode, and **every index batch is
//! a single transaction**. A process killed mid-batch has committed nothing;
//! reopening sees the previous consistent state and a re-index completes. The
//! crash-consistency test in this module proves it by dropping a transaction
//! (rusqlite rolls back on drop) and asserting the store is unchanged.
//!
//! Byte-compat skip (L-C2): a file whose content
//! sha256 matches the stored value is never re-parsed. [`IndexStats::files_parsed`]
//! counts real parse invocations, which the skip test asserts drops to zero on
//! an unchanged second pass.
//!
//! Schema: `codegraph.db` is versioned by *convergence* — every statement in
//! [`MIGRATION`] is `CREATE … IF NOT EXISTS` and the whole batch replays on
//! every writer [`open`], so an additive table or index reaches an existing
//! store the next time it is opened. Adding a table or an index here is the
//! migration. That is acceptable specifically because this file is a
//! **cache**: it is fully rebuildable from the tree by `stella init`.
//!
//! It nevertheless carries a **stamp** ([`SCHEMA_VERSION`] in `PRAGMA
//! user_version`, #617). Convergence cannot express a *reshape* — an altered
//! or backfilled column, which the `IF NOT EXISTS` guard silently skips on an
//! existing store — and without a version recorded on the file there is no
//! way for a future stella to know which stores need one. The stamp costs one
//! integer in the header and is the difference between "we can migrate this
//! later" and "we can only ever tell the user to delete it". It also makes
//! the downgrade case explicit: a `codegraph.db` written by a newer stella is
//! refused with a message that says which side is out of date, rather than
//! being written into by code that does not know its shape.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use crate::error::GraphError;
use crate::generated::{self, GeneratedFilter};
use crate::import::{self, ImportKind};
use crate::lang::Language;
use crate::manifest::StorageManifest;
use crate::parse::{Grammars, parse_file};
use crate::storage::{self, FieldEntry, RelationEntry};
use crate::symbol::SymbolKind;
use crate::walk::walk_indexable;

/// The on-disk schema version stamped in `PRAGMA user_version`. Bump it in
/// the same commit as a **reshape** — an altered or backfilled column,
/// anything the `IF NOT EXISTS` guard would silently skip on an existing
/// store. A purely *additive* table or index is what convergence already
/// expresses (the whole [`MIGRATION`] batch replays on every writer open),
/// so it ships **without** a bump — bumping would make every older binary
/// refuse a store a newer one touched
/// ([`stella_store::durable::ensure_converged_schema_version`] fails closed
/// on a future stamp) for no protection gained. Precedent: the #1214
/// storage indexes and the #335 `code_graph_calls` table, both additive,
/// both unbumped.
const SCHEMA_VERSION: i64 = 1;

/// The tables [`MIGRATION`] must have produced before the version is stamped.
/// A store missing one of these is not stamped — see
/// [`stella_store::durable::ensure_converged_schema_version`].
const SCHEMA_TABLES: &[&str] = &[
    "code_graph_files",
    "code_graph_symbols",
    "code_graph_imports",
    "code_graph_calls",
    "code_graph_storage_objects",
];

/// DDL for the code graph's tables. `IF NOT EXISTS` throughout so opening an
/// existing store (possibly a legacy file that already carries another
/// crate's tables) is a no-op.
const MIGRATION: &str = r#"
CREATE TABLE IF NOT EXISTS code_graph_files (
    id             INTEGER PRIMARY KEY,
    path           TEXT NOT NULL UNIQUE,
    language       TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    mtime_ns       INTEGER NOT NULL,
    indexed_at     INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS code_graph_symbols (
    id         INTEGER PRIMARY KEY,
    file_id    INTEGER NOT NULL REFERENCES code_graph_files(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS code_graph_imports (
    id           INTEGER PRIMARY KEY,
    from_file_id INTEGER NOT NULL REFERENCES code_graph_files(id) ON DELETE CASCADE,
    specifier    TEXT NOT NULL,
    to_path      TEXT,
    kind         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS code_graph_symbols_name ON code_graph_symbols(name);
CREATE INDEX IF NOT EXISTS code_graph_symbols_file ON code_graph_symbols(file_id);
CREATE INDEX IF NOT EXISTS code_graph_imports_from ON code_graph_imports(from_file_id);
CREATE INDEX IF NOT EXISTS code_graph_imports_to   ON code_graph_imports(to_path);
-- Call sites (#335, B1): unresolved name-based edges. `callee_name` is the
-- bare identifier at the call site; `line` is 1-based. `callers` filters on
-- the name, `callees` on (file, line-range) — one index each.
CREATE TABLE IF NOT EXISTS code_graph_calls (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER NOT NULL REFERENCES code_graph_files(id) ON DELETE CASCADE,
    callee_name TEXT NOT NULL,
    line        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS code_graph_calls_name ON code_graph_calls(callee_name);
CREATE INDEX IF NOT EXISTS code_graph_calls_file ON code_graph_calls(file_id, line);
CREATE TABLE IF NOT EXISTS code_graph_storage_objects (
    id            INTEGER PRIMARY KEY,
    file_id       INTEGER NOT NULL REFERENCES code_graph_files(id) ON DELETE CASCADE,
    parent_id     INTEGER REFERENCES code_graph_storage_objects(id) ON DELETE CASCADE,
    address       TEXT NOT NULL,
    layer         TEXT NOT NULL,
    namespace     TEXT NOT NULL,
    level         TEXT NOT NULL,
    kind          TEXT NOT NULL,
    display_name  TEXT NOT NULL,
    data_type     TEXT,
    nullable      INTEGER,
    default_value TEXT,
    constraints   TEXT,
    ref_target    TEXT,
    comment       TEXT,
    start_line    INTEGER NOT NULL,
    end_line      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS code_graph_storage_addr ON code_graph_storage_objects(address);
CREATE INDEX IF NOT EXISTS code_graph_storage_file ON code_graph_storage_objects(file_id);
-- Every read in `storage_rows` filters on `level` and orders by `address`, and
-- the pre-write gate runs that pair on the agent's write path; `parent_id` is
-- the self-referencing CASCADE parent, which SQLite scans on every delete.
CREATE INDEX IF NOT EXISTS code_graph_storage_level  ON code_graph_storage_objects(level, address);
CREATE INDEX IF NOT EXISTS code_graph_storage_parent ON code_graph_storage_objects(parent_id);
"#;

/// Largest source file this index will read, in bytes.
///
/// Nothing upstream bounds file size: [`crate::walk`]'s deny-list is
/// structural (directory names), and `generated::looks_minified` only catches
/// long-LINE shapes. A newline-per-statement `dump.sql`, a bindgen/protobuf
/// output committed outside any vendor directory, or a large fixture with a
/// recognized extension therefore reached `fs::read` whole and then
/// tree-sitter — whose syntax tree runs several times the source size, so the
/// real peak is a multiple of the file. One such file in a workspace could
/// take the process out during `stella init`, and the crash is attributed to
/// indexing rather than to the file.
///
/// 4 MiB sits far above any hand-written source file (the largest file in this
/// workspace is ~210 KiB) while capping the peak at a few tens of MiB. A file
/// over it is counted in [`IndexStats::files_skipped_too_large`] and skipped —
/// never an error, matching every other per-file failure mode here (L-L1).
pub(crate) const MAX_INDEXABLE_BYTES: u64 = 4 * 1024 * 1024;

/// Outcome of one index pass. `files_parsed` is the honest parse-invocation
/// count the byte-compat skip test asserts against.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IndexStats {
    pub files_seen: usize,
    pub files_parsed: usize,
    pub files_skipped_unchanged: usize,
    pub files_skipped_binary: usize,
    /// Files excluded as generated/minified (`crate::generated::is_excluded`)
    /// — `.gitattributes` `linguist-generated=true`, the `*.min.*` filename
    /// convention, or the minified-content heuristic. Surfaced by `stella
    /// init` as "skipped N generated files" (issue #272).
    pub files_skipped_generated: usize,
    /// Files skipped for exceeding the 4 MiB indexing ceiling — never read, so
    /// they cost a `stat` rather than their own bytes plus a syntax tree.
    ///
    /// The ceiling is deliberately not public: it is a memory-safety bound on
    /// tree-sitter, whose tree runs several times the size of its source, not
    /// a promise about which files get indexed.
    pub files_skipped_too_large: usize,
    pub files_unreadable: usize,
    pub parse_failures: usize,
    pub files_pruned: usize,
    pub symbols: usize,
    pub imports: usize,
    pub calls: usize,
}

/// One symbol row joined to its file — the shape every symbol-producing query
/// returns.
#[derive(Debug, Clone)]
pub(crate) struct DefRow {
    pub path: String,
    pub sha: String,
    pub name: String,
    pub kind: SymbolKind,
    pub start_line: u32,
    pub end_line: u32,
}

/// One import edge out of a file.
#[derive(Debug, Clone)]
pub(crate) struct ImportRow {
    pub specifier: String,
    pub to_path: Option<String>,
    pub kind: ImportKind,
}

/// Open the store at `db_path` with the per-connection pragmas but **without**
/// running [`MIGRATION`] — the read path. `stella-graph`'s DDL is idempotent
/// convergence, so running it is harmless but never free: it is the whole
/// `CREATE …` batch, and [`crate::load_storage_snapshot`] (the pre-write gate)
/// opens the store on *every* write an agent proposes. Readers get the schema
/// that is already there; a store no writer has migrated yet simply has no rows
/// to read.
pub(crate) fn open_read(db_path: &Path) -> Result<Connection, GraphError> {
    let conn = Connection::open(db_path)?;
    // WAL persists in the file; foreign_keys / busy_timeout are per-connection
    // and must be re-set on every open. NORMAL sync is the standard WAL
    // durability/throughput trade for a rebuildable index.
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;\
         PRAGMA foreign_keys=ON;\
         PRAGMA busy_timeout=5000;\
         PRAGMA synchronous=NORMAL;",
    )?;
    Ok(conn)
}

/// [`open_read`] plus [`MIGRATION`] — the writer's open. Safe to call against a
/// file that already holds another crate's tables.
///
/// The `user_version` stamp lives here rather than in [`open_read`] for two
/// reasons: stamping is a write, and putting one on the read path would make
/// the pre-write gate take a write lock on every proposed edit; and only the
/// writer has just run [`MIGRATION`], which is what makes the store's shape
/// provably match the version being stamped.
///
/// A store that fails to open because its disk image is damaged is not an
/// error here: the graph is a cache, fully rebuildable from the tree, so the
/// corrupt file is quarantined through
/// [`stella_store::integrity::quarantine_corrupt_store`] (which salvages what
/// is still readable) and a fresh store is opened in its place. Any OTHER
/// failure — permissions, a future schema stamp — still propagates.
pub(crate) fn open(db_path: &Path) -> Result<Connection, GraphError> {
    match open_migrated(db_path) {
        Ok(conn) => Ok(conn),
        Err(error) if is_corruption(&error) => {
            let quarantine = stella_store::integrity::quarantine_corrupt_store(db_path)
                .map_err(|quarantine_error| {
                    GraphError::Sqlite(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                        Some(format!(
                            "{error} — and quarantining the damaged store failed: {quarantine_error}"
                        )),
                    ))
                })?;
            // `moved` always leads with the database itself, but the shape of
            // another crate's return value is not something to `unwrap` on:
            // a quarantine that moved nothing still leaves the rebuild valid,
            // so the notice degrades rather than the open panicking.
            match quarantine.moved.first() {
                Some((_, quarantined)) => eprintln!(
                    "code-graph store was corrupt; moved it aside to {} — rebuilding from scratch",
                    quarantined.display()
                ),
                None => eprintln!("code-graph store was corrupt — rebuilding from scratch"),
            }
            open_migrated(db_path)
        }
        Err(error) => Err(error),
    }
}

/// The unmolested open-and-migrate [`open`] retries after a quarantine.
fn open_migrated(db_path: &Path) -> Result<Connection, GraphError> {
    let conn = open_read(db_path)?;
    conn.execute_batch(MIGRATION)?;
    stella_store::durable::ensure_converged_schema_version(
        &conn,
        "codegraph.db",
        SCHEMA_VERSION,
        SCHEMA_TABLES,
    )
    .map_err(|error| GraphError::Schema(error.to_string()))?;
    Ok(conn)
}

/// Does this error mean the on-disk image is damaged — the one failure a
/// rebuildable cache may answer by starting over? Matches the primary code
/// (`SQLITE_CORRUPT`, `SQLITE_NOTADB`) or, failing that, the extended code,
/// so a `SQLITE_IOERR_*` whose extended bits carry `CORRUPT` still counts
/// while a plain I/O error never does.
fn is_corruption(error: &GraphError) -> bool {
    let GraphError::Sqlite(rusqlite::Error::SqliteFailure(code, _)) = error else {
        return false;
    };
    matches!(
        code.code,
        rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
    ) || matches!(
        code.extended_code & 0xff,
        x if x == rusqlite::ffi::SQLITE_CORRUPT || x == rusqlite::ffi::SQLITE_NOTADB
    )
}

/// Full incremental index of `root`: walk, re-parse only changed files
/// (byte-compat skip), and prune rows for files that no longer exist. One
/// transaction (L-L1).
pub(crate) fn index_tree(
    conn: &mut Connection,
    root: &Path,
    grammars: &Grammars,
) -> Result<IndexStats, GraphError> {
    let files = walk_indexable(root);
    let mut stats = IndexStats::default();
    // Manifest layer mapping and the generated-file filter are each loaded
    // once per pass; a malformed manifest or absent `.gitattributes` degrades
    // to the implicit layer / no-op filter, never aborts the batch (L-L1).
    let manifest = StorageManifest::load(root).ok().flatten();
    let generated_filter = GeneratedFilter::load(root);
    // The Rust crate layout is lazy per pass: the first Rust file pays the
    // manifest scan, a pass with none never does (#443).
    let rust_layout = std::cell::OnceCell::new();
    let tx = conn.transaction()?;

    let mut current: HashSet<String> = HashSet::with_capacity(files.len());
    for abs in &files {
        current.insert(rel_path(root, abs));
        index_one(
            &tx,
            root,
            grammars,
            manifest.as_ref(),
            &generated_filter,
            &rust_layout,
            abs,
            &mut stats,
        )?;
    }
    stats.files_pruned += prune_missing(&tx, &current)?;

    tx.commit()?;
    Ok(stats)
}

/// Apply a specific set of changed paths (from the watcher): index the ones
/// that still exist and are indexable, drop rows for the ones that vanished.
/// One transaction; no full-tree prune.
pub(crate) fn apply_changes(
    conn: &mut Connection,
    root: &Path,
    grammars: &Grammars,
    changed: &[PathBuf],
) -> Result<IndexStats, GraphError> {
    let mut stats = IndexStats::default();
    let manifest = StorageManifest::load(root).ok().flatten();
    let generated_filter = GeneratedFilter::load(root);
    let rust_layout = std::cell::OnceCell::new();
    let tx = conn.transaction()?;
    for abs in changed {
        if abs.is_file() {
            if Language::from_path(abs).is_none() && !storage::indexes_without_language(abs) {
                continue;
            }
            index_one(
                &tx,
                root,
                grammars,
                manifest.as_ref(),
                &generated_filter,
                &rust_layout,
                abs,
                &mut stats,
            )?;
        } else {
            // A vanished path may have been a file OR a directory, and the
            // event does not say which. A removed directory has no extension
            // for the language filter to accept — which is why the prune arm
            // sits ahead of that filter — and its indexed children are only
            // reachable by prefix, so the exact match and the prefix match
            // run as one delete. `ON DELETE CASCADE` clears symbols and
            // imports either way.
            let rel = rel_path(root, abs);
            stats.files_pruned += tx.execute(
                "DELETE FROM code_graph_files WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'",
                params![rel, format!("{}/%", escape_like(&rel))],
            )?;
        }
    }
    tx.commit()?;
    Ok(stats)
}

/// Index one file into an open transaction. Every failure mode here
/// (unreadable, non-UTF-8, unparseable) is recorded in `stats` and skipped —
/// never propagated — so one bad file cannot abort the batch (L-L1).
#[allow(clippy::too_many_arguments)] // two call sites, both index passes
fn index_one(
    tx: &Transaction,
    root: &Path,
    grammars: &Grammars,
    manifest: Option<&StorageManifest>,
    generated_filter: &GeneratedFilter,
    rust_layout: &std::cell::OnceCell<crate::rust_resolve::RustLayout>,
    abs: &Path,
    stats: &mut IndexStats,
) -> Result<(), GraphError> {
    stats.files_seen += 1;
    let rel = rel_path(root, abs);

    // Size gate BEFORE the read, so an oversize file costs a `stat` rather
    // than its own bytes plus a syntax tree several times larger. See
    // [`MAX_INDEXABLE_BYTES`]. A file already indexed while small and since
    // grown past the cap has its row dropped, the same retroactive cleanup
    // the generated-exclusion branch below does — otherwise its stale
    // symbols would answer queries forever.
    if let Ok(metadata) = std::fs::metadata(abs)
        && metadata.len() > MAX_INDEXABLE_BYTES
    {
        stats.files_skipped_too_large += 1;
        stats.files_pruned +=
            tx.execute("DELETE FROM code_graph_files WHERE path = ?1", params![rel])?;
        return Ok(());
    }

    let content = match std::fs::read(abs) {
        Ok(bytes) => bytes,
        Err(_) => {
            stats.files_unreadable += 1;
            return Ok(());
        }
    };

    // Generated/minified exclusion (issue #272) runs **before** the
    // byte-compat skip below, on purpose: a file already sitting in an index
    // built before this filter existed would otherwise never be re-evaluated
    // (its bytes have not changed, so the skip would keep hiding it forever).
    // Deleting any pre-existing row here retroactively cleans that case out
    // on the very next index pass — the same row removal `prune_missing`
    // does for a file that vanished from disk.
    if generated::is_excluded(generated_filter, &rel, &content) {
        stats.files_skipped_generated += 1;
        stats.files_pruned +=
            tx.execute("DELETE FROM code_graph_files WHERE path = ?1", params![rel])?;
        return Ok(());
    }

    let sha = sha256_hex(&content);

    // Byte-compat skip (L-C2): identical content is never re-parsed.
    if let Some(existing) = file_sha(tx, &rel)?
        && existing == sha
    {
        stats.files_skipped_unchanged += 1;
        return Ok(());
    }

    let text = match std::str::from_utf8(&content) {
        Ok(text) => text,
        Err(_) => {
            stats.files_skipped_binary += 1;
            return Ok(());
        }
    };
    let Some(lang) = Language::from_path(abs) else {
        // Storage-DSL files no grammar claims (`.prisma`): no symbols or
        // imports, but the storage adapter still indexes them (spec §4a).
        if storage::indexes_without_language(abs) {
            let file_id = upsert_file(tx, &rel, "prisma", &sha, mtime_ns(abs))?;
            tx.execute(
                "DELETE FROM code_graph_storage_objects WHERE file_id = ?1",
                params![file_id],
            )?;
            let extract = storage::extract_for_path(grammars, &rel, text);
            let claim = manifest.and_then(|m| m.layer_claim(&rel));
            insert_storage_extract(tx, file_id, claim.as_deref(), &extract)?;
            stats.files_parsed += 1;
        }
        return Ok(());
    };
    let parsed = match parse_file(grammars, lang, text) {
        Some(parsed) => parsed,
        None => {
            stats.parse_failures += 1;
            return Ok(());
        }
    };
    stats.files_parsed += 1;

    let edges = import::resolve(parsed.imports, root, abs, rust_layout);
    let file_id = upsert_file(tx, &rel, lang.tag(), &sha, mtime_ns(abs))?;
    tx.execute(
        "DELETE FROM code_graph_symbols WHERE file_id = ?1",
        params![file_id],
    )?;
    tx.execute(
        "DELETE FROM code_graph_imports WHERE from_file_id = ?1",
        params![file_id],
    )?;
    tx.execute(
        "DELETE FROM code_graph_calls WHERE file_id = ?1",
        params![file_id],
    )?;

    // `prepare_cached`, not `execute`: the row loops below run once per symbol
    // and once per edge, and `Connection::execute` re-prepares (and discards)
    // its statement on every call. The cache lives on the connection, so the
    // compile cost is paid once for the whole batch, not once per row.
    {
        let mut insert_symbol = tx.prepare_cached(
            "INSERT INTO code_graph_symbols(file_id, name, kind, start_line, end_line) \
             VALUES(?1, ?2, ?3, ?4, ?5)",
        )?;
        for symbol in &parsed.symbols {
            insert_symbol.execute(params![
                file_id,
                symbol.name,
                symbol.kind.tag(),
                symbol.start_line,
                symbol.end_line
            ])?;
        }
    }
    {
        let mut insert_edge = tx.prepare_cached(
            "INSERT INTO code_graph_imports(from_file_id, specifier, to_path, kind) \
             VALUES(?1, ?2, ?3, ?4)",
        )?;
        for edge in &edges {
            insert_edge.execute(params![
                file_id,
                edge.specifier,
                edge.to_path,
                edge.kind.tag()
            ])?;
        }
    }
    {
        let mut insert_call = tx.prepare_cached(
            "INSERT INTO code_graph_calls(file_id, callee_name, line) VALUES(?1, ?2, ?3)",
        )?;
        for call in &parsed.calls {
            insert_call.execute(params![file_id, call.callee, call.line])?;
        }
    }
    stats.symbols += parsed.symbols.len();
    stats.imports += edges.len();
    stats.calls += parsed.calls.len();

    // Storage adapters (deep pass, spec §6): the same transaction that
    // replaced this file's symbols replaces its storage rows. Extraction is
    // dispatched by path — SQL DDL, Prisma, TS/JS ORMs, Python ORMs — and
    // marker-gated inside each adapter, so a schema-free source file costs
    // a substring scan.
    tx.execute(
        "DELETE FROM code_graph_storage_objects WHERE file_id = ?1",
        params![file_id],
    )?;
    let extract = storage::extract_for_path(grammars, &rel, text);
    if !extract.is_empty() {
        let claim = manifest.and_then(|m| m.layer_claim(&rel));
        insert_storage_extract(tx, file_id, claim.as_deref(), &extract)?;
    }
    Ok(())
}

/// Persist one file's extracted storage entities: relation rows with their
/// field children, plus parentless field rows for `ALTER TABLE … ADD COLUMN`
/// (attached to their relation by address at snapshot-assembly time).
/// `claim` is the manifest layer whose `paths` glob claims this file — it
/// wins over every relation's own `layer_hint`; with neither, relations
/// land in the implicit relational layer (spec §4a).
fn insert_storage_extract(
    tx: &Transaction,
    file_id: i64,
    claim: Option<&str>,
    extract: &storage::StorageExtract,
) -> Result<(), GraphError> {
    for rel in &extract.relations {
        let layer = claim
            .or(rel.layer_hint.as_deref())
            .unwrap_or(storage::DEFAULT_SQL_LAYER);
        let address = storage::relation_address(layer, &rel.namespace, &rel.name);
        let rel_id: i64 = tx.query_row(
            "INSERT INTO code_graph_storage_objects(\
                 file_id, parent_id, address, layer, namespace, level, kind, \
                 display_name, constraints, comment, start_line, end_line) \
             VALUES(?1, NULL, ?2, ?3, ?4, 'relation', ?5, ?6, ?7, ?8, ?9, ?10) \
             RETURNING id",
            params![
                file_id,
                address,
                storage::normalize_name(layer),
                storage::normalize_name(&rel.namespace),
                rel.kind.tag(),
                rel.name,
                // Relations reuse the constraints column for their enum
                // variants (JSON array; empty for tables/views).
                serde_json::to_string(&rel.enum_values).unwrap_or_default(),
                rel.comment,
                rel.start_line,
                rel.end_line,
            ],
            |row| row.get(0),
        )?;
        for field in &rel.fields {
            insert_storage_field(tx, file_id, Some(rel_id), &address, layer, rel, field)?;
        }
    }
    for addition in &extract.additions {
        // Additions only come from SQL ALTER statements today, so the
        // relational fallback applies when no manifest layer claims the file.
        let layer = claim.unwrap_or(storage::DEFAULT_SQL_LAYER);
        let address = storage::relation_address(layer, &addition.namespace, &addition.relation);
        let rel = storage::RelationDef {
            name: addition.relation.clone(),
            namespace: addition.namespace.clone(),
            kind: storage::RelationKind::Table,
            fields: Vec::new(),
            enum_values: Vec::new(),
            comment: None,
            layer_hint: None,
            start_line: addition.field.line,
            end_line: addition.field.line,
        };
        insert_storage_field(tx, file_id, None, &address, layer, &rel, &addition.field)?;
    }
    Ok(())
}

fn insert_storage_field(
    tx: &Transaction,
    file_id: i64,
    parent_id: Option<i64>,
    relation_address: &str,
    layer: &str,
    rel: &storage::RelationDef,
    field: &storage::FieldDef,
) -> Result<(), GraphError> {
    let address = format!(
        "{relation_address}/{}",
        storage::normalize_name(&field.name)
    );
    tx.execute(
        "INSERT INTO code_graph_storage_objects(\
             file_id, parent_id, address, layer, namespace, level, kind, \
             display_name, data_type, nullable, default_value, constraints, \
             ref_target, comment, start_line, end_line) \
         VALUES(?1, ?2, ?3, ?4, ?5, 'field', 'column', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
        params![
            file_id,
            parent_id,
            address,
            storage::normalize_name(layer),
            storage::normalize_name(&rel.namespace),
            field.name,
            field.data_type,
            field.nullable,
            field.default_value,
            serde_json::to_string(&field.constraints).unwrap_or_default(),
            field.references,
            field.comment,
            field.line,
        ],
    )?;
    Ok(())
}

// Storage read side (snapshot assembly)
/// Every persisted storage entity, grouped into [`RelationEntry`] shape:
/// relation rows carry their child fields; parentless field rows (`ALTER`
/// additions) fold onto the relation their address names, or synthesize a
/// placeholder relation when the CREATE lives outside the indexed tree.
/// Harvested comments ride along as intent (manifest meaning overrides at
/// merge time). Ordered by address for deterministic snapshots.
pub(crate) fn storage_rows(conn: &Connection) -> Result<Vec<RelationEntry>, GraphError> {
    let mut relations: Vec<RelationEntry> = Vec::new();
    // Address → index into `relations`. Both passes below used to locate a
    // relation by scanning everything collected so far, which made snapshot
    // assembly quadratic in the number of relations — and the pre-write gate
    // assembles a snapshot on every call, so that cost sits in the agent's
    // write path. The map keeps both passes linear.
    let mut by_address: HashMap<String, usize> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT o.id, o.address, o.layer, o.namespace, o.kind, o.display_name, \
                    o.constraints, o.comment, o.start_line, f.path \
             FROM code_graph_storage_objects o \
             JOIN code_graph_files f ON f.id = o.file_id \
             WHERE o.level = 'relation' ORDER BY o.address, f.path",
        )?;
        let rows = stmt.query_map([], |row| {
            let enum_values: Option<String> = row.get(6)?;
            Ok((
                row.get::<_, i64>(0)?,
                RelationEntry {
                    address: row.get(1)?,
                    layer: row.get(2)?,
                    namespace: row.get(3)?,
                    kind: row.get(4)?,
                    name: row.get(5)?,
                    fields: Vec::new(),
                    enum_values: enum_values
                        .and_then(|v| serde_json::from_str(&v).ok())
                        .unwrap_or_default(),
                    intent: row.get(7)?,
                    boundary: None,
                    redirects: Vec::new(),
                    source: Some(format!(
                        "{}:{}",
                        row.get::<_, String>(9)?,
                        row.get::<_, u32>(8)?
                    )),
                },
            ))
        })?;
        for row in rows {
            let (_id, entry) = row?;
            // The same address can be defined in several migration files;
            // the first (path-ordered) definition wins, its fields merge in
            // the field pass below regardless of which file defined them.
            if !by_address.contains_key(&entry.address) {
                by_address.insert(entry.address.clone(), relations.len());
                relations.push(entry);
            }
        }
    }

    let mut stmt = conn.prepare(
        "SELECT o.address, o.display_name, o.data_type, o.nullable, o.default_value, \
                o.constraints, o.ref_target, o.comment, o.start_line \
         FROM code_graph_storage_objects o \
         WHERE o.level = 'field' ORDER BY o.address, o.start_line",
    )?;
    let fields = stmt.query_map([], |row| {
        let constraints: Option<String> = row.get(5)?;
        Ok((
            row.get::<_, String>(0)?,
            FieldEntry {
                name: row.get(1)?,
                data_type: row.get(2)?,
                nullable: row.get::<_, Option<bool>>(3)?.unwrap_or(true),
                default_value: row.get(4)?,
                constraints: constraints
                    .and_then(|v| serde_json::from_str(&v).ok())
                    .unwrap_or_default(),
                references: row.get(6)?,
                intent: row.get(7)?,
                line: row.get(8)?,
            },
        ))
    })?;
    // Field identity within a relation, so the duplicate check costs one hash
    // instead of re-normalizing every field already attached to the relation.
    let mut seen_fields: HashSet<(usize, String)> = HashSet::new();
    for row in fields {
        let (address, field) = row?;
        let Some(relation_address) = address.rsplit_once('/').map(|(rel, _)| rel.to_string())
        else {
            continue;
        };
        let at = match by_address.get(&relation_address).copied() {
            Some(at) => at,
            None => {
                // ALTER addition whose CREATE is not indexed: synthesize the
                // relation so the field is still visible and gate-checkable.
                let (layer, rest) = relation_address.split_once('/').unwrap_or(("sql", ""));
                let (namespace, name) = rest.split_once('/').unwrap_or(("default", rest));
                let at = relations.len();
                relations.push(RelationEntry {
                    address: relation_address.clone(),
                    layer: layer.to_string(),
                    namespace: namespace.to_string(),
                    name: name.to_string(),
                    kind: "table".into(),
                    fields: Vec::new(),
                    enum_values: Vec::new(),
                    intent: None,
                    boundary: None,
                    redirects: Vec::new(),
                    source: None,
                });
                by_address.insert(relation_address, at);
                at
            }
        };
        if seen_fields.insert((at, storage::normalize_name(&field.name))) {
            relations[at].fields.push(field);
        }
    }
    Ok(relations)
}

/// Upsert a file row keyed by its unique path, returning the (stable) row id.
fn upsert_file(
    tx: &Transaction,
    rel: &str,
    lang: &str,
    sha: &str,
    mtime_ns: i64,
) -> Result<i64, GraphError> {
    let id = tx.query_row(
        "INSERT INTO code_graph_files(path, language, content_sha256, mtime_ns, indexed_at) \
         VALUES(?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(path) DO UPDATE SET \
           language = excluded.language, \
           content_sha256 = excluded.content_sha256, \
           mtime_ns = excluded.mtime_ns, \
           indexed_at = excluded.indexed_at \
         RETURNING id",
        params![rel, lang, sha, mtime_ns, now_unix()],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Delete rows for any stored file whose path is not in the current tree.
/// `ON DELETE CASCADE` clears its symbols and imports.
fn prune_missing(tx: &Transaction, current: &HashSet<String>) -> Result<usize, GraphError> {
    let stored: Vec<String> = {
        let mut stmt = tx.prepare("SELECT path FROM code_graph_files")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut removed = 0;
    for path in stored {
        if !current.contains(&path) {
            removed += tx.execute(
                "DELETE FROM code_graph_files WHERE path = ?1",
                params![path],
            )?;
        }
    }
    Ok(removed)
}

fn file_sha(tx: &Transaction, rel: &str) -> Result<Option<String>, GraphError> {
    // `.optional()` maps only "no row for this path" to `None`; any other DB
    // error propagates as `GraphError` instead of being silently swallowed
    // into a spurious "not previously indexed" (which would re-parse and mask
    // a real store fault).
    let sha = tx
        .query_row(
            "SELECT content_sha256 FROM code_graph_files WHERE path = ?1",
            params![rel],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(sha)
}

// Read side (frames come from these)
/// Every symbol named `name`, across the whole indexed tree.
pub(crate) fn definitions(conn: &Connection, name: &str) -> Result<Vec<DefRow>, GraphError> {
    let mut stmt = conn.prepare(
        "SELECT f.path, f.content_sha256, s.name, s.kind, s.start_line, s.end_line \
         FROM code_graph_symbols s JOIN code_graph_files f ON f.id = s.file_id \
         WHERE s.name = ?1 ORDER BY f.path, s.start_line",
    )?;
    let rows = stmt.query_map(params![name], map_def_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Every symbol defined in a given file.
pub(crate) fn symbols_in_file(conn: &Connection, rel: &str) -> Result<Vec<DefRow>, GraphError> {
    let mut stmt = conn.prepare(
        "SELECT f.path, f.content_sha256, s.name, s.kind, s.start_line, s.end_line \
         FROM code_graph_symbols s JOIN code_graph_files f ON f.id = s.file_id \
         WHERE f.path = ?1 ORDER BY s.start_line",
    )?;
    let rows = stmt.query_map(params![rel], map_def_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The import edges out of a file.
pub(crate) fn imports_from(conn: &Connection, rel: &str) -> Result<Vec<ImportRow>, GraphError> {
    let mut stmt = conn.prepare(
        "SELECT i.specifier, i.to_path, i.kind \
         FROM code_graph_imports i JOIN code_graph_files f ON f.id = i.from_file_id \
         WHERE f.path = ?1 ORDER BY i.specifier",
    )?;
    let rows = stmt.query_map(params![rel], |row| {
        Ok(ImportRow {
            specifier: row.get(0)?,
            to_path: row.get(1)?,
            kind: import_kind_from_tag(&row.get::<_, String>(2)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The files that import (resolve to) a given file.
pub(crate) fn importers_of(conn: &Connection, rel: &str) -> Result<Vec<String>, GraphError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.path \
         FROM code_graph_imports i JOIN code_graph_files f ON f.id = i.from_file_id \
         WHERE i.to_path = ?1 ORDER BY f.path",
    )?;
    let rows = stmt.query_map(params![rel], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// One call site joined to its file — the row behind the `callers` reverse
/// lookup.
#[derive(Debug, Clone)]
pub(crate) struct CallRow {
    pub path: String,
    pub line: u32,
}

/// The call sites recorded inside `rel`'s lines `start..=end` — the `callees`
/// read (#335, B1). The span is a stored symbol span, so "what does this
/// definition call" is one indexed range query, ordered by line.
pub(crate) fn calls_in_span(
    conn: &Connection,
    rel: &str,
    start_line: u32,
    end_line: u32,
) -> Result<Vec<crate::symbol::CallSite>, GraphError> {
    let mut stmt = conn.prepare(
        "SELECT c.callee_name, c.line \
         FROM code_graph_calls c JOIN code_graph_files f ON f.id = c.file_id \
         WHERE f.path = ?1 AND c.line BETWEEN ?2 AND ?3 \
         ORDER BY c.line, c.callee_name",
    )?;
    let rows = stmt.query_map(params![rel, start_line, end_line], |row| {
        Ok(crate::symbol::CallSite {
            callee: row.get(0)?,
            line: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Up to `limit` call sites whose callee name is exactly `name` — the
/// best-effort `callers` reverse lookup (#335, B1). Name-based: same-name
/// methods on different types all match, which is the labeled honesty of
/// this op. Path-then-line order keeps a truncated list stable.
pub(crate) fn calls_of_name(
    conn: &Connection,
    name: &str,
    limit: usize,
) -> Result<Vec<CallRow>, GraphError> {
    let mut stmt = conn.prepare(
        "SELECT f.path, c.line \
         FROM code_graph_calls c JOIN code_graph_files f ON f.id = c.file_id \
         WHERE c.callee_name = ?1 ORDER BY f.path, c.line LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![name, limit as i64], |row| {
        Ok(CallRow {
            path: row.get(0)?,
            line: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Total indexed call sites across the whole tree.
pub(crate) fn call_count(conn: &Connection) -> Result<usize, GraphError> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM code_graph_calls", [], |r| r.get(0))?;
    Ok(count as usize)
}

/// Up to `limit` files that no resolved import edge points at — the
/// entry-point set. One query whatever the index size (the anti-join rides
/// the `code_graph_imports_to` index), unlike the per-file `importers_of`
/// scan this replaces, which forced callers to cap orientation at a few
/// hundred files. Shallowest path first, then lexicographic, so the
/// truncated list is stable and leads with the roots worth reading first.
pub(crate) fn entry_points(conn: &Connection, limit: usize) -> Result<Vec<String>, GraphError> {
    let mut stmt = conn.prepare(
        "SELECT f.path FROM code_graph_files f \
         WHERE NOT EXISTS (SELECT 1 FROM code_graph_imports i WHERE i.to_path = f.path) \
         ORDER BY LENGTH(f.path) - LENGTH(REPLACE(f.path, '/', '')), f.path \
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// All indexed file paths (used by the linear reference scan).
pub(crate) fn all_files(conn: &Connection) -> Result<Vec<String>, GraphError> {
    let mut stmt = conn.prepare("SELECT path FROM code_graph_files ORDER BY path")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Count of indexed files — used by status/tests.
pub(crate) fn file_count(conn: &Connection) -> Result<usize, GraphError> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM code_graph_files", [], |r| r.get(0))?;
    Ok(count as usize)
}

/// Total indexed symbols across the whole tree — the graph total, not a
/// per-pass delta. The startup summary reports this so an incremental pass over
/// an unchanged tree (which re-parses nothing) still shows the real symbol
/// count instead of a misleading zero.
pub(crate) fn symbol_count(conn: &Connection) -> Result<usize, GraphError> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM code_graph_symbols", [], |r| r.get(0))?;
    Ok(count as usize)
}

/// Total indexed import edges across the whole tree.
pub(crate) fn import_count(conn: &Connection) -> Result<usize, GraphError> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM code_graph_imports", [], |r| r.get(0))?;
    Ok(count as usize)
}

/// The best-connected file in the index: most symbols plus import edges in
/// both directions. UI consumers use it as a default focus when the caller
/// hasn't picked a file yet. `None` on an empty index.
pub(crate) fn busiest_file(conn: &Connection) -> Result<Option<String>, GraphError> {
    let path = conn
        .query_row(
            "SELECT f.path,
                    (SELECT COUNT(*) FROM code_graph_symbols s WHERE s.file_id = f.id)
                  + (SELECT COUNT(*) FROM code_graph_imports i WHERE i.from_file_id = f.id)
                  + (SELECT COUNT(*) FROM code_graph_imports i2 WHERE i2.to_path = f.path)
                    AS degree
             FROM code_graph_files f ORDER BY degree DESC, f.path LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(path)
}

/// All distinct symbol names of the given kind tag (e.g. `"table"`,
/// `"schema_enum"`, `"view"`). Used by the schema gate to populate the
/// known-schema index at session start.
pub(crate) fn names_of_kind(conn: &Connection, kind: &str) -> Result<HashSet<String>, GraphError> {
    let mut stmt =
        conn.prepare("SELECT DISTINCT LOWER(name) FROM code_graph_symbols WHERE kind = ?")?;
    let rows = stmt.query_map(params![kind], |row| row.get::<_, String>(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn map_def_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DefRow> {
    Ok(DefRow {
        path: row.get(0)?,
        sha: row.get(1)?,
        name: row.get(2)?,
        kind: SymbolKind::from_tag(&row.get::<_, String>(3)?),
        start_line: row.get(4)?,
        end_line: row.get(5)?,
    })
}

fn import_kind_from_tag(tag: &str) -> ImportKind {
    match tag {
        "relative" => ImportKind::Relative,
        "absolute" => ImportKind::Absolute,
        _ => ImportKind::Bare,
    }
}

// small helpers
/// Escape SQL `LIKE` wildcards in a literal path so it can serve as the fixed
/// part of a prefix pattern. Paths legally contain `%` and `_`; without the
/// escape a directory named `a_b` would prune `axb/…` too.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Forward-slash path of `abs` relative to `root` (falls back to the whole
/// path if `abs` is somehow not under `root`).
fn rel_path(root: &Path, abs: &Path) -> String {
    match abs.strip_prefix(root) {
        Ok(rel) => import::rel_to_slash(rel),
        Err(_) => import::rel_to_slash(abs),
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn mtime_ns(abs: &Path) -> i64 {
    std::fs::metadata(abs)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
