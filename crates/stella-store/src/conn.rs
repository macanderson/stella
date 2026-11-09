// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Wrappers around the rusqlite types that `Store::lock()`, `UsageStore::lock()`
//! and the catalog's `lock()` return. Each one knows the path of the open file.
//!
//! A corrupt-database error names its file only when the caller already knew
//! the path. The open sequence gets that treatment
//! ([`crate::integrity::classify_store_corruption`]). Every ordinary read or
//! write after that ran on a bare `MutexGuard<Connection>`, with no path to
//! name (see `crate::error::corrupt_subject`).
//!
//! [`Guard`] asks `rusqlite::Connection::path()` for the file it already
//! knows. [`Guard::prepare`] returns a [`Statement`] that keeps the same
//! path, and [`Statement::query_map`] returns [`Rows`], whose iterator keeps
//! it too. That is where `Store::execution_events` actually fails: inside
//! `Rows::next`, not the statement that started the scan.
//!
//! An inherent method wins over a plain field read, so `self.lock().execute(...)`
//! and `stmt.query_map(...)` keep compiling once `lock()`'s return type
//! changes below, under the same names, now returning [`crate::Result`]. A
//! call site that instead unwraps a raw `rusqlite::Result`, through a
//! `rusqlite::Result<..>` turbofish or `rusqlite::OptionalExtension`, stops
//! compiling. That short list is what needed a look.
//!
//! [`Guard::transaction`] runs a closure on the open transaction. Any error
//! that leaves the closure names the file, even one from a helper it called.
//! [`Guard::with_conn`] does the same for a helper that wants the bare link.

use std::path::Path;

use rusqlite::{MappedRows, Params, Row};

use crate::integrity::{classify_store_corruption, corrupt_store_error};
use crate::{Result, StoreError};

/// Turn a raw rusqlite failure into a `StoreError`, naming `path` when the
/// caller has one. A free function, not a method: `Option<&str>` is `Copy`,
/// so a closure can capture it by value and never fight the borrow checker
/// over which field of `self` it holds.
fn wrap(path: Option<&str>, error: rusqlite::Error) -> StoreError {
    corrupt_store_error(error, path.map(Path::new))
}

/// Add `path` to an error that is already a `StoreError`. The `?` inside a
/// transaction body turns a rusqlite error into one with no path. This puts
/// the path back once the body returns.
fn named<T>(path: Option<&str>, result: Result<T>) -> Result<T> {
    result.map_err(|error| classify_store_corruption(error, path.map(Path::new)))
}

/// A locked connection that knows which file backs it. `Store::lock()`,
/// `UsageStore::lock()` and the catalog's `lock()` return this now, in place
/// of a bare `MutexGuard<Connection>`.
pub(crate) struct Guard<'a> {
    conn: std::sync::MutexGuard<'a, rusqlite::Connection>,
}

impl<'a> Guard<'a> {
    pub(crate) fn new(conn: std::sync::MutexGuard<'a, rusqlite::Connection>) -> Self {
        Self { conn }
    }

    pub(crate) fn execute<P: Params>(&self, sql: &str, params: P) -> Result<usize> {
        self.conn
            .execute(sql, params)
            .map_err(|e| wrap(self.conn.path(), e))
    }

    pub(crate) fn execute_batch(&self, sql: &str) -> Result<()> {
        self.conn
            .execute_batch(sql)
            .map_err(|e| wrap(self.conn.path(), e))
    }

    pub(crate) fn query_row<T, P, F>(&self, sql: &str, params: P, f: F) -> Result<T>
    where
        P: Params,
        F: FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    {
        self.conn
            .query_row(sql, params, f)
            .map_err(|e| wrap(self.conn.path(), e))
    }

