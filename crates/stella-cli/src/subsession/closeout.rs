//! A worker's store closeout — the execution's audit record, and deliberately
//! nothing keyed to the session's task board (#1708).
//!
//! Until #1708 the closeout also mirrored the worker's own board into the
//! `tasks` table under the LEAD's session id, "so `tasks` queries see
//! sub-agent boards too". At the table's `UNIQUE(session_id, task_id)` key
//! that goal is unachievable and the write was pure corruption, twice over:
//!
//! * **Ordinal collision.** A worker's private board numbers from "1" in its
//!   own namespace, so its task "1" upserted over the lead's task "1" — a
//!   different task, different subject, different owner — and no reader could
//!   tell the surviving mixture apart.
//! * **It bypassed `/clear`'s seal** (#1692). The seal that quarantines a
//!   pre-clear worker's board report lives on the driver
//!   ([`SubSessions::seal_task_board`](super::SubSessions::seal_task_board),
//!   consumed by `session_clear::settle_worker_task`), and this closeout runs
//!   on the worker's own thread where no seal can reach — so a worker that
//!   predated a `/clear` repopulated the persisted mirror the user destroyed.
//!
//! The fix is subtraction, not plumbing: a worker's private board is
//! scaffolding for its one delegated run — the same lifetime the pipeline
//! gives an authored witness — and is discarded with the run. The delegation
//! outcome the session cares about is the LEAD's board row, which the driver
//! already mirrors at both of its own write sites (the lead's turn end, and
//! worker settlement in `session_clear::settle_worker_task`). With this
//! module holding the only worker-side closeout, the session's `tasks` rows
//! have exactly one writer — the driver — which is what makes the `/clear`
//! seal airtight rather than advisory.
//!
//! The seam is a module of its own for two reasons: `subsession.rs` sits just
//! under the file-size gate's ceiling, and the invariant needs a witness —
//! the tests below drive this exact production path against a real store.

use std::sync::Arc;

use stella_store::Store;
use stella_tools::ToolRegistry;

use crate::agent;

/// Close out a worker's execution row: files touched, citations, agent and
/// MCP usage, and the outcome label ([`agent::record_execution_end`]) —
/// best-effort, exactly as the worker's closeout always was (the inbox
/// notification and the lane's own events are the user-facing signal).
///
/// The signature is the contract (#1708): no session id comes in, so the
/// closeout **cannot** address the session-keyed `tasks` rows at all. See
/// the module docs for why that absence is deliberate.
pub(crate) fn close_worker_execution(
    execution: Option<&(Arc<Store>, i64)>,
    registry: &ToolRegistry,
    files_before: usize,
    outcome_label: &str,
    cost_usd: f64,
    persistence_complete: bool,
) {
    let Some((store, id)) = execution else {
        return;
    };
    let _ = agent::record_execution_end(
        store,
        *id,
        registry,
        files_before,
        outcome_label,
        cost_usd,
        persistence_complete,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{TaskItem, TaskStatus};

    fn lead_row(id: &str, subject: &str) -> TaskItem {
        TaskItem {
            id: id.into(),
            subject: subject.into(),
            description: None,
            status: TaskStatus::Pending,
            owner: None,
        }
    }

    /// A store holding the LEAD session's board mirror, plus a worker whose
    /// own private board is populated — the exact state `run_worker` closes
    /// out from after a delegated run that used `task_create`.
    fn delegation_fixture() -> (Arc<Store>, i64, ToolRegistry, tempfile::TempDir) {
        let root = tempfile::tempdir().expect("root");
        let store = Arc::new(Store::in_memory().expect("store"));
        let lead_exec = store
            .begin_execution("deck", "the lead's goal", "anthropic", "claude")
            .expect("lead execution");
        store
            .record_task_board(
                lead_exec,
                Some("ses"),
                &[
                    lead_row("1", "ship the parser"),
                    lead_row("2", "write the release notes"),
                ],
                1,
            )
            .expect("lead mirror");
        let worker_exec = store
            .begin_execution("deck-sub", "delegated work", "anthropic", "claude")
            .expect("worker execution");
        let registry = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);
        registry
            .task_board()
            .lock()
            .unwrap()
            .create("read the grammar", None);
        (store, worker_exec, registry, root)
    }

    /// **Witness for #1708, defect one.** A worker's private board numbers
    /// from "1" in its own namespace, so persisting it under the lead's
    /// session id upserted the worker's task "1" over the lead's unrelated
    /// task "1" (`UNIQUE(session_id, task_id)`). The closeout must leave the
    /// session's mirror byte-identical: same rows, same subjects, and no
    /// appended strays (the row COUNT is asserted too, because a write that
    /// switched to a NULL session id would pass the per-session read while
    /// still growing the table).
    #[test]
    fn a_workers_private_board_never_lands_in_the_sessions_tasks_rows() {
        let (store, worker_exec, registry, _root) = delegation_fixture();

        close_worker_execution(
            Some(&(store.clone(), worker_exec)),
            &registry,
            0,
            "completed",
            0.0,
            true,
        );

        let rows = store.list_session_tasks("ses").expect("read back");
        assert_eq!(
            rows.iter()
                .map(|t| (t.id.as_str(), t.subject.as_str()))
                .collect::<Vec<_>>(),
            vec![("1", "ship the parser"), ("2", "write the release notes")],
            "the lead's mirror survives a worker closeout untouched"
        );
        assert_eq!(
            store.count("tasks").expect("count"),
            2,
            "no worker row lands anywhere in the table, session-keyed or not"
        );
        assert!(
            !store
                .unfinished_executions()
                .expect("unfinished")
                .contains(&worker_exec),
            "the closeout still closes: the worker's execution row is finished"
        );
    }

    /// **Witness for #1708, defect two.** `/clear` deletes the session's
    /// `tasks` rows (#1692), and its worker seal lives on the driver — a
    /// thread this closeout runs nowhere near. The only way a pre-clear
    /// worker cannot repopulate the destroyed mirror is for its closeout to
    /// write no board at all: after the clear's delete, a worker closeout
    /// must leave the session's mirror exactly as empty as the user made it.
    #[test]
    fn a_pre_clear_workers_closeout_cannot_repopulate_a_cleared_mirror() {
        let (store, worker_exec, registry, _root) = delegation_fixture();
        store.clear_session_tasks("ses").expect("the /clear delete");

        close_worker_execution(
            Some(&(store.clone(), worker_exec)),
            &registry,
            0,
            "completed",
            0.0,
            true,
        );

        assert!(
            store
                .list_session_tasks("ses")
                .expect("read back")
                .is_empty(),
            "a board the user destroyed stays destroyed"
        );
        assert_eq!(
            store.count("tasks").expect("count"),
            0,
            "and no session-less stray survives either"
        );
    }
}
