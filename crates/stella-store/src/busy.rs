// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Asking again when the database says it is busy.
//!
//! WAL lets one writer work at a time. A second one that asks while the first
//! holds the lock is turned away. It is not put in a queue. Store connections
//! set a wait, so short holds pass by. What reaches a caller is a hold that
//! lasted longer than that wait. A caller that takes it as final loses the row.
//!
//! [`retry_busy`] asks again a few times. It also says how many tries it took,
//! so a caller knows whether a retry ran.
//!
//! [`is_busy`] is the test the file opener uses too. Both read it from here,
//! so there is one answer to "was that the lock, or the data?".

use std::time::Duration;

use crate::Result;

/// How many tries a busy write gets before it is dropped.
const MAX_ATTEMPTS: u32 = 5;

/// The wait before the second try. Each later wait doubles. Five tries wait
/// 300 ms in all. That is long enough to outlast a peer turn's write, and
/// short enough that a turn's exit never stalls on it.
const FIRST_BACKOFF: Duration = Duration::from_millis(20);

/// Whether a lock was held, rather than the query or the data being wrong.
///
/// Both codes mean "ask again later". One is another connection holding the
/// write lock. The other is another query on this one.
#[must_use]
pub fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// How a write ended, and how many tries it took.
#[derive(Debug)]
pub struct BusyRetry<T> {
    /// What the last attempt returned.
    pub outcome: Result<T>,
    /// Tries spent, counting the one that settled it. Never below 1.
    pub attempts: u32,
}

impl<T> BusyRetry<T> {
    /// Whether the write was asked for more than once.
    #[must_use]
    pub fn retried(&self) -> bool {
        self.attempts > 1
    }
}

/// Run `write`, asking again while a lock is held.
///
/// Any other failure comes back on the first try. A missing table or a bad
/// value does not get better by waiting. Asking again would only make the
/// same failure slower.
///
/// The write is run from the start each time, so it has to be safe to
/// repeat. Each store write is one transaction, so a turned-away try wrote
/// nothing.
pub fn retry_busy<T>(mut write: impl FnMut() -> Result<T>) -> BusyRetry<T> {
    let mut backoff = FIRST_BACKOFF;
    for attempt in 1..MAX_ATTEMPTS {
        match write() {
            Err(error) if error.is_busy() => {
                std::thread::sleep(backoff);
                backoff *= 2;
            }
            outcome => {
                return BusyRetry {
                    outcome,
                    attempts: attempt,
                };
            }
        }
    }
    BusyRetry {
        outcome: write(),
        attempts: MAX_ATTEMPTS,
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rusqlite::Connection;

    use super::{MAX_ATTEMPTS, retry_busy};
    use crate::{Store, StoreError};

    /// A workspace with a real `store.db`, plus the path to that file.
    fn opened_workspace() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("store");
        drop(store);
        let path = dir.path().join(".stella/private/store.db");
        assert!(
            path.exists(),
            "the file the two connections will contend on"
        );
        (dir, path)
    }

    /// A connection that reports a held lock at once instead of waiting, so a
    /// test never depends on how long a timeout is.
    fn impatient(path: &Path) -> Connection {
        let conn = Connection::open(path).expect("open");
        conn.busy_timeout(std::time::Duration::ZERO)
            .expect("no waiting");
        conn
    }

    fn insert_one_use(conn: &Connection) -> crate::Result<()> {
        conn.execute(
            "INSERT INTO agent_uses (execution_id, agent, version, reason, kind) \
             VALUES (1, 'reviewer', 1, '', 'definition')",
            [],
        )?;
        Ok(())
    }

    /// **The witness.** A write refused because a competing transaction held
    /// the write lock lands once that transaction commits. Before this helper
    /// existed the first refusal was the whole answer, and the row was lost.
    #[test]
    fn a_write_refused_for_a_held_lock_lands_after_the_lock_clears() {
        let (_dir, path) = opened_workspace();
        let holder = impatient(&path);
        let writer = impatient(&path);
        holder
            .execute_batch("BEGIN IMMEDIATE")
            .expect("hold the write lock");

        let mut attempt = 0;
        let retry = retry_busy(|| {
            attempt += 1;
            // The competing turn commits between the first refusal and the
            // second ask, which is the sequence this helper exists to survive.
            if attempt == 2 {
                holder.execute_batch("COMMIT").expect("release");
            }
            insert_one_use(&writer)
        });

        assert!(retry.outcome.is_ok(), "the row lands: {:?}", retry.outcome);
        assert_eq!(retry.attempts, 2);
        assert!(retry.retried());
    }

    /// Bounded: a lock nobody releases costs a fixed number of attempts and
    /// then reports the code it was refused with.
    #[test]
    fn a_lock_that_never_clears_gives_up_after_a_bounded_number_of_attempts() {
        let (_dir, path) = opened_workspace();
        let holder = impatient(&path);
        let writer = impatient(&path);
        holder
            .execute_batch("BEGIN IMMEDIATE")
            .expect("hold the write lock");

        let retry = retry_busy(|| insert_one_use(&writer));

        assert_eq!(retry.attempts, MAX_ATTEMPTS);
        let error = retry.outcome.expect_err("the lock is still held");
        assert!(error.is_busy(), "and says so: {error}");
        assert!(error.sqlite_code().is_some(), "with a code to report");
    }

    /// A failure that waiting cannot fix is answered at once. Retrying a
    /// missing table would only make the same failure slower.
    #[test]
    fn a_failure_that_is_not_a_lock_is_answered_on_the_first_attempt() {
        let (_dir, path) = opened_workspace();
        let writer = impatient(&path);

        let retry = retry_busy(|| {
            writer.execute("INSERT INTO no_such_table (x) VALUES (1)", [])?;
            Ok(())
        });

        assert_eq!(retry.attempts, 1);
        assert!(!retry.retried());
        let error = retry.outcome.expect_err("there is no such table");
        assert!(!error.is_busy(), "not a lock: {error}");
        assert!(matches!(error, StoreError::Sqlite(_)));
    }
}
