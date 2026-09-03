// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! [`StoreError`] — everything this crate can fail with, as named cases.
//!
//! Its own module rather than more lines in the already-oversized `lib.rs`,
//! per the crate's working rule: new code goes in a new module instead of
//! raising a grandfathered ceiling.
//!
//! The type used to be `pub struct StoreError(pub String)`, which meant a
//! caller could only tell a corrupt database from a stale binary from an
//! ordinary `SQLITE_BUSY` by reading prose (#3735) — a breach of AGENTS.md #5
//! ("typed errors, no panics"), and one the `typed-errors` gate cannot see
//! because it only flags the literal `Result<_, String>` shape.
//!
//! # What earns a case
//!
//! A failure gets its own case when a caller can *act* differently on it —
//! the four the crate already distinguished internally do:
//! [`StoreError::NegativeSchemaVersion`] and [`StoreError::SchemaTooNew`] tell
//! "move the file aside" from "upgrade your binary", and
//! [`StoreError::Corrupt`] tells a malformed file (run `stella doctor`) from
//! the plain [`StoreError::Sqlite`] failure it otherwise arrives as. Which of
//! those two a SQLite failure becomes is decided in one place, the
//! `From<rusqlite::Error>` conversion below, because that is the only path
//! every `?` in the crate takes.
//! [`StoreError::Io`] and [`StoreError::Serde`] keep the underlying
//! `std::io::Error` / `serde_json::Error` reachable through
//! [`std::error::Error::source`], so a caller can branch on
//! [`std::io::ErrorKind`] rather than on the word "permission".
//!
//! Everything else is a one-off rule check — a count that overflowed a
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
/// job; the four actionable cases carry their own full sentence, because
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
    /// violate whatever rules the newer schema added.
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

    /// SQLite cannot read a database it was given, on page 1 or anywhere
    /// after it.
    ///
    /// Separated from [`StoreError::Sqlite`] because the remedy is different
    /// and because it is otherwise invisible: without this the failure reaches
    /// the caller as the raw rusqlite string ("database disk image is
    /// malformed"), every later session repeats the same warning, and nothing
    /// ever tells the user to move the file aside. Reached from every `?` in
    /// the crate — see the `From<rusqlite::Error>` conversion below.
    #[error(
        "{subject} cannot be read as a SQLite database ({source}), so it is corrupt or \
         was overwritten. Run `stella doctor` to confirm, then `stella doctor --repair` \
         to salvage what is readable and move the file aside (it is renamed, never \
         deleted) — it holds local telemetry and session replay, never your source.",
        subject = corrupt_subject(.path)
    )]
    Corrupt {
        /// The file named in the remedy, as the caller located it. `None` when
        /// the failure arrived through the blanket conversion, which has no
        /// database to ask — see `corrupt_subject`.
        path: Option<String>,
        /// The corruption SQLite reported.
        source: rusqlite::Error,
    },

    /// An ordinary SQLite failure — a busy database, a constraint, a query
    /// against a schema this build did not expect. Never corruption: the
    /// conversion below routes that to [`StoreError::Corrupt`] instead.
    #[error("{0}")]
    Sqlite(#[source] rusqlite::Error),

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

    /// A one-off rule check with no second branch for a caller to take —
    /// a value that overflowed its column, a spool row that failed validation,
    /// a table name this build does not know.
    ///
    /// Narrow: a failure a caller will want to *match* on belongs
    /// in a case of its own, not here.
    #[error("{0}")]
    Other(String),
}

/// How the corruption message names the database.
///
/// A caller that resolved the file gets the file. The blanket conversion below
/// has none to give: SQLite raises `SQLITE_CORRUPT` from a statement, and a
/// statement does not carry the database it ran against. This crate opens
/// several (`store.db`, `usage.db`, `catalog.db`, `enterprise-telemetry.db`),
/// so a guessed name would send a user to move a healthy database aside.
/// `stella doctor`, which the remedy points at, locates and names the file it
/// checks.
fn corrupt_subject(path: &Option<String>) -> &str {
    path.as_deref().unwrap_or("a stella SQLite database")
}

