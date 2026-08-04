//! Error type for the code-graph indexer.
//!
//! Per the "fail loud, recover gracefully" rule, every fallible boundary
//! returns a typed `thiserror` error rather than panicking.
//! The one hot-path subtlety this crate adds (the indexer's quality
//! bar): a tree-sitter *parse* failure on an arbitrary file is **not** a
//! `GraphError` — it is skipped-with-record inside the indexer so one
//! unparseable file never aborts a whole index batch (L-L1). `GraphError` is
//! reserved for genuine infrastructure faults (SQLite, I/O, a malformed
//! built-in query — all of which are programmer/environment errors, not
//! untrusted input).

use std::path::PathBuf;

/// Anything that can go wrong opening, indexing, or querying the code graph.
///
/// Deliberately narrow: per-file I/O and parse faults are skipped-with-record
/// inside the indexer (see the module doc), so there is no per-file I/O
/// variant here — the only filesystem fault that aborts anything is the
/// root canonicalization ([`GraphError::Root`]).
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// A SQLite operation failed (open, migrate, transaction, query).
    #[error("code-graph store error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// One of the crate's own compile-time `.scm` queries (L-L2) failed to
    /// compile against its grammar.
    /// This is a programmer error caught by the crate's own tests, surfaced
    /// as an error rather than a panic so a mis-edit degrades loudly instead
    /// of aborting a host process.
    #[error("failed to compile the {lang} {kind} query: {message}")]
    Query {
        lang: &'static str,
        kind: &'static str,
        message: String,
    },

    /// The workspace root could not be canonicalized (does not exist, or a
    /// permissions fault) — mounting against a missing root is a caller bug.
    #[error("workspace root {root} is not accessible: {source}")]
    Root {
        root: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A filesystem watcher could not be created or armed (`notify`).
    #[error("code-graph watcher error: {0}")]
    Watch(String),

    /// The store's `PRAGMA user_version` cannot be reconciled with this
    /// build's schema — it was written by a newer stella, or the shape the
    /// DDL claims to have produced is not there (#617). The message carries
    /// the store's name and what to do about it.
    #[error("{0}")]
    Schema(String),
}
