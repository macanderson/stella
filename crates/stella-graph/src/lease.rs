// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Single-flight leases over the expensive passes (#3650).
//!
//! # What this protects, and what it deliberately does not
//!
//! Six things write `codegraph.db`: the mount reconcile, the mount catch-up
//! walk, the live watcher's debounced batches, the full `index_all` that every
//! graph-tool open runs, the lazy embed catch-up inside a search, and any
//! second `stella` process in the same workspace. Before this module the only
//! thing between them was `PRAGMA busy_timeout=5000`, which makes them correct
//! (SQLite's write lock does that) but not *efficient*: two processes each
//! walk and hash the whole tree to produce one result, and under a large tree
//! the loser can exhaust the timeout and surface as an `index_warning` the
//! search path degrades past.
//!
//! So a lease is guarded **only where duplicated work is genuinely redundant**:
//!
//! - [`Purpose::IndexWalk`] — a whole-tree walk. Two of them produce the same
//!   rows, so the second is pure waste and skipping it loses nothing.
//! - [`Purpose::Embed`] — an embedding pass. This one spends the user's API
//!   budget, so a duplicate is worse than waste.
//!
//! **Scoped passes are deliberately unguarded.** The watcher's `apply_changes`
//! batches and the reconcile's committed change set are bounded, cheap, and —
//! this is the load-bearing part — *not covered by whatever else holds the
//! lease*. A concurrent scoped pass indexes different files. Skipping one
//! because someone else is busy would silently drop an update, which is a
//! correctness loss traded for a performance win nobody asked for. A lease is
//! an optimization; it may never be the reason a file goes unindexed.
//!
//! # Fail open, never closed
//!
//! Every failure here resolves to "do the work". An unacquirable lease because
//! the table is missing, the store is read-only, or SQLite returned something
//! unexpected must never block indexing: the correctness story lives in the
//! transaction, not in this row. The only outcome that skips work is a
//! *successful* read saying someone else holds an unexpired lease.
//!
//! # Expiry is mandatory
//!
//! A process killed mid-walk cannot be allowed to wedge indexing forever, and
//! there is no reaper to clean up after it. So a lease carries a deadline and
//! a stale one is stolen rather than honored. That makes the TTL a real
//! parameter: too short and two passes overlap anyway, too long and a crash
//! costs a user that much dead time.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::GraphError;
use crate::store;

/// How long a lease stays valid before another pass may steal it.
///
/// Ten minutes is chosen against the operation being protected, not against a
/// round number: a whole-tree walk over a large monorepo is minutes, and a
/// TTL under it would let a second pass start while the first is still
/// legitimately working — the exact duplication this exists to prevent. The
/// cost of the other direction is bounded and visible: a process killed
/// mid-walk delays the next walk by at most this long, and the store is
/// unchanged in the meantime because the killed transaction rolled back.
pub const LEASE_TTL_SECONDS: i64 = 600;

/// What a lease covers. Two purposes never contend with each other: an index
/// walk and an embedding pass touch different tables and are useful
/// concurrently, so they are separate rows rather than one global lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// A whole-tree walk (`index_tree`). Scoped passes do not take this.
    IndexWalk,
    /// An embedding pass, file or chunk.
    Embed,
}

impl Purpose {
    /// The stored key. Stable strings: they are a primary key on disk, so
    /// renaming one silently orphans every live lease under the old name.
    fn key(self) -> &'static str {
        match self {
            Purpose::IndexWalk => "index_walk",
            Purpose::Embed => "embed",
        }
    }
}

/// A held lease. Releasing is explicit rather than a `Drop` impl, because the
/// release is a database write that can fail, and a `Drop` that swallows a
/// failure is how a lease outlives its holder without anyone noticing. The
/// expiry is the real backstop either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    purpose: Purpose,
    holder: String,
}

/// The outcome of asking for a lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Acquired {
    /// This caller holds it and must do the work.
    Held(Lease),
    /// Someone else holds an unexpired lease for this purpose. Skip; they are
    /// doing the work. This is the *only* outcome that skips.
    Busy,
}

