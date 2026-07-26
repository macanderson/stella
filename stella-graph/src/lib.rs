// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-graph` — the code-graph indexer.
//!
//! A tree-sitter symbol + import-edge indexer over a workspace, persisted in
//! SQLite, exposed as a **built-in CGP provider** feeding `stella-context`
//! ("implemented AS a built-in CGP provider … the
//! protocol's first proof of non-triviality"). It depends only on `contextgraph-types`
//! for the wire shape — never on `stella-context` — so the provider boundary
//! stays one-directional.
//!
//! # What it does
//!
//! - **Indexes** Rust, TypeScript, TSX, JavaScript, Python, SQL, Go, Java, C,
//!   and PHP (see [`Language`]): symbols (functions, methods,
//!   structs/classes/enums/traits/interfaces; for SQL, tables/views/schema
//!   enums — the rows the schema gate reads) and import edges
//!   (`file → module/file`), via compile-time tree-sitter queries (L-L2).
//! - **Resolves** Python relative imports (`from . import x`,
//!   `from ..pkg import y`), TS/JS relative specifiers (`./x`, `../y`,
//!   `index.*`), and the literal-path imports of C (`#include "x.h"`) and PHP
//!   (`require '…'`) to real files; bare package specifiers, Rust `use` paths,
//!   Java packages, and PHP namespaces are recorded unresolved — the edge
//!   survives even when its target is outside the tree.
//! - **Maps storage** through vendor-neutral adapters ([`storage`]): SQL DDL,
//!   Prisma, TS/JS ORMs (Drizzle, TypeORM, Mongoose, DynamoDB), and Python
//!   ORMs (Django, SQLAlchemy) all reduce to one layer/namespace/relation/field
//!   model, merged with the committed [`manifest`] at snapshot time.
//! - **Warms at mount** and re-indexes live via an in-process `notify`
//!   watcher (L-C1).
//! - **Skips byte-identical content** on re-index (L-C2).
//! - **Answers queries as frames** with mandatory human citation labels and
//!   `code-graph` provenance (L-C4).
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//! use stella_graph::CodeGraph;
//!
//! # fn main() -> Result<(), stella_graph::GraphError> {
//! let graph = CodeGraph::open(Path::new("."), Path::new("./.stella/private/codegraph.db"))?;
//! graph.index_all()?;
//! for frame in graph.definitions("run_turn")? {
//!     // Cite by the human label, never the raw id (L-C4).
//!     println!("{}", frame.citation_label.as_deref().unwrap_or(&frame.title));
//! }
//! # Ok(())
//! # }
//! ```

mod error;
mod frames;
mod generated;
mod graph;
mod import;
mod lang;
pub mod manifest;
mod parse;
mod queries;
pub mod storage;
mod store;
mod symbol;
mod walk;
mod watch;

pub use error::GraphError;
pub use frames::PROVIDER_ID;
pub use graph::{CodeGraph, FileNeighborhood, NeighborhoodSymbol, SymbolSpan};
pub use import::{ImportEdge, ImportKind};
pub use lang::Language;
pub use storage::{StorageExtract, StorageExtractor, StorageSnapshot};
pub use store::IndexStats;
pub use symbol::{Symbol, SymbolKind};
#[doc(hidden)]
pub use watch::WatchInjector;

/// Assemble the storage map without mounting a graph: read the persisted
/// index (if `stella init` has built one) and merge `stella.storage.toml`.
/// The pre-write gate and `stella storage` call this per use so the map is
/// always current — no session-start cache to go stale. Best-effort: no
/// index and no manifest yields an empty snapshot, which makes every
/// storage mechanism a no-op (spec §11).
///
/// This is a pure read: it opens the store with `store::open_read`, so the
/// gate never runs DDL on the agent's write path. A store no writer has
/// migrated yet has no tables to read and yields the same empty snapshot a
/// missing store does.
pub fn load_storage_snapshot(workspace_root: &std::path::Path) -> StorageSnapshot {
    // A parse error is carried, not discarded: it is the difference between
    // "this repo declares no boundaries" and "this repo's boundaries are not
    // being enforced because of a typo", and those must not look identical.
    let (manifest, manifest_error) = match manifest::StorageManifest::load(workspace_root) {
        Ok(manifest) => (manifest, None),
        Err(error) => (None, Some(error)),
    };
    let db_path = workspace_root
        .join(".stella")
        .join("private")
        .join("codegraph.db");
    let rows = if db_path.exists() {
        store::open_read(&db_path)
            .and_then(|conn| store::storage_rows(&conn))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut snapshot = manifest::merge_snapshot(rows, manifest.as_ref());
    snapshot.manifest_error = manifest_error;
    snapshot
}

// Re-export the CGP wire types this provider produces so downstream callers
// need not also depend on `contextgraph-types` directly for the return shapes.
pub use contextgraph_types::{ContextFrame, ContextQuery, FrameKind};
