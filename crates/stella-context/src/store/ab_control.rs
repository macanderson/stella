//! The durable turn counter behind the A/B recall control (#1221).
//!
//! One row per named experiment, holding how many turns have consulted it in
//! this workspace. It is here — in `context.db`, the store recall itself reads
//! — rather than in the caller's process because the surfaces that most need
//! the measurement are **one turn per process**: `stella run`, a fleet task, a
//! `/goal` invocation. A per-session counter on those either never reaches the
//! rate (so no control turn ever happens) or, seeded differently, suppresses
//! every turn. Neither is an experiment.
//!
//! Increment is a **single statement**, not a read-then-write. Two stella
//! processes against one workspace — a deck beside a `stella run`, or a fleet
//! — hold the write lock for the whole `INSERT … ON CONFLICT … RETURNING`, so
//! they serialize (the connection pragmas set `busy_timeout`) and each claims
//! a distinct turn number. A read-then-write pair could hand both processes
//! the same number, which would put two turns in the same arm and count them
//! as one.
//!
//! The counter only ever rises; it is deliberately not reset by a rate change.
//! The schedule shifts once when the rate changes and stays exact after, which
//! is a smaller distortion than restarting the count (and thereby delaying the
//! next control turn by a whole period) every time someone edits a setting.

use rusqlite::{Connection, params};

use crate::error::ContextError;

/// Claim the next turn number for `experiment`, atomically. The first call
/// against a fresh workspace returns 1.
pub(crate) fn next_turn(
    conn: &Connection,
    experiment: &str,
    now: &str,
) -> Result<u64, ContextError> {
    let turns: i64 = conn.query_row(
        "INSERT INTO ab_control_counter (experiment, turns, updated_at)
         VALUES (?1, 1, ?2)
         ON CONFLICT(experiment) DO UPDATE SET
             turns = turns + 1,
             updated_at = excluded.updated_at
         RETURNING turns",
        params![experiment, now],
        |r| r.get(0),
    )?;
    // Saturating rather than wrapping: `u64::MAX` turns is unreachable, and a
    // negative row (only reachable by hand-editing the file) must degrade to
    // "turn 0" — never suppress — rather than panic on the cast.
    Ok(u64::try_from(turns).unwrap_or(0))
}

/// How many turns `experiment` has counted so far, without claiming one.
/// `0` when it has never run here. Read-only: for reporting over the schedule,
/// never for deciding a turn's arm — that decision must claim.
pub(crate) fn turns_so_far(conn: &Connection, experiment: &str) -> Result<u64, ContextError> {
    use rusqlite::OptionalExtension as _;
    let turns: Option<i64> = conn
        .query_row(
            "SELECT turns FROM ab_control_counter WHERE experiment = ?1",
            params![experiment],
            |r| r.get(0),
        )
        .optional()?;
    Ok(turns.map(|t| u64::try_from(t).unwrap_or(0)).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::clock::SystemClock;
    use crate::embed::HashEmbedder;
    use crate::store::ContextStore;

    fn store(dir: &std::path::Path) -> ContextStore {
        ContextStore::open_with(
            dir.join("context.db"),
            Arc::new(HashEmbedder::default()),
            Arc::new(SystemClock),
        )
        .expect("open")
    }

    #[test]
    fn the_counter_starts_at_one_and_rises_by_one() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        assert_eq!(s.ab_control_turns_so_far("recall").unwrap(), 0);
        assert_eq!(s.next_ab_control_turn("recall").unwrap(), 1);
        assert_eq!(s.next_ab_control_turn("recall").unwrap(), 2);
        assert_eq!(s.next_ab_control_turn("recall").unwrap(), 3);
        assert_eq!(s.ab_control_turns_so_far("recall").unwrap(), 3);
    }

    /// The whole point of the durable home: `stella run` is one turn per
    /// process, so the count has to survive the process that made it.
    #[test]
    fn the_count_survives_the_process_that_made_it() {
        let dir = tempfile::tempdir().unwrap();
        for expected in 1..=4 {
            let s = store(dir.path());
            assert_eq!(s.next_ab_control_turn("recall").unwrap(), expected);
        }
        // And a reader that only wants the number does not advance it.
        let s = store(dir.path());
        assert_eq!(s.ab_control_turns_so_far("recall").unwrap(), 4);
        assert_eq!(s.ab_control_turns_so_far("recall").unwrap(), 4);
    }

    /// Experiments are independent rows: adding a second one must not
    /// renumber the first, or every schedule shifts the day one is added.
    #[test]
    fn experiments_count_independently() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        assert_eq!(s.next_ab_control_turn("recall").unwrap(), 1);
        assert_eq!(s.next_ab_control_turn("recall").unwrap(), 2);
        assert_eq!(s.next_ab_control_turn("other").unwrap(), 1);
        assert_eq!(s.next_ab_control_turn("recall").unwrap(), 3);
        assert_eq!(s.ab_control_turns_so_far("other").unwrap(), 1);
    }
}
