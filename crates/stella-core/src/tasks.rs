//! The session task board — pure decision logic for the `task_*` tools
//! (`task_create` / `task_list` / `task_complete` / `task_cancel` /
//! `task_assign`).
//!
//! The board is owned data with no I/O: the tools crate mutates it through
//! these methods, the CLI tap snapshots it into `AgentEvent::TaskUpdate`
//! events (the sole render path — the TUI folds snapshots, never reads this
//! struct), and the store mirrors snapshots for cross-session findability.
//! Keeping the transition rules here makes them property-testable without a
//! registry or a runtime.
//!
//! `task_assign` does not spawn anything itself — spawning is I/O. It
//! validates the transition and records a [`SpawnRequest`]; the session
//! driver drains them (`stella-tools`' `ToolRegistry::take_spawn_requests`)
//! and runs each on its own deck sub-session lane.

use stella_protocol::{Closure, TaskContract, TaskItem, TaskStatus};

/// Why a board mutation was rejected. Named errors, never a bare string —
/// the tools surface these verbatim to the model so it can self-correct.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TaskBoardError {
    #[error("no task with id {id} — call task_list to see the board")]
    UnknownTask { id: String },
    #[error("task {id} is already {status:?} — terminal tasks cannot change state")]
    Terminal { id: String, status: TaskStatus },
    #[error(
        "task {open_id} `{open_subject}` is still in_progress — you hold exactly one task at a \
         time. Finish it with task_complete if its work is done; if you are changing order and \
         it is not, task_cancel it with the reason (the row stays as an audit trail, and \
         task_create can put the step back later — ids are never reused). Only then start \
         task {id}. Do not complete a task whose work is unfinished."
    )]
    AnotherTaskInProgress {
        id: String,
        open_id: String,
        open_subject: String,
    },
    #[error(
        "task {id} cannot be completed: its definition of done has {pending} check(s) not yet \
         run and {failed} that did not pass — {outstanding}. Run them and record the outcome; a \
         task closes when its checks pass, not when it is reported done. If a check turns out to \
         be the wrong check, change the contract deliberately — do not cancel the task to get \
         past this."
    )]
    ContractUnsatisfied {
        id: String,
        pending: usize,
        failed: usize,
        /// The outstanding clauses, quoted, so the refusal names the work
        /// rather than only its count.
        outstanding: String,
    },
}

/// One queued request to spawn a dedicated sub-agent for a task
/// (`task_assign`). Pure data; the driver owns the actual spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnRequest {
    /// The board task this sub-agent works on.
    pub task_id: String,
    pub subject: String,
    pub description: Option<String>,
    /// What the assigning agent wants the sub-agent to know — carried into
    /// the sub-agent's prompt verbatim (the "communication" of
    /// `task_assign`).
    pub briefing: String,
}

/// The task board: an insertion-ordered list of [`TaskItem`]s with ordinal
/// string ids ("1", "2", …). All mutation goes through the methods below so
/// the transition rules hold by construction.
#[derive(Debug, Clone, Default)]
pub struct TaskBoard {
    items: Vec<TaskItem>,
}

