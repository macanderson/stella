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

use stella_protocol::{TaskItem, TaskStatus};

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
    pub fn create(&mut self, subject: impl Into<String>, description: Option<String>) -> &TaskItem {
        let id = (self.items.len() + 1).to_string();
        self.items.push(TaskItem {
            id,
            subject: subject.into(),
            description,
            status: TaskStatus::Pending,
            owner: None,
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
            self.create(step.as_ref(), None);
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
        board.create("one", None);
        board.create("two", None);
        board.set_status("1", TaskStatus::Cancelled).unwrap();
        board.create("three", None);
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
        board.create("one", None);
        board.create("two", None);
        board.set_status("1", TaskStatus::Completed).unwrap();
        board.assign("2", "sub:2").unwrap();

        board.clear();

        assert!(board.is_empty());
        assert!(board.items().is_empty());
        let fresh = board.create("a brand new plan", None);
        assert_eq!(
            fresh.id, "1",
            "ids restart — the cleared board is a new one"
        );
        assert_eq!(fresh.status, TaskStatus::Pending);
        // And seeding works again, which it would not if `clear` had merely
        // hidden the rows: `seed_from_plan` no-ops on a non-empty board.
        let mut seeded = TaskBoard::new();
        seeded.create("stale", None);
        seeded.clear();
        assert!(seeded.seed_from_plan(&["step one", "step two"]));
        assert_eq!(seeded.items().len(), 2);
    }

    #[test]
    fn terminal_tasks_reject_every_transition() {
        let mut board = TaskBoard::new();
        board.create("t", None);
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
        board.create("already here", None);
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
        board.create("abandoned", None);
        board.create("the real work", None);
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
        board.create("mine", None);
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
            board.create(subject, None);
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
        board.create("open", None);
        board.create("done", None);
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
        board.create("t", None);
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
                        Op::Create(s) => { board.create(s.clone(), None); }
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
            board.create("pinned", None);
            board.set_status("1", TaskStatus::Cancelled).unwrap();
            let frozen = board.items()[0].clone();
            for op in &ops {
                match op {
                    Op::Create(s) => { board.create(s.clone(), None); }
                    Op::SetStatus(i, s) => { let _ = board.set_status(&(i + 1).to_string(), *s); }
                    Op::Assign(i, o) => { let _ = board.assign(&(i + 1).to_string(), o.clone()); }
                }
            }
            prop_assert_eq!(&board.items()[0], &frozen);
        }
    }
}
