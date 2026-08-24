// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The per-class error split beside `tool_usage_rollup` (#4550).
//!
//! `tool_usage_rollup.errors` counts every `state = 'error'` call as the same
//! failure, which is exactly the number #3145 says cannot mean anything: a
//! tool defect, a model mistake, and a policy refusal all land in it. This
//! module keys the same additive fold by [`stella_protocol::ErrorClass`]'s
//! wire token, so "error rate for `bash`, excluding `invalid_input`" is a SUM
//! over rows rather than a string match over messages.
//!
//! A sibling table rather than a column on `tool_usage_rollup`, because the
//! class is part of the key: splitting `errors` inside the existing
//! `(project, tool, surface, day)` primary key would rebuild that table for a
//! count that is additive anyway. Being a new table, schema convergence
//! creates it on existing hubs with no migration rung.
//!
//! The fold rides [`super::tool_fold`]'s claim — one claim covers both
//! tables, so they can only be counted together or not at all. Buckets folded
//! before this table existed have no class rows here; like `tool_report`'s
//! pre-v24 caveat, the split converges on the truth going forward rather than
//! pretending to know the past.

use rusqlite::{Transaction, params};

use super::{Result, UsageStore};

/// One per-class error bucket for one execution: how many of a tool's failed
/// calls fell in one [`stella_protocol::ErrorClass`].
///
/// `class` is the class's `snake_case` wire token, or the empty string for a
/// call whose error site has not been audited into a class — the same
/// spelling `tool_calls.error_class` stores, kept distinct from every real
/// class because "not audited" is not a class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorClassBucket {
    pub tool: String,
    pub surface: String,
    pub class: String,
    pub errors: i64,
}

/// One tool's cross-project error total for one class, straight off
/// `tool_error_class_rollup` — the classified counterpart of
/// [`super::ToolReportRow`]'s `errors`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolErrorClassRow {
    pub tool: String,
    pub surface: String,
    /// The [`stella_protocol::ErrorClass`] wire token, `""` for unclassified.
    pub class: String,
    pub errors: i64,
}

/// Fold one execution's class buckets into the per-day counts, inside the
/// caller's transaction and under the claim it already holds.
pub(super) fn fold(
    tx: &Transaction<'_>,
    project_id: &str,
    day: &str,
    buckets: &[ErrorClassBucket],
) -> Result<()> {
    for b in buckets {
        tx.execute(
            "INSERT INTO tool_error_class_rollup (project_id, tool, surface, day, class, errors) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(project_id, tool, surface, day, class) DO UPDATE SET \
               errors = errors + excluded.errors",
            params![project_id, b.tool, b.surface, day, b.class, b.errors],
        )?;
    }
    Ok(())
}

/// Drop a garbage-collected project's class rows, beside the bucket and
/// ledger deletes in [`super::UsageStore::prune`].
pub(super) fn gc_project(tx: &Transaction<'_>, project_id: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM tool_error_class_rollup WHERE project_id = ?1",
        params![project_id],
    )?;
    Ok(())
}

/// Age out class rows on exactly the predicate that ages out
/// `tool_usage_rollup` and the fold ledger, returning how many went — the
/// three tables share one claim, so they must share one cutoff.
pub(super) fn age_out(tx: &Transaction<'_>, modifier: &str) -> Result<u64> {
    Ok(tx.execute(
        "DELETE FROM tool_error_class_rollup WHERE day < date('now', ?1)",
        params![modifier],
    )? as u64)
}

