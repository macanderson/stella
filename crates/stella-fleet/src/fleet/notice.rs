//! The per-handle warning lines `stella fleet`'s end-of-run report prints —
//! one line per durable failure a settled attempt survived: a ledger close
//! that failed after the worker settled ([`TaskHandle::ledger_error`]) and a
//! dispatch lease lost mid-run ([`TaskHandle::lease_loss`], #1677).
//!
//! The text is composed here rather than in `stella-cli/src/fleet_cmd.rs`
//! because that file is grandfathered at its file-size ceiling and closed to
//! growth (AGENTS.md § God files) — the CLI iterates the returned lines and
//! owns only color and layout.

use super::TaskHandle;

/// The warning lines the fleet report prints under `handle`'s row, in a
/// fixed order (ledger close first, lease loss second). Empty for the
/// common, healthy attempt. Plain text — the caller owns color and layout,
/// and prints each line exactly once.
#[must_use]
pub fn handle_notices(handle: &TaskHandle) -> Vec<String> {
    let mut notices = Vec::new();
    if let Some(error) = &handle.ledger_error {
        // The in-memory result is authoritative; what was lost is the
        // durable row. Silent here would mean the only witness to an open
        // attempts row is a future `stella doctor`.
        notices.push(format!(
            "attempt not recorded in the fleet ledger ({error}) — the row stays open; \
             `stella doctor` will report it"
        ));
    }
    if let Some(loss) = &handle.lease_loss {
        // The worker was deliberately left to finish (never killed
        // mid-flight), so the result above is real work — but a rival may
        // have produced the same work in parallel, and this line is the one
        // place that overlap reaches a human.
        notices.push(match &loss.taken_by {
            Some(owner) => format!(
                "task `{}` lost its dispatch lease mid-run to session `{owner}` — two workers \
                 may have run this task; review both results before keeping either",
                handle.task_id
            ),
            None => format!(
                "task `{}` lost its dispatch lease mid-run (no rival holder was readable) — \
                 the attempt finished without a live claim on the task",
                handle.task_id
            ),
        });
    }
    notices
}

#[cfg(test)]
mod tests {
    use stella_core::BudgetOutcome;

    use super::super::{LeaseLoss, TaskHandle, WorkerOutcome};
    use super::*;

    fn handle() -> TaskHandle {
        TaskHandle {
            task_id: "t1".to_string(),
            attempt_id: 1,
            outcome: WorkerOutcome {
                cost_usd: 0.0,
                commits: Vec::new(),
                summary: String::new(),
                success: true,
            },
            worktree: None,
            budget: BudgetOutcome::Continue,
            ledger_error: None,
            lease_loss: None,
        }
    }

    #[test]
    fn a_healthy_attempt_has_no_notices() {
        assert!(handle_notices(&handle()).is_empty());
    }

    #[test]
    fn a_lost_lease_names_the_task_and_the_session_that_took_it_over() {
        let mut h = handle();
        h.lease_loss = Some(LeaseLoss {
            taken_by: Some("run-b".to_string()),
        });
        let notices = handle_notices(&h);
        assert_eq!(notices.len(), 1, "one loss, one line");
        assert!(notices[0].contains("`t1`"), "names the task: {notices:?}");
        assert!(
            notices[0].contains("`run-b`"),
            "names the rival session: {notices:?}"
        );
    }

    #[test]
    fn a_lost_lease_with_no_readable_rival_still_reports() {
        let mut h = handle();
        h.lease_loss = Some(LeaseLoss { taken_by: None });
        let notices = handle_notices(&h);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("lost its dispatch lease"));
    }

    #[test]
    fn ledger_and_lease_notices_stack_in_a_fixed_order() {
        let mut h = handle();
        h.ledger_error = Some("disk I/O error".to_string());
        h.lease_loss = Some(LeaseLoss { taken_by: None });
        let notices = handle_notices(&h);
        assert_eq!(notices.len(), 2);
        assert!(notices[0].contains("not recorded in the fleet ledger"));
        assert!(notices[0].contains("disk I/O error"));
        assert!(notices[1].contains("lost its dispatch lease"));
    }
}