/// A holder identity, unique per acquisition.
///
/// Three components, each covering what the others cannot. The process id
/// separates concurrent processes but is reused after one exits, so a
/// recycled id could otherwise release a lease it never took. The clock
/// separates those reuses across time — but only to the second, which is far
/// coarser than two acquisitions in one process can be. The counter closes
/// exactly that gap, and it is what makes "release only what you still hold"
/// a real check rather than one that passes by coincidence.
fn holder_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}:{}:{}",
        std::process::id(),
        store::now_unix(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// Take the lease for `purpose`, or report that someone else has it.
///
/// The acquire is one statement so it is atomic against another process doing
/// the same thing: the upsert only fires when there is no row or the existing
/// row has expired, and `changes()` reports whether this caller was the one
/// that wrote it. Doing it as a `SELECT` followed by an `INSERT` would leave
/// exactly the window two racing passes need to both win.
pub fn acquire(conn: &Connection, purpose: Purpose) -> Acquired {
    match try_acquire(conn, purpose) {
        Ok(acquired) => acquired,
        // Fail open: a store that cannot answer the lease question must not
        // stop the pass. Worst case the two passes contend on the write lock,
        // which is exactly where they were before this module existed.
        Err(_) => Acquired::Held(Lease {
            purpose,
            holder: holder_id(),
        }),
    }
}

fn try_acquire(conn: &Connection, purpose: Purpose) -> Result<Acquired, GraphError> {
    let holder = holder_id();
    let now = store::now_unix();
    let changed = conn.execute(
        "INSERT INTO code_graph_leases (purpose, holder, expires_at) \
         VALUES (?1, ?2, ?3) \
         ON CONFLICT(purpose) DO UPDATE SET \
           holder = excluded.holder, \
           expires_at = excluded.expires_at \
         WHERE code_graph_leases.expires_at <= ?4",
        params![purpose.key(), holder, now + LEASE_TTL_SECONDS, now],
    )?;
    if changed == 0 {
        return Ok(Acquired::Busy);
    }
    Ok(Acquired::Held(Lease { purpose, holder }))
}

/// Give up a lease this caller holds.
///
/// Scoped to the holder: a pass whose lease already expired and was stolen by
/// someone else must not delete the new holder's row on its way out. Absent or
/// reassigned rows are a no-op, never an error.
pub fn release(conn: &Connection, lease: &Lease) {
    let _ = conn.execute(
        "DELETE FROM code_graph_leases WHERE purpose = ?1 AND holder = ?2",
        params![lease.purpose.key(), lease.holder],
    );
}

/// Who holds `purpose` right now, if anyone whose lease has not expired.
/// Test- and diagnostic-facing; the acquire path does not consult it.
pub fn current_holder(conn: &Connection, purpose: Purpose) -> Option<String> {
    conn.query_row(
        "SELECT holder FROM code_graph_leases \
         WHERE purpose = ?1 AND expires_at > ?2",
        params![purpose.key(), store::now_unix()],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = store::open(&dir.path().join("codegraph.db")).expect("open");
        (dir, conn)
    }

    #[test]
    fn one_caller_wins_and_the_second_is_told_to_skip() {
        let (_dir, conn) = store_conn();
        assert!(matches!(
            acquire(&conn, Purpose::IndexWalk),
            Acquired::Held(_)
        ));
        assert_eq!(acquire(&conn, Purpose::IndexWalk), Acquired::Busy);
    }

    #[test]
    fn releasing_lets_the_next_caller_in() {
        let (_dir, conn) = store_conn();
        let Acquired::Held(lease) = acquire(&conn, Purpose::IndexWalk) else {
            panic!("first acquire must win");
        };
        assert_eq!(acquire(&conn, Purpose::IndexWalk), Acquired::Busy);
        release(&conn, &lease);
        assert!(matches!(
            acquire(&conn, Purpose::IndexWalk),
            Acquired::Held(_)
        ));
    }

    /// The crash case: a holder that never released must not wedge indexing
    /// forever, because nothing reaps on its behalf.
    #[test]
    fn an_expired_lease_is_stolen_rather_than_honored() {
        let (_dir, conn) = store_conn();
        assert!(matches!(
            acquire(&conn, Purpose::IndexWalk),
            Acquired::Held(_)
        ));
        // Backdate the deadline: the holder is gone and its lease is stale.
        conn.execute(
            "UPDATE code_graph_leases SET expires_at = ?1",
            params![store::now_unix() - 1],
        )
        .expect("backdate");
        assert!(
            matches!(acquire(&conn, Purpose::IndexWalk), Acquired::Held(_)),
            "a stale lease must be stealable or a killed process wedges the index"
        );
    }

    #[test]
    fn the_two_purposes_do_not_contend() {
        let (_dir, conn) = store_conn();
        assert!(matches!(
            acquire(&conn, Purpose::IndexWalk),
            Acquired::Held(_)
        ));
        assert!(
            matches!(acquire(&conn, Purpose::Embed), Acquired::Held(_)),
            "an embedding pass and a tree walk are useful concurrently"
        );
    }

    /// A pass whose lease expired and was taken by someone else must not
    /// delete the new holder's row when it finally finishes.
    #[test]
    fn releasing_a_stolen_lease_does_not_evict_the_new_holder() {
        let (_dir, conn) = store_conn();
        let Acquired::Held(first) = acquire(&conn, Purpose::Embed) else {
            panic!("first acquire must win");
        };
        conn.execute(
            "UPDATE code_graph_leases SET expires_at = ?1",
            params![store::now_unix() - 1],
        )
        .expect("backdate");
        let Acquired::Held(_second) = acquire(&conn, Purpose::Embed) else {
            panic!("the stale lease must be stealable");
        };

        release(&conn, &first);
        assert!(
            current_holder(&conn, Purpose::Embed).is_some(),
            "the slow first pass must not release the lease it no longer owns"
        );
    }
}
