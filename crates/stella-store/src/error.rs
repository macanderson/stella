// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! [`StoreError`] — everything this crate can fail with, as named variants.
//!
//! Its own module rather than more lines in the already-oversized `lib.rs`,
//! per the crate's working rule: new code goes in a new module instead of
//! raising a grandfathered ceiling.
//!
//! The type used to be `pub struct StoreError(pub String)`, which meant a
//! caller could only tell a corrupt database from a stale binary from an
//! ordinary `SQLITE_BUSY` by reading prose (#3735) — a breach of invariant #5
//! ("typed errors, no panics"), and one the `typed-errors` gate cannot see
//! because it only flags the literal `Result<_, String>` shape.
//!
//! # What earns a variant
//!
//! A failure gets its own variant when a caller can *act* differently on it —
//! the four the crate already distinguished internally do:
//! [`StoreError::NegativeSchemaVersion`] and [`StoreError::SchemaTooNew`] tell
//! "move the file aside" from "upgrade your binary", and
//! [`StoreError::Corrupt`] tells a malformed file (run `stella doctor`) from
//! the plain [`StoreError::Sqlite`] failure it otherwise arrives as.
//! [`StoreError::Io`] and [`StoreError::Serde`] keep the underlying
//! `std::io::Error` / `serde_json::Error` reachable through
//! [`std::error::Error::source`], so a caller can branch on
//! [`std::io::ErrorKind`] rather than on the word "permission".
//!
//! Everything else is a one-off invariant check — a count that overflowed a
//! column, a spool row that failed validation — where no caller has a second
//! branch to take. Those stay in [`StoreError::Other`], which is the deliberate
//! narrow catch-all, not a place to put a failure someone will later want to
//! match on.

/// Everything the store can fail with.
///
/// `Display` renders the failure and nothing else. The old wrapper prefixed
/// every message with `store: `, which every caller then said again — the CLI
/// printed `cannot open store: store: …` and the runtime
/// `local store unavailable (store: …)`. Naming the subject is the caller's
/// job; the four actionable variants carry their own full sentence, because
/// that sentence is the remedy the user has to follow.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// `PRAGMA user_version` is negative, so the file header is not one this
    /// crate wrote (or was overwritten). Not recoverable in place — the file
    /// has to be moved aside.
    ///
    /// Refused rather than indexed: a negative stamp would index `MIGRATIONS`
    /// with a wrapped `usize` and panic, taking the session down with it.
    #[error(
        "store.db carries a negative schema version ({version}), so it is not a \
         stella store or its header was overwritten. Move .stella/private/store.db aside \
         and reopen this workspace to start a fresh one."
    )]
    NegativeSchemaVersion {
        /// The stamp read out of the file header.
        version: i64,
    },

    /// The file was written by a newer build than this one. A downgrade guard,
    /// not a formality: older code writing into a newer shape would silently
    /// violate whatever invariants the newer schema added.
    #[error(
        "store.db is at schema version {file_version}, but this build only knows \
         {build_version} — your stella binary is out of date, not the workspace. Upgrade \
         with `brew upgrade stella`, re-run install.sh, or grab a newer build from \
         https://github.com/macanderson/stella/releases, then reopen this workspace."
    )]
    SchemaTooNew {
        /// The version stamped in the file.
        file_version: i64,
        /// The version this binary understands (`migrations::SCHEMA_VERSION`).
        build_version: i64,
    },

    /// SQLite cannot read the file as a database at all.
    ///
    /// Separated from [`StoreError::Sqlite`] because the remedy is different
    /// and because it is otherwise invisible: without this the failure reaches
    /// the caller as the raw rusqlite string ("database disk image is
    /// malformed"), every later session repeats the same warning, and nothing
    /// ever tells the user to move the file aside. See
    /// `integrity::corrupt_store_error`.
    #[error(
        "store.db cannot be read as a SQLite database ({source}), so it is corrupt \
         or was overwritten. Run `stella doctor` to confirm, then `stella doctor --repair` \
         to salvage what is readable and move {path} aside (it is renamed, never deleted) \
         — it holds local telemetry and session replay, never your source."
    )]
    Corrupt {
        /// The file named in the remedy, as the caller located it.
        path: String,
        /// The corruption SQLite reported.
        source: rusqlite::Error,
    },

    /// An ordinary SQLite failure — a busy database, a constraint, a query
    /// against a schema this build did not expect.
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A filesystem operation failed. `context` names the operation and the
    /// path in the crate's established wording ("cannot read /…"); the kind is
    /// on `source`, which is where a caller should read it from.
    #[error("{context}: {source}")]
    Io {
        /// What was being attempted, path included.
        context: String,
        /// The underlying failure.
        source: std::io::Error,
    },

    /// A JSON payload could not be serialized or parsed. Same split as
    /// [`StoreError::Io`]: prose in `context`, the typed cause on `source`.
    #[error("{context}: {source}")]
    Serde {
        /// What was being encoded or decoded.
        context: String,
        /// The underlying failure.
        source: serde_json::Error,
    },

    /// A one-off invariant check with no second branch for a caller to take —
    /// a value that overflowed its column, a spool row that failed validation,
    /// a table name this build does not know.
    ///
    /// Deliberately narrow: a failure a caller will want to *match* on belongs
    /// in a variant of its own, not here.
    #[error("{0}")]
    Other(String),
}

