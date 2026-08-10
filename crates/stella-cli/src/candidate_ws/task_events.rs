//! The candidate stack's task-board seam (#1719) — the board half of the
//! sweep that gave isolated candidates `attach_read_events` for the Files
//! tab. A candidate's registry builds its own empty [`TaskBoard`] with no
//! path to any surface, so before this module existed an isolated run's
//! `task_start "3"` answered `UnknownTask` for a step the scope gate had
//! already numbered 3, and even a successful mutation reached nobody: the
//! deck's PLAN rail froze at hollow rings for the whole turn.
//!
//! Two halves, matching the two failures:
//!
//! - **Seeding.** [`CandidateTaskBoard::seed`] is driven by the pipeline
//!   through `CandidateWorkspace::seed_task_board`, right after `create` —
//!   the pipeline owns the approved plan, so the plan reaches every
//!   candidate's private board with the gate's own ordinals and titles.
//!   Boards stay per-candidate: a step is completable exactly once per
//!   board, so siblings sharing one would hand the second finisher a
//!   terminal-state tool error for work it really did.
//! - **Announcing.** [`CandidateTaskTap`] mirrors the deck's session
//!   `TaskTap` (`crate::command_deck::task_tap`): after any `task_*` call
//!   the full board snapshot rides the turn's channel as
//!   `AgentEvent::TaskUpdate`, persisted by the forwarder so replay shows
//!   the checklist as it moved. Unlike the session tap there is no
//!   supervisor half — candidates run non-interactively and v1 delegation
//!   spawns from the lead only — and announcing waits on the latch the
//!   pipeline arms only for a lone candidate: a `TaskUpdate` carries no
//!   candidate tag, so several boards reporting onto one channel would
//!   splice into a checklist that is nobody's.
//!
//! [`TaskBoard`]: stella_core::tasks::TaskBoard

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ports::ToolExecutor;
use stella_protocol::{AgentEvent, TaskItem, ToolOutput, ToolSchema};
use stella_tools::tasks::TaskBoardHandle;

/// The workspace's handle on its own board: what
/// `GitCandidateWorkspace::seed_task_board` drives when the pipeline calls
/// it. Split from the tap because the two outlive different owners — the tap
/// moves into the boxed tool stack, while this stays a plain field on the
/// workspace struct.
pub(super) struct CandidateTaskBoard {
    board: TaskBoardHandle,
    /// Shared with the tap. Starts disarmed, so a workspace whose host never
    /// calls `seed_task_board` (a fresh port consumer, or the witness
    /// author's pristine snapshot, which the pipeline deliberately does not
    /// seed) announces nothing.
    announce: Arc<AtomicBool>,
}

impl CandidateTaskBoard {
    /// Seed the board from the approved plan and set the announce latch —
    /// the two settle together, before any dispatch, so the first `task_*`
    /// call already sees both. An empty `steps` still arms `announce`: a run
    /// that planned nothing lets the model build its own rows, and a lone
    /// candidate's rows deserve the rail exactly as the session's do.
    pub(super) fn seed(&self, steps: &[String], announce: bool) {
        {
            let mut guard = self.board.lock().unwrap_or_else(|p| p.into_inner());
            guard.seed_from_plan(steps);
        }
        self.announce.store(announce, Ordering::Relaxed);
    }
}

/// Wrap a candidate's complete tool surface in the task tap and hand back
/// the seeding seam. `events: None` (a host that wired no event sink) skips
/// the tap entirely — seeding still works, because a resolvable board fixes
/// the worker's tool errors whether or not anything is listening.
pub(super) fn tap(
    tools: Box<dyn ToolExecutor>,
    board: TaskBoardHandle,
    events: Option<stella_core::EventSender>,
) -> (Box<dyn ToolExecutor>, CandidateTaskBoard) {
    let announce = Arc::new(AtomicBool::new(false));
    let seam = CandidateTaskBoard {
        board: board.clone(),
        announce: announce.clone(),
    };
    let tools = match events {
        Some(events) => Box::new(CandidateTaskTap {
            inner: tools,
            events,
            board,
            announce,
        }),
        None => tools,
    };
    (tools, seam)
}

/// The decorator: outermost over registry + customs + candidate MCP +
/// policy, like the deck's session tap, so it observes every route a
/// `task_*` call can take. It snapshots after errors too — a failed call
/// changed nothing, and re-asserting the unchanged board is cheaper than
/// guessing which failures mutated.
struct CandidateTaskTap {
    inner: Box<dyn ToolExecutor>,
    events: stella_core::EventSender,
    board: TaskBoardHandle,
    announce: Arc<AtomicBool>,
}

