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
//! This module does not wrap a transaction past its own start.
//! `Guard::transaction` and `Guard::unchecked_transaction` name the path only
//! if starting fails, then hand back a plain `rusqlite::Transaction`. Every
//! transaction here runs during the open sequence, or as a short insert or
//! update unlikely to be the first read of a damaged page.

use std::path::Path;

use rusqlite::{MappedRows, Params, Row};

use crate::integrity::corrupt_store_error;
use crate::{Result, StoreError};

/// Turn a raw rusqlite failure into a `StoreError`, naming `path` when the
/// caller has one. A free function, not a method: `Option<&str>` is `Copy`,
/// so a closure can capture it by value and never fight the borrow checker
/// over which field of `self` it holds.
fn wrap(path: Option<&str>, error: rusqlite::Error) -> StoreError {
    corrupt_store_error(error, path.map(Path::new))
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

    /// Starts a transaction. Names the path if SQLite refuses to start it.
    /// Everything run through the returned `Transaction` is plain rusqlite.
    /// See the module doc for why.
    pub(crate) fn transaction(&mut self) -> Result<rusqlite::Transaction<'_>> {
        let path = self.conn.path().map(str::to_string);
        self.conn
            .transaction()
            .map_err(|e| wrap(path.as_deref(), e))
    }

    pub(crate) fn transaction_with_behavior(
        &mut self,
        behavior: rusqlite::TransactionBehavior,
    ) -> Result<rusqlite::Transaction<'_>> {
        let path = self.conn.path().map(str::to_string);
        self.conn
            .transaction_with_behavior(behavior)
            .map_err(|e| wrap(path.as_deref(), e))
    }

    pub(crate) fn unchecked_transaction(&self) -> Result<rusqlite::Transaction<'_>> {
        self.conn
            .unchecked_transaction()
            .map_err(|e| wrap(self.conn.path(), e))
    }

    pub(crate) fn last_insert_rowid(&self) -> i64 {
        self.conn.last_insert_rowid()
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
        self.inner.column_names()
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