impl UsageStore {
    /// Cross-project per-tool error totals split by class, largest first.
    ///
    /// The classified half of [`UsageStore::tool_report`]: same table family,
    /// same additive fold, with the class in the key. Unclassified errors
    /// appear under `class == ""` rather than vanishing, so the two reports'
    /// totals reconcile for every execution folded since the split existed.
    pub fn tool_error_class_report(&self) -> Result<Vec<ToolErrorClassRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT tool, surface, class, SUM(errors) AS e \
             FROM tool_error_class_rollup \
             GROUP BY tool, surface, class \
             ORDER BY e DESC, tool ASC, surface ASC, class ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ToolErrorClassRow {
                tool: row.get(0)?,
                surface: row.get(1)?,
                class: row.get(2)?,
                errors: row.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::rollup;
    use super::super::{ErrorClassBucket, PrunePolicy, ToolBucket, UsageStore};

    /// Two bash failures in one turn: one the model's fault, one the world's,
    /// plus one from a site not yet audited.
    fn classified_bash(execution_id: i64) -> super::super::ExecutionRollupRow {
        let mut row = rollup(
            execution_id,
            vec![ToolBucket {
                tool: "bash".into(),
                surface: "native".into(),
                calls: 5,
                errors: 3,
            }],
        );
        row.error_class_histogram = vec![
            ErrorClassBucket {
                tool: "bash".into(),
                surface: "native".into(),
                class: "invalid_input".into(),
                errors: 1,
            },
            ErrorClassBucket {
                tool: "bash".into(),
                surface: "native".into(),
                class: "environment".into(),
                errors: 1,
            },
            ErrorClassBucket {
                tool: "bash".into(),
                surface: "native".into(),
                class: String::new(),
                errors: 1,
            },
        ];
        row
    }

    /// The witness for #4550's definition of done, carried from #3145: "error
    /// rate for `bash`, excluding `invalid_input`" is arithmetic over
    /// classified rows — no string ever inspected.
    #[test]
    fn bash_errors_excluding_invalid_input_need_no_string_matching() {
        let usage = UsageStore::in_memory().unwrap();
        usage.sync_execution(&classified_bash(1)).unwrap();

        let split = usage.tool_error_class_report().unwrap();
        let bash_excluding_model_mistakes: i64 = split
            .iter()
            .filter(|r| r.tool == "bash" && r.class != "invalid_input")
            .map(|r| r.errors)
            .sum();
        assert_eq!(bash_excluding_model_mistakes, 2);

        let total: i64 = split.iter().map(|r| r.errors).sum();
        let report = usage.tool_report().unwrap();
        assert_eq!(
            total, report[0].errors,
            "the split's total reconciles with the unsplit errors column"
        );
    }

    /// The split rides the same fold claim as the calls/errors buckets: a
    /// re-sync adds nothing to either.
    #[test]
    fn a_re_sync_does_not_double_the_class_counts() {
        let usage = UsageStore::in_memory().unwrap();
        let row = classified_bash(7);
        usage.sync_execution(&row).unwrap();
        usage.sync_execution(&row).unwrap();

        let split = usage.tool_error_class_report().unwrap();
        assert_eq!(split.iter().map(|r| r.errors).sum::<i64>(), 3);
    }

    /// Prune's project GC and age cutoff take the class rows with the buckets
    /// and the claim, on the same predicates, so a later re-sync folds from
    /// zero instead of adding onto survivors.
    #[test]
    fn prune_takes_the_class_rows_with_their_buckets() {
        let usage = UsageStore::in_memory().unwrap();
        let row = classified_bash(7);

        usage.sync_execution(&row).unwrap();
        usage
            .prune(&PrunePolicy {
                gc_project_ids: vec![row.project_id.clone()],
                ..PrunePolicy::default()
            })
            .unwrap();
        assert!(usage.tool_error_class_report().unwrap().is_empty());
        usage.sync_execution(&row).unwrap();
        assert_eq!(
            usage
                .tool_error_class_report()
                .unwrap()
                .iter()
                .map(|r| r.errors)
                .sum::<i64>(),
            3,
            "a returning project counts from zero, never from residue"
        );

        usage
            .prune(&PrunePolicy {
                older_than: Some("-1 days".into()),
                ..PrunePolicy::default()
            })
            .unwrap();
        assert!(
            usage.tool_error_class_report().unwrap().is_empty(),
            "the age cutoff reaches the class rows on the buckets' predicate"
        );
    }
}