    pub(crate) fn prepare(&self, sql: &str) -> Result<Statement<'_>> {
        let inner = self
            .conn
            .prepare(sql)
            .map_err(|e| wrap(self.conn.path(), e))?;
        Ok(Statement {
            inner,
            path: self.conn.path(),
        })
    }

    /// Runs `body` in a transaction. `body` gets the transaction and commits
    /// it, so a return before the commit rolls back. Any error out of `body`
    /// names this file, even one from a helper `body` called.
    pub(crate) fn transaction<T>(
        &mut self,
        body: impl FnOnce(rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let path = self.conn.path().map(str::to_string);
        let result = match self.conn.transaction() {
            Ok(tx) => body(tx),
            Err(error) => Err(error.into()),
        };
        named(path.as_deref(), result)
    }

    /// [`Guard::transaction`] with the lock mode the caller picks.
    pub(crate) fn transaction_with_behavior<T>(
        &mut self,
        behavior: rusqlite::TransactionBehavior,
        body: impl FnOnce(rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let path = self.conn.path().map(str::to_string);
        let result = match self.conn.transaction_with_behavior(behavior) {
            Ok(tx) => body(tx),
            Err(error) => Err(error.into()),
        };
        named(path.as_deref(), result)
    }

    /// [`Guard::transaction`] for a caller that holds only `&self`.
    pub(crate) fn unchecked_transaction<T>(
        &self,
        body: impl FnOnce(rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let result = match self.conn.unchecked_transaction() {
            Ok(tx) => body(tx),
            Err(error) => Err(error.into()),
        };
        named(self.conn.path(), result)
    }

    /// Runs `body` on the bare connection, for a helper that takes a
    /// `&rusqlite::Connection`. Any error out of `body` names this file.
    pub(crate) fn with_conn<T>(
        &self,
        body: impl FnOnce(&rusqlite::Connection) -> Result<T>,
    ) -> Result<T> {
        let conn: &rusqlite::Connection = &self.conn;
        named(conn.path(), body(conn))
    }

    pub(crate) fn last_insert_rowid(&self) -> i64 {
        self.conn.last_insert_rowid()
    }
}

impl crate::Store {
    /// The lock for a plain read or write. If the file is bad, the error
    /// names it. A poisoned lock means a panic in the middle of a write. The
    /// link to the file still works, so this takes the lock back. To refuse
    /// every later write would lose all the logs after one bad one.
    pub(crate) fn lock(&self) -> Guard<'_> {
        Guard::new(self.raw_lock())
    }

    /// The bare lock, for code that names the file on its own. `migrate`
    /// and `migrate_and_prepare_exports` run when the store opens. They pass
    /// each error through [`crate::integrity::classify_store_corruption`].
    /// `integrity_check` shows what SQLite said, word for word. All other
    /// code should use `lock`.
    pub(crate) fn raw_lock(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A prepared statement that still knows its connection's path. A query run
/// past this point can still name the file if it hits corruption. See
/// [`Rows`].
pub(crate) struct Statement<'stmt> {
    inner: rusqlite::Statement<'stmt>,
    path: Option<&'stmt str>,
}

impl<'stmt> Statement<'stmt> {
    pub(crate) fn query_map<T, P, F>(&mut self, params: P, f: F) -> Result<Rows<'_, F>>
    where
        P: Params,
        F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
    {
        let path = self.path;
        let inner = self.inner.query_map(params, f).map_err(|e| wrap(path, e))?;
        Ok(Rows { inner, path })
    }

    /// Column metadata never fails, so this and [`Statement::column_names`]
    /// just forward to the wrapped type.
    pub(crate) fn column_count(&self) -> usize {
        self.inner.column_count()
    }

    pub(crate) fn column_names(&self) -> Vec<&str> {
        self.inner.column_names().collect()
    }
}

/// The row iterator [`Statement::query_map`] returns. Corruption SQLite finds
/// mid-scan surfaces from [`Iterator::next`], not from the statement that
/// started it. This is the wrapper `Store::execution_events` needs.
///
/// The struct does not store `T`. `rusqlite::MappedRows` does not either: the
/// closure `F` produces each row, so only the `Iterator` impl below needs to
/// name its type.
pub(crate) struct Rows<'stmt, F> {
    inner: MappedRows<'stmt, F>,
    path: Option<&'stmt str>,
}

impl<T, F> Iterator for Rows<'_, F>
where
    F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
{
    type Item = Result<T>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|row| row.map_err(|e| wrap(self.path, e)))
    }
}

/// The crate's own `.optional()`. It stands in for `rusqlite::OptionalExtension`
/// wherever a call now returns [`crate::Result`] instead of `rusqlite::Result`.
/// Rust's orphan rule blocks `OptionalExtension` itself from covering
/// `crate::Result`, since neither the trait nor the `Result` alias belongs to
/// this crate alone. This trait has the same shape, and this crate owns it.
pub(crate) trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>>;
}

impl<T> OptionalExt<T> for Result<T> {
    fn optional(self) -> Result<Option<T>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => Ok(None),
            Err(other) => Err(other),
        }
    }
}
