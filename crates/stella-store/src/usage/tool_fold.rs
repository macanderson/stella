// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The ledger that makes the hub's tool-day fold idempotent (#3411).
//!
//! `tool_usage_rollup` is the one hub table [`super::UsageStore::sync_execution`]
//! **accumulates** into rather than replacing: a turn contributes
//! `calls = calls + excluded.calls` to a `(project, tool, surface, day)`
//! bucket. So the fold has to happen exactly once per execution, and until
//! this module the question "has it already?" was asked of `execution_rollup`
//! — a different table, with a different lifetime.
//!
//! That is where it broke. [`super::UsageStore::prune`] deletes from
//! `execution_rollup` on predicates `tool_usage_rollup` does not share (a
//! project GC, and an age cutoff over `started_at` versus one over `day`), so
//! the two go out of step by design. A re-sync then found no rollup row, read
//! that as "not folded yet", and added the histogram onto buckets that had
//! survived. `stella stats prune` calls `rewind_telemetry_cursor` immediately
//! afterwards (`stella-cli/src/stats.rs`), so the re-sync is not hypothetical:
//! it is the next thing that happens.
//!
//! The ledger is its own record of its own question, which is the whole point:
//! it cannot be deleted by a rule written for a different table. Claiming is
//! `INSERT OR IGNORE`, so the claim and the check are one atomic statement
//! inside the caller's transaction rather than a read followed by a write with
//! a window between them.
//!
//! # Why it carries a day
//!
//! A ledger that only ever grows is a leak, and pruning it on the wrong
//! predicate would restore the bug it fixes. It carries the execution's `day`
//! — the same string the bucket is keyed on, copied rather than re-derived —
//! and [`age_out`] deletes on **exactly** the predicate that ages out
//! `tool_usage_rollup`. The two can then only disappear together, so a re-sync
//! after an age cutoff never finds surviving buckets to add to.

use rusqlite::{Transaction, params};

use crate::Result;

/// Claim the tool-day fold for one execution, returning whether this caller
/// won it. `false` means the histogram has already been counted and must not
/// be added again.
pub(super) fn claim(
    tx: &Transaction<'_>,
    project_id: &str,
    execution_id: i64,
    day: &str,
) -> Result<bool> {
    Ok(tx.execute(
        "INSERT OR IGNORE INTO tool_fold_ledger (project_id, execution_id, day) \
         VALUES (?1, ?2, ?3)",
        params![project_id, execution_id, day],
    )? == 1)
}

/// Drop a garbage-collected project's claims, so a project that is later seen
/// again folds from zero exactly as a new one does.
pub(super) fn forget_project(tx: &Transaction<'_>, project_id: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM tool_fold_ledger WHERE project_id = ?1",
        params![project_id],
    )?;
    Ok(())
}

/// Drop claims for days whose buckets the age cutoff has taken.
///
/// `modifier` is the SQLite datetime modifier the caller applied to
/// `tool_usage_rollup`, and the predicate is deliberately identical to that
/// one: a claim must never outlive its bucket, and a bucket must never outlive
/// its claim.
pub(super) fn age_out(tx: &Transaction<'_>, modifier: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM tool_fold_ledger WHERE day < date('now', ?1)",
        params![modifier],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    // `rollup` is the fixture `super::super::tests` already builds; a second
    // copy here would be one more thing to keep in step with the row's shape.
    use super::super::tests::rollup;
    use super::super::{PrunePolicy, ToolBucket, UsageStore};

    fn one_grep(execution_id: i64) -> super::super::ExecutionRollupRow {
        rollup(
            execution_id,
            vec![ToolBucket {
                tool: "grep".into(),
                surface: "native".into(),
                calls: 2,
                errors: 0,
            }],
        )
    }

    fn grep_calls(usage: &UsageStore) -> i64 {
        usage
            .tool_report()
            .unwrap()
            .iter()
            .find(|row| row.tool == "grep")
            .map_or(0, |row| row.calls)
    }

    /// The witness for #3411. `prune` deletes an execution's `execution_rollup`
    /// row on predicates `tool_usage_rollup` does not share, and
    /// `stella stats prune` rewinds the replication cursor immediately
    /// afterwards (`stella-cli/src/stats.rs`), so the re-sync is the next thing
    /// that happens. Asking `execution_rollup` "have I folded this?" then read
    /// the missing row as "not yet" and added the histogram onto the buckets
    /// that had survived.
    #[test]
    fn a_re_sync_after_the_rollup_row_is_gone_does_not_re_fold() {
        let usage = UsageStore::in_memory().unwrap();
        let row = one_grep(7);
        usage.sync_execution(&row).unwrap();
        assert_eq!(grep_calls(&usage), 2);

        // Exactly what prune does to that table, and to that table alone.
        usage
            .lock()
            .execute("DELETE FROM execution_rollup", [])
            .unwrap();
        usage.sync_execution(&row).unwrap();

        assert_eq!(
            grep_calls(&usage),
            2,
            "the fold is claimed once per execution, not once per surviving rollup row"
        );
    }

    /// The other direction: a garbage-collected project must fold from zero
    /// when it is seen again, or its counts would be stuck at whatever the GC
    /// left behind.
    #[test]
    fn a_gc_d_project_folds_from_zero_when_it_returns() {
        let usage = UsageStore::in_memory().unwrap();
        let row = one_grep(7);
        usage.sync_execution(&row).unwrap();

        usage
            .prune(&PrunePolicy {
                gc_project_ids: vec![row.project_id.clone()],
                ..PrunePolicy::default()
            })
            .unwrap();
        assert_eq!(grep_calls(&usage), 0, "the GC took the buckets");

        usage.sync_execution(&row).unwrap();
        assert_eq!(
            grep_calls(&usage),
            2,
            "and the claim with them, so the project counts again from zero"
        );
    }

    /// A claim must never outlive its bucket. The age cutoff takes both on one
    /// predicate, so a re-sync after it re-folds a day that is genuinely empty
    /// rather than adding onto one that is not.
    #[test]
    fn the_age_cutoff_takes_a_claim_and_its_bucket_together() {
        let usage = UsageStore::in_memory().unwrap();
        let row = one_grep(7);
        usage.sync_execution(&row).unwrap();

        usage
            .prune(&PrunePolicy {
                older_than: Some("-1 days".into()),
                ..PrunePolicy::default()
            })
            .unwrap();
        assert_eq!(grep_calls(&usage), 0, "the fixture's day is long past");

        usage.sync_execution(&row).unwrap();
        assert_eq!(
            grep_calls(&usage),
            2,
            "an aged-out day re-folds to its true count, never to double it"
        );
    }
}