#[async_trait]
impl ToolExecutor for CandidateTaskTap {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.inner.schemas()
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        let output = self.inner.execute(name, input).await;
        if name.starts_with("task_") && self.announce.load(Ordering::Relaxed) {
            let tasks: Vec<TaskItem> = {
                let guard = self.board.lock().unwrap_or_else(|p| p.into_inner());
                guard.items().to_vec()
            };
            let _ = self.events.send(AgentEvent::TaskUpdate { tasks });
        }
        output
    }

    /// Forwarded: a decorator that let the default `0.0` stand would
    /// silently drop sub-agent spend out of the parent's budget (see the
    /// port's contract).
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.drain_sub_agent_spend_usd()
    }

    /// Forwarded for the same reason: a swallowed wait request silently
    /// turns parked waits (#1471) back into model-step polling.
    fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
        self.inner.drain_wait_request()
    }

    /// Forwarded: the empty default would silently serialize the inner
    /// executor's sibling spawns (see the port's contract).
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        self.inner.parallel_safe_names()
    }
}

#[cfg(test)]
mod tests {
    use super::super::GitCandidateWorkspaces;
    use super::super::tests::scaffold;
    use stella_pipeline::ports::CandidateWorkspace;
    use stella_protocol::{AgentEvent, TaskStatus};
    use stella_tools::RegistryOptions;

    fn port_with_events(
        root: std::path::PathBuf,
    ) -> (
        GitCandidateWorkspaces,
        tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let port = GitCandidateWorkspaces::new(
            root,
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        )
        .with_events(stella_core::EventSender::new(tx));
        (port, rx)
    }

    /// Drain every `TaskUpdate` snapshot currently in the channel.
    fn boards(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    ) -> Vec<Vec<stella_protocol::TaskItem>> {
        let mut boards = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::TaskUpdate { tasks } = event {
                boards.push(tasks);
            }
        }
        boards
    }

    /// #1719's witness: on `main`, a candidate's board was private, unseeded,
    /// and unannounced — `task_start "1"` answered `UnknownTask` for the step
    /// the gate numbered 1, and zero `TaskUpdate` events reached the channel
    /// (execution 66's shape: 1168 events, none of them `task_update`).
    #[tokio::test]
    async fn a_seeded_lone_candidate_moves_the_approved_step_and_announces_it() {
        let root = scaffold("taskseed");
        let (port, mut rx) = port_with_events(root.clone());
        let ws = port.create_workspace().await.unwrap();
        ws.seed_task_board(&["find the defect".into(), "fix it".into()], true);

        let out = ws
            .tools()
            .execute("task_start", &serde_json::json!({"id": "1"}))
            .await;
        assert!(
            !out.is_error(),
            "the gate-numbered step must resolve on the candidate board: {out:?}"
        );

        let snapshot = boards(&mut rx)
            .pop()
            .expect("a lone candidate's task_start must announce a TaskUpdate");
        assert_eq!(
            snapshot[0].subject, "find the defect",
            "the approved step title must survive — the board is seeded, not re-typed"
        );
        assert_eq!(snapshot[0].status, TaskStatus::InProgress);
        assert_eq!(snapshot[1].status, TaskStatus::Pending);

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// The fan-out half of #1719's definition of done: several candidates
    /// keep separate boards — the second to finish a step must not get a
    /// terminal-state error for work it really did — and none of them
    /// reports onto the shared channel.
    #[tokio::test]
    async fn fanout_candidates_keep_separate_and_silent_boards() {
        let root = scaffold("taskfan");
        let (port, mut rx) = port_with_events(root.clone());
        let first = port.create_workspace().await.unwrap();
        let second = port.create_workspace().await.unwrap();
        first.seed_task_board(&["the one step".into()], false);
        second.seed_task_board(&["the one step".into()], false);

        let done = first
            .tools()
            .execute("task_complete", &serde_json::json!({"id": "1"}))
            .await;
        assert!(!done.is_error(), "{done:?}");
        let sibling = second
            .tools()
            .execute("task_complete", &serde_json::json!({"id": "1"}))
            .await;
        assert!(
            !sibling.is_error(),
            "sibling boards must be independent — one candidate's terminal \
             state must not reject another's real work: {sibling:?}"
        );
        assert!(
            boards(&mut rx).is_empty(),
            "a fan-out candidate must not report its board onto the shared channel"
        );

        first.remove().await;
        second.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }
}