impl TaskBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current board, oldest first — what `TaskUpdate` snapshots carry.
    pub fn items(&self) -> &[TaskItem] {
        &self.items
    }

    /// A task's contract, mutably — how a check runner records an outcome.
    ///
    /// Scoped to the contract rather than handing out the whole item (or the
    /// whole board) because recording an outcome is the only mutation anything
    /// outside this module needs, and `status` in particular must stay
    /// reachable only through [`Self::set_status`] — that is where the refusal
    /// lives, and an escape hatch beside it would be a way around it.
    ///
    /// `Ok(None)` is a task that declared no contract; the error is an unknown
    /// id.
    pub fn contract_mut(&mut self, id: &str) -> Result<Option<&mut TaskContract>, TaskBoardError> {
        Ok(Self::find(&mut self.items, id)?.contract.as_mut())
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Empty the board — `/clear`'s half of a session destroy-and-reset
    /// (#1692). Ids restart at "1" with the next [`Self::create`], which is
    /// the point: the cleared session is a new session as far as the board
    /// is concerned, and its plan is numbered from the top like any other.
    ///
    /// That id reuse is also why a clear cannot simply be "wipe and carry
    /// on". A worker that was already running for pre-clear task "1" can
    /// still report in afterwards, and its task id would now address a
    /// *different* task. Rejecting those late reports is the caller's job —
    /// see `stella-cli`'s `command_deck::session_clear`, which seals the
    /// board against every worker generation that predates the clear.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Create a task in `Pending`; returns the new item. Ids are ordinal and
    /// never reused — cancelling task "2" does not renumber task "3", so ids
    /// in the transcript stay valid for the whole session.
    /// `contract` is what the task means by done (SPEC 7.1). `None` records
    /// that nobody has said yet — deliberately not the same fact as
    /// [`TaskContract::ReadOnly`], which records that someone looked and found
    /// nothing to prove. An undeclared task is created rather than refused, and
    /// pays for it at [`Self::set_status`]: it can be completed, because
    /// refusing every legacy task would break every board that predates
    /// contracts, while a *declared* one closes only on its checks.
    pub fn create(
        &mut self,
        subject: impl Into<String>,
        description: Option<String>,
        contract: Option<TaskContract>,
    ) -> &TaskItem {
        let id = (self.items.len() + 1).to_string();
        self.items.push(TaskItem {
            id,
            subject: subject.into(),
            description,
            status: TaskStatus::Pending,
            owner: None,
            contract,
        });
        self.items.last().expect("just pushed")
    }

    /// Seed the board from an approved plan's steps — one `Pending` task per
    /// step, in plan order, so the ordinal ids the model addresses (`"3"`) are
    /// the ordinals the user approved.
    ///
    /// # Why this exists
    ///
    /// Without it the two halves of the same idea never met. The user approved
    /// a list of steps; the board those steps' progress would be recorded
    /// against started empty, so `task_start "3"` answered `UnknownTask` and
    /// the only way to get a populated board was for the model to *re-type the
    /// plan* into `task_create`. It reliably did not, and the deck's checklist
    /// was permanently empty as a result.
    ///
    /// A no-op on a board that already has rows: seeding is how a plan becomes
    /// a board, never a way to overwrite work already in flight. Returns
    /// whether anything was seeded.
    pub fn seed_from_plan<S: AsRef<str>>(&mut self, steps: &[S]) -> bool {
        if !self.items.is_empty() || steps.is_empty() {
            return false;
        }
        for step in steps {
            self.create(step.as_ref(), None, None);
        }
        true
    }

    /// Move a task to a new status. Terminal tasks (completed / cancelled)
    /// reject every transition; re-asserting the same open status is a no-op
    /// that succeeds (idempotent `task_complete` retries stay terminal-safe
    /// because terminality is checked first).
    ///
    /// # The lead holds one task at a time
    ///
    /// A move *into* `InProgress` is refused while another **unowned** task is
    /// already in progress: that is the agent's own lane, and it is
    /// single-occupancy. `task_start`'s and `task_complete`'s descriptions have
    /// promised this since they were written ("keep exactly ONE task
    /// in_progress at a time: complete the current task before starting the
    /// next") and nothing enforced it, so a board could — and did — sit with a
    /// step marked in progress that the agent had long since walked past, while
    /// it edited files for a later step. An advisory board is a board that
    /// misreports which step the work is on, which is the one thing it exists
    /// to report.
    ///
    /// Refusal, not silent auto-completion: closing a card the agent never said
    /// was finished would claim work was done on the board's own authority. The
    /// error names the open task and every way out, so the next call can be the
    /// `task_complete` that should have come first.
    ///
    /// All three ways, deliberately. A refusal offering only "finish it" and
    /// "drop it" is a trap for the agent that is merely re-ordering — it starts
    /// a step, learns a later one must land first, and finds that the only exit
    /// phrased as *keeping* the step is a `task_complete` that would be a lie.
    /// So the message also names the honest reorder path (`task_cancel` with the
    /// reason, which keeps the row as an audit trail, then `task_create` when the
    /// step is reachable — ids are never reused, so nothing is corrupted) and
    /// says outright not to complete unfinished work. A rule that makes the
    /// board honest must not buy that by making the agent dishonest.
    ///
    /// Only *unowned* tasks occupy the lane. [`Self::assign`] deliberately does
    /// not route through here: delegated tasks carry an owner and run in
    /// parallel by design, so N sub-agent lanes plus the lead's own task is a
    /// legal board, not a violation.
    pub fn set_status(
        &mut self,
        id: &str,
        status: TaskStatus,
    ) -> Result<&TaskItem, TaskBoardError> {
        // Scanned before the mutable borrow, but reported *after* the
        // unknown/terminal check below: a task that can never start should be
        // told why it can never start, not which lane it happened to collide
        // with.
        let occupant = (status == TaskStatus::InProgress)
            .then(|| {
                self.items
                    .iter()
                    .find(|t| t.id != id && t.owner.is_none() && t.status == TaskStatus::InProgress)
            })
            .flatten()
            .map(|t| (t.id.clone(), t.subject.clone()));
        let item = Self::find(&mut self.items, id)?;
        if let Some((open_id, open_subject)) = occupant {
            return Err(TaskBoardError::AnotherTaskInProgress {
                id: id.to_string(),
                open_id,
                open_subject,
            });
        }
        // A task closes when its checks pass (SPEC 7.1, and SPEC 1's second
        // thesis). This is the enforcement point `TaskStatus`'s own doc has
        // always pointed at: `set_status` is the single transition gate, so a
        // refusal here cannot be routed around by a caller that reaches for a
        // different setter — there is no different setter.
        //
        // Only `Completed` is gated. `Cancelled` is not a claim that the work
        // was done and must stay available precisely when the checks are
        // failing, or the board would trap a task nobody can honestly close.
        if status == TaskStatus::Completed
            && let Some(contract) = &item.contract
            && let Closure::Outstanding { pending, failed } = contract.closure()
        {
            let outstanding = contract
                .checks()
                .filter(|c| !c.passed())
                .map(|c| format!("`{}`", c.statement))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(TaskBoardError::ContractUnsatisfied {
                id: id.to_string(),
                pending,
                failed,
                outstanding,
            });
        }
        item.status = status;
        Ok(item)
    }

    /// Record an owner lane for a task and mark it in progress — the board
    /// half of `task_assign` (the spawn half is the driver's, via
    /// [`SpawnRequest`]).
    pub fn assign(
        &mut self,
        id: &str,
        owner: impl Into<String>,
    ) -> Result<&TaskItem, TaskBoardError> {
        let item = Self::find(&mut self.items, id)?;
        item.owner = Some(owner.into());
        item.status = TaskStatus::InProgress;
        Ok(item)
    }

    fn find<'a>(items: &'a mut [TaskItem], id: &str) -> Result<&'a mut TaskItem, TaskBoardError> {
        let item = items
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| TaskBoardError::UnknownTask { id: id.to_string() })?;
        if !item.status.is_open() {
            return Err(TaskBoardError::Terminal {
                id: item.id.clone(),
                status: item.status,
            });
        }
        Ok(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn ids_are_ordinal_and_survive_cancellation() {
        let mut board = TaskBoard::new();
        board.create("one", None, None);
        board.create("two", None, None);
        board.set_status("1", TaskStatus::Cancelled).unwrap();
        board.create("three", None, None);
        let ids: Vec<&str> = board.items().iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["1", "2", "3"]);
        assert_eq!(board.items()[2].subject, "three");
    }

    /// `/clear` takes the whole board, terminal rows included, and the next
    /// plan is numbered from the top. The id restart is asserted rather than
    /// merely observed because it is what makes a pre-clear worker's task id
    /// dangerous — see the callers in `stella-cli`'s `session_clear` (#1692).
    #[test]
    fn clear_empties_the_board_and_restarts_the_ids() {
        let mut board = TaskBoard::new();
        board.create("one", None, None);
        board.create("two", None, None);
        board.set_status("1", TaskStatus::Completed).unwrap();
        board.assign("2", "sub:2").unwrap();

        board.clear();

        assert!(board.is_empty());
        assert!(board.items().is_empty());
        let fresh = board.create("a brand new plan", None, None);
        assert_eq!(
            fresh.id, "1",
            "ids restart — the cleared board is a new one"
        );
        assert_eq!(fresh.status, TaskStatus::Pending);
        // And seeding works again, which it would not if `clear` had merely
        // hidden the rows: `seed_from_plan` no-ops on a non-empty board.
        let mut seeded = TaskBoard::new();
        seeded.create("stale", None, None);
        seeded.clear();
        assert!(seeded.seed_from_plan(&["step one", "step two"]));
        assert_eq!(seeded.items().len(), 2);
    }

    #[test]
    fn terminal_tasks_reject_every_transition() {
        let mut board = TaskBoard::new();
        board.create("t", None, None);
        board.set_status("1", TaskStatus::Completed).unwrap();
        for attempt in [
            board.set_status("1", TaskStatus::Pending).unwrap_err(),
            board.set_status("1", TaskStatus::InProgress).unwrap_err(),
            board.assign("1", "sub:1").unwrap_err(),
        ] {
            assert_eq!(
                attempt,
                TaskBoardError::Terminal {
                    id: "1".into(),
                    status: TaskStatus::Completed
                }
            );
        }
    }

    /// The join that makes the plan and the board one system: seeding numbers
    /// the steps exactly as the approval gate did, so `task_start "2"` moves
    /// the step the user read as 2.
    #[test]
    fn seeding_numbers_steps_as_the_plan_did() {
        let mut board = TaskBoard::new();
        assert!(board.seed_from_plan(&["read the layout", "fold the rail", "test it"]));
        let ids: Vec<&str> = board.items().iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["1", "2", "3"]);
        assert_eq!(board.items()[1].subject, "fold the rail");
        assert!(
            board
                .items()
                .iter()
                .all(|t| t.status == TaskStatus::Pending)
        );
        board.set_status("2", TaskStatus::InProgress).unwrap();
        assert_eq!(board.items()[1].status, TaskStatus::InProgress);
    }

    /// Seeding is how a plan becomes a board, never a way to overwrite work
    /// already in flight.
    #[test]
    fn seeding_never_disturbs_a_board_that_has_rows() {
        let mut board = TaskBoard::new();
        board.create("already here", None, None);
        board.set_status("1", TaskStatus::Completed).unwrap();
        assert!(!board.seed_from_plan(&["a", "b"]));
        assert_eq!(board.items().len(), 1);
        assert_eq!(board.items()[0].subject, "already here");
    }

    #[test]
    fn seeding_an_empty_plan_does_nothing() {
        let mut board = TaskBoard::new();
        assert!(!board.seed_from_plan::<&str>(&[]));
        assert!(board.is_empty());
    }

    #[test]
    fn unknown_ids_name_the_id_in_the_error() {
        let mut board = TaskBoard::new();
        assert_eq!(
            board.set_status("7", TaskStatus::Completed).unwrap_err(),
            TaskBoardError::UnknownTask { id: "7".into() }
        );
    }

    /// Witness for the defect: a plan whose step 1 is never checked off.
    ///
    /// A three-step plan is seeded, step 1 is started, and the agent walks on
    /// to step 2 without completing step 1. On main both starts succeeded and
    /// the board carried two live steps — so the panel kept pointing at
    /// "investigate" while the edits belonged to "implement". The refusal names
    /// the step that has to be closed and both ways to close it.
    #[test]
    fn a_second_task_cannot_start_while_the_first_is_still_open() {
        let mut board = TaskBoard::new();
        assert!(board.seed_from_plan(&[
            "Investigate chunk embedding pass",
            "Implement cross-file batching",
            "Add tests",
        ]));
        board.set_status("1", TaskStatus::InProgress).unwrap();

        let refusal = board.set_status("2", TaskStatus::InProgress).unwrap_err();
        assert_eq!(
            refusal,
            TaskBoardError::AnotherTaskInProgress {
                id: "2".into(),
                open_id: "1".into(),
                open_subject: "Investigate chunk embedding pass".into(),
            }
        );
        let rendered = refusal.to_string();
        // All three ways out, not just the two that are easy to reach for. A
        // refusal offering only "complete it" and "drop it" pushes an agent
        // that is merely re-ordering toward a false task_complete — the exact
        // dishonesty this rule exists to prevent — so the reorder path
        // (cancel with a reason, re-create the step later) is named too, along
        // with the guard against taking the lying exit.
        for remedy in ["task_complete", "task_cancel", "task_create"] {
            assert!(
                rendered.contains(remedy),
                "the refusal must name the way out: {rendered}"
            );
        }
        assert!(
            rendered.contains("Do not complete a task whose work is unfinished"),
            "the refusal must not read as an invitation to lie: {rendered}"
        );

        // The board is untouched by a refused start — no second live step.
        assert_eq!(board.items()[0].status, TaskStatus::InProgress);
        assert_eq!(board.items()[1].status, TaskStatus::Pending);

        // And closing step 1 is what unblocks step 2, which is the whole point.
        board.set_status("1", TaskStatus::Completed).unwrap();
        board.set_status("2", TaskStatus::InProgress).unwrap();
        assert_eq!(board.items()[1].status, TaskStatus::InProgress);
    }

    /// Cancelling is the other way out, and a completed/cancelled step never
    /// occupies the lane it has left.
    #[test]
    fn cancelling_the_open_task_also_frees_the_lane() {
        let mut board = TaskBoard::new();
        board.create("abandoned", None, None);
        board.create("the real work", None, None);
        board.set_status("1", TaskStatus::InProgress).unwrap();
        board.set_status("1", TaskStatus::Cancelled).unwrap();
        board.set_status("2", TaskStatus::InProgress).unwrap();
        assert_eq!(board.items()[1].status, TaskStatus::InProgress);
    }

    /// Re-asserting in_progress on the task you already hold stays the
    /// idempotent no-op it always was — the occupant check excludes the target
    /// itself, so a repeated `task_start` is not a self-collision.
    #[test]
    fn restarting_the_task_you_already_hold_is_not_a_collision() {
        let mut board = TaskBoard::new();
        board.create("mine", None, None);
        board.set_status("1", TaskStatus::InProgress).unwrap();
        board.set_status("1", TaskStatus::InProgress).unwrap();
        assert_eq!(board.items()[0].status, TaskStatus::InProgress);
    }

    /// Delegation is parallel by design: sub-agent lanes carry an owner and do
    /// not occupy the lead's. Three workers running plus the lead's own step is
    /// a legal board — the rule is one task *of yours*, not one task on the
    /// board.
    #[test]
    fn delegated_lanes_do_not_occupy_the_leads_lane() {
        let mut board = TaskBoard::new();
        for subject in [
            "port the parser",
            "port the lexer",
            "port the printer",
            "review it all",
        ] {
            board.create(subject, None, None);
        }
        board.assign("1", "sub:1").unwrap();
        board.assign("2", "sub:2").unwrap();
        board.assign("3", "sub:3").unwrap();

        board
            .set_status("4", TaskStatus::InProgress)
            .expect("the lead may work while sub-agents run");
        assert_eq!(board.items()[3].status, TaskStatus::InProgress);
    }

    /// Terminality outranks the lane: a task that can never start is told why
    /// it can never start, not which lane it collided with.
    #[test]
    fn a_terminal_target_reports_terminality_not_the_open_lane() {
        let mut board = TaskBoard::new();
        board.create("open", None, None);
        board.create("done", None, None);
        board.set_status("1", TaskStatus::InProgress).unwrap();
        board.set_status("2", TaskStatus::Completed).unwrap();
        assert_eq!(
            board.set_status("2", TaskStatus::InProgress).unwrap_err(),
            TaskBoardError::Terminal {
                id: "2".into(),
                status: TaskStatus::Completed
            }
        );
    }

    #[test]
    fn assign_records_owner_and_marks_in_progress() {
        let mut board = TaskBoard::new();
        board.create("t", None, None);
        let item = board.assign("1", "sub:1").unwrap();
        assert_eq!(item.owner.as_deref(), Some("sub:1"));
        assert_eq!(item.status, TaskStatus::InProgress);
    }

    /// One random board operation, for the fold property below.
    #[derive(Debug, Clone)]
    enum Op {
        Create(String),
        SetStatus(usize, TaskStatus),
        Assign(usize, String),
    }

    fn arb_op() -> impl Strategy<Value = Op> {
        prop_oneof![
            "[a-z]{1,8}".prop_map(Op::Create),
            (0usize..12, arb_status()).prop_map(|(i, s)| Op::SetStatus(i, s)),
            (0usize..12, "[a-z]{1,6}").prop_map(|(i, o)| Op::Assign(i, o)),
        ]
    }

    fn arb_status() -> impl Strategy<Value = TaskStatus> {
        prop_oneof![
            Just(TaskStatus::Pending),
            Just(TaskStatus::InProgress),
            Just(TaskStatus::Completed),
            Just(TaskStatus::Cancelled),
        ]
    }

    proptest! {
        /// Replaying the same op sequence on a fresh board yields an
        /// identical board — the board is a deterministic fold, which is
        /// what lets `TaskUpdate` snapshots reconstruct it anywhere.
        #[test]
        fn board_is_a_deterministic_fold(ops in proptest::collection::vec(arb_op(), 0..40)) {
            let mut a = TaskBoard::new();
            let mut b = TaskBoard::new();
            for board in [&mut a, &mut b] {
                for op in &ops {
                    match op {
                        Op::Create(s) => { board.create(s.clone(), None, None); }
                        Op::SetStatus(i, s) => { let _ = board.set_status(&(i + 1).to_string(), *s); }
                        Op::Assign(i, o) => { let _ = board.assign(&(i + 1).to_string(), o.clone()); }
                    }
                }
            }
            prop_assert_eq!(a.items(), b.items());
        }

        /// Terminality is absorbing: once a task leaves the open states, no
        /// op sequence can ever change it again.
        #[test]
        fn terminal_states_are_absorbing(ops in proptest::collection::vec(arb_op(), 0..40)) {
            let mut board = TaskBoard::new();
            board.create("pinned", None, None);
            board.set_status("1", TaskStatus::Cancelled).unwrap();
            let frozen = board.items()[0].clone();
            for op in &ops {
                match op {
                    Op::Create(s) => { board.create(s.clone(), None, None); }
                    Op::SetStatus(i, s) => { let _ = board.set_status(&(i + 1).to_string(), *s); }
                    Op::Assign(i, o) => { let _ = board.assign(&(i + 1).to_string(), o.clone()); }
                }
            }
            prop_assert_eq!(&board.items()[0], &frozen);
        }
    }

    // ── SPEC 7.1: a task closes on its checks, not on being reported done ────

    fn contracted(statement: &str) -> TaskContract {
        TaskContract::DefinitionOfDone(stella_protocol::DefinitionOfDone::new(
            stella_protocol::Check::new(
                statement,
                stella_protocol::CheckMechanism::Known(stella_protocol::CheckKind::Unit),
            ),
            Vec::new(),
        ))
    }

    /// The witness. On the old board this was a plain `Ok` — `task_complete`
    /// set the field and the task was done because something said so.
    #[test]
    fn a_contracted_task_is_refused_a_close_while_a_check_is_outstanding() {
        let mut board = TaskBoard::default();
        board.create(
            "wire the dedup digest",
            None,
            Some(contracted("the dedup suite is green")),
        );
        let err = board
            .set_status("1", TaskStatus::Completed)
            .expect_err("an unsatisfied contract must refuse the close");
        assert!(
            matches!(
                err,
                TaskBoardError::ContractUnsatisfied {
                    pending: 1,
                    failed: 0,
                    ..
                }
            ),
            "{err:?}"
        );
        // The refusal names the work, not just its count — a reader who cannot
        // tell *which* check is outstanding cannot act on the message.
        assert!(
            err.to_string().contains("the dedup suite is green"),
            "{err}"
        );
        assert_eq!(board.items()[0].status, TaskStatus::Pending);
    }

    #[test]
    fn a_contracted_task_closes_once_its_checks_pass() {
        let mut board = TaskBoard::default();
        board.create(
            "wire the dedup digest",
            None,
            Some(contracted("the dedup suite is green")),
        );
        if let Some(TaskContract::DefinitionOfDone(dod)) =
            board.contract_mut("1").expect("known task")
        {
            for check in dod.iter_mut() {
                check.outcome = stella_protocol::CheckOutcome::Passed {
                    evidence: "12 tests, 0 failures".into(),
                };
            }
        }
        board
            .set_status("1", TaskStatus::Completed)
            .expect("a satisfied contract permits the close");
        assert_eq!(board.items()[0].status, TaskStatus::Completed);
    }

    /// A read-only task has nothing to prove and must not be trapped by a gate
    /// meant for tasks that produce diffs.
    #[test]
    fn a_read_only_task_still_closes() {
        let mut board = TaskBoard::default();
        board.create("read the retry policy", None, Some(TaskContract::ReadOnly));
        board
            .set_status("1", TaskStatus::Completed)
            .expect("a read-only task closes on its events");
    }

    /// Every board that predates contracts keeps working: `None` means nobody
    /// said, and the gate has nothing to enforce.
    #[test]
    fn an_undeclared_task_closes_as_it_always_did() {
        let mut board = TaskBoard::default();
        board.create("legacy task", None, None);
        board
            .set_status("1", TaskStatus::Completed)
            .expect("an undeclared task is not gated");
    }

    /// Cancelling must stay available precisely when the checks are failing, or
    /// a task nobody can honestly close would be stuck on the board forever —
    /// and the pressure would be to fake a passing check.
    #[test]
    fn a_failing_contract_can_still_be_cancelled() {
        let mut board = TaskBoard::default();
        board.create(
            "wire the dedup digest",
            None,
            Some(contracted("the suite is green")),
        );
        board
            .set_status("1", TaskStatus::Cancelled)
            .expect("cancel is not a claim that the work was done");
    }
}
