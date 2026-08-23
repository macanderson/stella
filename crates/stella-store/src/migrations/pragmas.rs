// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The connection pragmas every opener runs before the version ladder: WAL,
//! the busy handling a concurrent first open needs, and the [`Durability`]
//! level `STELLA_STORE_DURABILITY` selects.
//!
//! This is not a migration — it reshapes nothing and runs on every open, not
//! once per version — but it belongs to the same "before the store is usable"
//! moment, which is why it stayed in [`super`] for as long as it did and why
//! it is re-exported from there rather than moved out of the module.

use rusqlite::Connection;

/// How hard the store tries to survive the machine going away, selected by
/// `STELLA_STORE_DURABILITY`.
///
/// The three levels are not a style preference — they are three genuinely
/// different failure models, and the numbers below are measured on this
/// workspace's own write path (one transaction per event, 2 KiB payload,
/// APFS on SSD), not assumed:
///
/// | level      | ms/event | survives process kill | survives kernel panic | survives power loss |
/// |------------|----------|-----------------------|-----------------------|---------------------|
/// | `normal`   | 0.022    | yes                   | **no**                | **no**              |
/// | `full`     | 0.037    | yes                   | yes                   | **no** (see below)  |
/// | `paranoid` | 3.99     | yes                   | yes                   | yes                 |
///
/// `full` is the default, changed from `normal`, because it closes the
/// kernel-panic and forced-reboot window for **15 microseconds an event** —
/// about 75 ms across a whole turn, which no one can perceive. Telemetry that
/// was committed should not evaporate because the machine was restarted
/// ungracefully.
///
/// `paranoid` adds `fullfsync`, which on macOS is what actually forces the
/// **drive's own write cache** to disk — a plain `fsync()` there returns once
/// the data reaches the drive, not once the drive has persisted it. It is the
/// only level that genuinely survives losing power, and it costs 180× per
/// event: roughly twenty seconds of pure fsync across a turn that makes a few
/// thousand events. That is a work stoppage, not a trade-off, which is why it
/// is opt-in rather than the default despite being the only complete answer.
///
/// The honest summary for an operator: at the default, a crash loses nothing
/// and a power cut can lose the last few seconds of *raw* events. What it
/// cannot lose is everything derived from them —
/// [`Store::reconcile_interrupted_executions`](crate::Store::reconcile_interrupted_executions)
/// rebuilds the projections from whatever log survived, which is the part
/// that used to be lost permanently and the reason this table is a tail risk
/// rather than the main one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Durability {
    /// `synchronous=NORMAL` — the pre-v18 setting.
    Normal,
    /// `synchronous=FULL`, no `fullfsync`. The default.
    Full,
    /// `synchronous=FULL` + `fullfsync`. Survives power loss; 180× the cost.
    Paranoid,
}

impl Durability {
    /// Read `STELLA_STORE_DURABILITY`.
    pub(crate) fn from_env() -> Self {
        Self::parse(&std::env::var("STELLA_STORE_DURABILITY").unwrap_or_default())
    }

    /// Parse one level name. An empty or unrecognized value is
    /// [`Self::Full`]: a typo in an environment variable must not silently
    /// downgrade a durability guarantee, and the safe direction is the
    /// default anyway.
    ///
    /// Split from [`Self::from_env`] so the mapping is testable without
    /// touching the process environment — `set_var` is a global mutation, and
    /// a test binary runs its cases on many threads at once, so pinning this
    /// contract through the environment would race every other test.
    pub(crate) fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "normal" => Self::Normal,
            "paranoid" => Self::Paranoid,
            _ => Self::Full,
        }
    }

    /// The pragma pair this level sets.
    fn pragmas(self) -> &'static str {
        match self {
            Self::Normal => "PRAGMA synchronous=NORMAL; PRAGMA fullfsync=0;",
            Self::Full => "PRAGMA synchronous=FULL; PRAGMA fullfsync=0;",
            Self::Paranoid => "PRAGMA synchronous=FULL; PRAGMA fullfsync=1;",
        }
    }
}

/// Runs the connection pragmas, absorbing a concurrent first-open's
/// `SQLITE_BUSY`.
///
/// `busy_timeout` is set first so every statement after it can wait, but that
/// is not enough on its own: converting a fresh rollback-journal database into
/// WAL needs an exclusive lock, and **SQLite skips the busy handler on that
/// upgrade** to avoid deadlock. So two processes opening the same new workspace
/// at once — a fleet run beside a `stella stats`, or four threads in a test —
/// gave one of them an immediate `database is locked` from `journal_mode=WAL`
/// itself, which no timeout could absorb, and `Store::open` failed outright.
/// Backing off lets the loser observe the settled WAL file instead.
///
/// Only first opens are affected: once the file is WAL the pragma is a no-op
/// and takes no lock. The main store never got this treatment (#617 item 8);
/// the one place that already did it was `stella-media`'s journal, which left
/// the workspace with that crate (#3236).
///
/// WAL also means a read-only caller (`stella stats`) is never blocked by a
/// live session's writes; the `synchronous`/`fullfsync` pair is chosen by
/// [`Durability`], which documents what each level actually survives and what
/// it costs per event.
pub(crate) fn initialize_store_pragmas(
    conn: &Connection,
) -> std::result::Result<(), rusqlite::Error> {
    const BUSY_ATTEMPTS: u32 = 40;
    const BUSY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(50);

    let durability = Durability::from_env().pragmas();
    let mut attempts = 0;
    loop {
        // `execute_batch` tolerates the row `PRAGMA journal_mode` returns (a
        // plain `pragma_update` errors on it).
        let result = conn.execute_batch(&format!(
            "PRAGMA busy_timeout=5000;
             PRAGMA journal_mode=WAL;
             {durability}
             PRAGMA foreign_keys=ON;"
        ));
        match result {
            Err(error)
                if attempts < BUSY_ATTEMPTS
                    && error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy) =>
            {
                attempts += 1;
                std::thread::sleep(BUSY_BACKOFF);
            }
            other => return other,
        }
    }
}