impl StoreError {
    /// A filesystem failure, with the operation and path as context.
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// A JSON encode/decode failure, with what was being handled as context.
    pub(crate) fn serde(context: impl Into<String>, source: serde_json::Error) -> Self {
        Self::Serde {
            context: context.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rusqlite::Connection;

    use super::StoreError;
    use crate::{Store, migrations::SCHEMA_VERSION};

    /// A workspace whose store exists on disk, plus the file itself.
    fn opened_workspace() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("store");
        let path = store
            .workspace_root()
            .map(Path::to_path_buf)
            .expect("a workspace store has a root")
            .join(".stella/private/store.db");
        drop(store);
        assert!(path.exists(), "the store file must exist to be re-stamped");
        (dir, path)
    }

    fn stamp_user_version(db_path: &Path, version: i64) {
        let conn = Connection::open(db_path).expect("open");
        conn.pragma_update(None, "user_version", version)
            .expect("stamp");
    }

    /// The two schema-version refusals have opposite remedies — move the file
    /// aside vs. upgrade the binary — and a caller must be able to tell them
    /// apart without reading the sentence. Under the old
    /// `StoreError(pub String)` this could only be spelled as a substring
    /// search over prose.
    #[test]
    fn a_negative_schema_version_is_matchable_as_itself() {
        let (dir, db_path) = opened_workspace();
        stamp_user_version(&db_path, -4);

        match Store::open(dir.path()) {
            Ok(_) => panic!("a negative stamp must fail closed"),
            Err(StoreError::NegativeSchemaVersion { version }) => assert_eq!(version, -4),
            Err(other) => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn a_newer_schema_version_is_matchable_as_itself() {
        let (dir, db_path) = opened_workspace();
        stamp_user_version(&db_path, SCHEMA_VERSION + 1);

        match Store::open(dir.path()) {
            Ok(_) => panic!("a newer-versioned file must refuse to open"),
            Err(StoreError::SchemaTooNew {
                file_version,
                build_version,
            }) => {
                assert_eq!(file_version, SCHEMA_VERSION + 1);
                assert_eq!(build_version, SCHEMA_VERSION);
            }
            Err(other) => panic!("wrong variant: {other:?}"),
        }
    }

    /// The remedy for a corrupt file (`stella doctor`) is not the remedy for a
    /// busy or malformed *statement*, and both used to arrive as the same
    /// stringly-typed error.
    #[test]
    fn a_corrupt_file_is_matchable_apart_from_an_ordinary_sqlite_failure() {
        let (dir, db_path) = opened_workspace();
        std::fs::write(&db_path, [0x7f; 4096]).expect("overwrite the header");

        match Store::open(dir.path()) {
            Ok(_) => panic!("a corrupt store must not open"),
            Err(error @ StoreError::Corrupt { .. }) => assert!(
                error.to_string().contains("stella doctor"),
                "the corrupt variant carries the remedy: {error}"
            ),
            Err(other) => panic!("wrong variant: {other:?}"),
        }

        let store = Store::in_memory().expect("in-memory store");
        let ordinary = store
            .lock()
            .execute_batch("SELECT * FROM no_such_table")
            .map_err(StoreError::from)
            .expect_err("a query against a missing table must fail");
        assert!(
            matches!(ordinary, StoreError::Sqlite(_)),
            "an ordinary statement failure is not corruption: {ordinary:?}"
        );
    }

    /// A filesystem failure keeps its `ErrorKind` reachable through
    /// [`std::error::Error::source`], so a caller branches on the kind rather
    /// than on the word "permission" appearing in a sentence.
    #[test]
    fn a_filesystem_failure_keeps_its_kind_on_the_source() {
        let missing = std::fs::read("/nonexistent/stella/store-error-witness")
            .expect_err("reading a missing path must fail");
        let error = StoreError::io("cannot read /nonexistent", missing);

        let source = std::error::Error::source(&error).expect("io errors carry their cause");
        let io = source
            .downcast_ref::<std::io::Error>()
            .expect("the cause is the std::io::Error itself, not a rendering of it");
        assert_eq!(io.kind(), std::io::ErrorKind::NotFound);
        assert!(
            error.to_string().starts_with("cannot read /nonexistent"),
            "the context still leads the message: {error}"
        );
    }
}