/// Every `?` on a SQLite failure in this crate lands here, which is why the
/// corruption split is made here and not at the call sites.
///
/// A store can take damage on any page, and the statement that walks it is
/// whichever read reaches that table next — so the split has to hold for the
/// whole read/write surface, not for the two statements that run while opening.
/// Wrapping call sites cannot do that: SQLite reports the damage from
/// `Rows::next`, so [`Store::execution_events`](crate::Store::execution_events)
/// fails inside `for row in rows { … row? }`, where there is no
/// connection-level result to wrap. Every fallible method in the crate would
/// also have to remember, and a new one would not. This conversion is the one
/// path all of them already take.
///
/// Anything that is not corruption passes through as
/// [`StoreError::Sqlite`] unchanged, so `SQLITE_BUSY` keeps its own retry
/// (see [`crate::busy`]).
impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        crate::integrity::corrupt_store_error(error, None)
    }
}

impl StoreError {
    /// The SQLite result code behind this failure, when it has one.
    ///
    /// A caller reporting a dropped write needs the code, not the sentence:
    /// `DatabaseBusy` sends an operator to whatever else is writing the file,
    /// `ReadOnly` to its permissions, `Full` to the disk. Rendering it is the
    /// caller's job; naming it is this crate's.
    #[must_use]
    pub fn sqlite_code(&self) -> Option<rusqlite::ErrorCode> {
        match self {
            Self::Sqlite(error) | Self::Corrupt { source: error, .. } => error.sqlite_error_code(),
            _ => None,
        }
    }

    /// Whether SQLite refused because a lock was held — see [`crate::busy`],
    /// which owns the predicate and the retry built on it.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Sqlite(error) if crate::busy::is_busy(error))
    }

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

    /// Corruption reaches a caller as [`StoreError::Corrupt`] however it was
    /// raised, not only from the two statements the open sequence runs.
    ///
    /// This is the wire shape SQLite hands back mid-scan, put through the
    /// conversion every `?` in the crate takes. Flattened into
    /// [`StoreError::Sqlite`] it says "database disk image is malformed" and
    /// nothing else: no remedy, no file, and nothing to tell it from an
    /// ordinary `SQLITE_BUSY`.
    #[test]
    fn corruption_is_classified_by_the_conversion_every_query_takes() {
        let malformed = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some("database disk image is malformed".to_string()),
        );

        let error = StoreError::from(malformed);
        match &error {
            StoreError::Corrupt { path, .. } => assert_eq!(
                *path, None,
                "the conversion has no database to name, and must not guess one"
            ),
            other => panic!(
                "corruption from an ordinary statement must classify as Corrupt, \
                 not as a bare SQLite failure: {other:?}"
            ),
        }
        assert!(
            error.to_string().contains("stella doctor"),
            "the remedy travels with the error: {error}"
        );
        assert_eq!(
            error.sqlite_code(),
            Some(rusqlite::ErrorCode::DatabaseCorrupt),
            "the code stays reachable through the corrupt case"
        );
        assert!(!error.is_busy(), "corruption is not a held lock");
    }

    /// The other half of the split: a lock that is held is still an ordinary
    /// failure, so [`crate::busy::retry_busy`] keeps asking again for it. A
    /// conversion that classified too eagerly would turn a retryable write
    /// into a corrupt-database report.
    #[test]
    fn a_held_lock_is_not_reclassified_as_corruption() {
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".to_string()),
        );

        let error = StoreError::from(busy);
        assert!(
            matches!(error, StoreError::Sqlite(_)),
            "a busy database is not corruption: {error:?}"
        );
        assert!(error.is_busy(), "and it is still retryable: {error}");
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
