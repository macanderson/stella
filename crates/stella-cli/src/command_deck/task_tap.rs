//! The deck's task-board decorator, split out of `command_deck.rs` to keep
//! it under the size gate (the `driver/settlement.rs` pattern).

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ports::ToolExecutor;
use stella_protocol::{AgentEvent, TaskItem, ToolOutput, ToolSchema};
use stella_tools::ToolRegistry;
use tokio::sync::mpsc::UnboundedSender;

use crate::subsession::SupervisorMsg;

mod plan_gate;

pub(crate) use plan_gate::PlanSetup;

/// Mirrors the task board into the event stream: after any `task_*` tool
/// call the FULL board snapshot rides the turn's channel as
/// `AgentEvent::TaskUpdate` — persisted by the forwarder, so replay shows
/// the checklist exactly as it moved — and `task_assign`'s spawn requests
/// are handed to the driver's supervisor channel. `supervisor: None` is the
/// worker configuration (v1 delegation runs from the lead only; a worker's
/// stranded requests are reported on its lane by `crate::subsession`).
///
/// It is also where the board becomes a **scope**: the same `task_*` traffic
/// that produces `TaskUpdate` is what the plan gate reads to raise
/// `AgentEvent::ScopeReview` before the first step runs (#4594). One
/// decorator for both because they are one fact observed twice — see
/// [`plan_gate`] for why the board is the scope and why the gate asks.
pub(crate) struct TaskTap<'a> {
    pub(crate) inner: &'a dyn ToolExecutor,
    pub(crate) events: UnboundedSender<AgentEvent>,
    pub(crate) registry: &'a ToolRegistry,
    pub(crate) supervisor: Option<UnboundedSender<SupervisorMsg>>,
    /// `None` when no driver is attached to answer, or when the `plan_review`
    /// policy withholds the gate — it is then not installed rather than
    /// installed and auto-approving.
    plan_gate: Option<plan_gate::PlanGate>,
}

impl<'a> TaskTap<'a> {
    /// Build the tap and tell the registry whether delegation is real here.
    ///
    /// The supervisor is the only thing that turns a queued `task_assign`
    /// request into a running sub-agent, so it is also the only honest answer
    /// to "may `task_assign` accept?" — binding the two in one constructor is
    /// what keeps the next tap from advertising a delegation it cannot
    /// perform.
    ///
    /// `plan` is what the gate needs from the turn: the headline the board is a
    /// plan for, and the `plan_review` policy that decides whether there is a
    /// gate at all ([`PlanSetup`]).
    pub(crate) fn new(
        inner: &'a dyn ToolExecutor,
        events: UnboundedSender<AgentEvent>,
        registry: &'a ToolRegistry,
        supervisor: Option<UnboundedSender<SupervisorMsg>>,
        plan: PlanSetup,
    ) -> Self {
        if supervisor.is_some() {
            registry.enable_task_delegation();
        }
        let plan_gate =
            plan_gate::PlanGate::install(registry.question_broker(), events.clone(), plan);
        Self {
            inner,
            events,
            registry,
            supervisor,
            plan_gate,
        }
    }

    /// This lane's board, as the snapshot both the gate and `TaskUpdate` read.
    fn board(&self) -> Vec<TaskItem> {
        let board = self.registry.task_board();
        let guard = board.lock().unwrap_or_else(|p| p.into_inner());
        guard.items().to_vec()
    }
}

#[async_trait]
impl ToolExecutor for TaskTap<'_> {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.inner.schemas()
    }

    /// Forwarded unfiltered, like `schemas()` (#3287): the tap observes
    /// board writes, it does not change what exists.
    fn contracts(&self) -> Vec<stella_protocol::ToolContract> {
        self.inner.contracts()
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        // The plan gate runs BEFORE the tool, not after it: the park has to
        // sit at a safe boundary (AGENTS.md rule #6), and a plan reviewed after
        // its first step ran is a report rather than a gate.
        if name == stella_tools::tasks::START
            && let Some(gate) = &self.plan_gate
            && let Some(refused) = gate.review(&self.board()).await
        {
            return refused;
        }
        let output = self.inner.execute(name, input).await;
        if name.starts_with("task_") {
            let _ = self.events.send(AgentEvent::TaskUpdate {
                tasks: self.board(),
            });
            if let Some(sup) = &self.supervisor {
                for request in self.registry.take_spawn_requests() {
                    let _ = sup.send(SupervisorMsg::SpawnTask(request));
                }
            }
        }
        output
    }

    /// Forwarded: this is a decorator, and a decorator that let the default
    /// `0.0` stand would silently drop sub-agent spend out of the parent's
    /// budget (see the port's contract).
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.drain_sub_agent_spend_usd()
    }

    /// Forwarded for the same reason: a swallowed wait request silently
    /// turns parked waits (#1471) back into model-step polling.
    fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
        self.inner.drain_wait_request()
    }

    /// Forwarded: a decorator that let the empty default stand would silently
    /// turn the end-of-turn service assertion (#2764) off for every surface
    /// composed through it — the agent goes back to declaring a service done
    /// without ever being asked whether it is still listening.
    fn live_services(&self) -> Vec<stella_core::LiveService> {
        self.inner.live_services()
    }

    /// Forwarded: letting the empty default stand would silently serialize
    /// the inner executor's sibling spawns (see the port's contract). The
    /// spawn tool is `delegate`, not `task_*` — the tap never fires for it.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        self.inner.parallel_safe_names()
    }

    /// Forwarded: letting the `None` default stand would drop the blocking
    /// hook chain and the approval flow for every tool dispatched under a
    /// deck session — the tap sits between the decorators that dispatch names
    /// of their own and the registry that owns the gate, so a gate it does not
    /// forward is a gate nothing consults (#2793).
    fn dispatch_gate(&self) -> Option<&dyn stella_core::ports::DispatchGate> {
        self.inner.dispatch_gate()
    }

    /// Forwarded: the deck's lead lane wraps the discovery mount (which owns
    /// the invocation plane) in this tap, so a tap that let the empty default
    /// stand would silently stop active skill bodies surviving summarization
    /// (#2685) for every deck session.
    fn active_skill_slugs(&self) -> Vec<String> {
        self.inner.active_skill_slugs()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use stella_protocol::{
        Answer, QuestionOutcome, QuestionRequest, StageKind, StageScope, TaskStatus,
    };
    use stella_tools::registry::question::QuestionResponder;

    use super::*;

    /// A leaf claiming one parallel-safe name, standing in for the registry.
    struct Claiming;

    #[async_trait]
    impl ToolExecutor for Claiming {
        fn schemas(&self) -> Vec<ToolSchema> {
            Vec::new()
        }
        async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
            ToolOutput::Ok {
                content: String::new(),
                data: None,
            }
        }
        fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
            std::collections::HashSet::from(["delegate".to_string()])
        }
        fn active_skill_slugs(&self) -> Vec<String> {
            vec!["deploy".to_string()]
        }
    }

    /// The deck's lead lane wraps the whole stack in this tap last, so a tap
    /// that swallowed the claim would kill concurrent sibling spawns for
    /// every deck session no matter what the layers below advertised.
    #[test]
    fn the_task_tap_forwards_parallel_safe_names() {
        let inner = Claiming;
        let registry = ToolRegistry::new(std::path::PathBuf::from("."));
        let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tap = TaskTap {
            inner: &inner,
            events,
            registry: &registry,
            supervisor: None,
            plan_gate: None,
        };
        assert!(
            tap.parallel_safe_names().contains("delegate"),
            "the tap must forward the inner executor's concurrency claims"
        );
    }

    /// An executor that records whether the model's call ever reached it.
    struct Ran(Arc<AtomicBool>);

    #[async_trait]
    impl ToolExecutor for Ran {
        fn schemas(&self) -> Vec<ToolSchema> {
            Vec::new()
        }
        async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
            self.0.store(true, Ordering::SeqCst);
            ToolOutput::Ok {
                content: "started".into(),
                data: None,
            }
        }
    }

    /// A driver that always answers the same way.
    struct Scripted(QuestionOutcome);

    #[async_trait]
    impl QuestionResponder for Scripted {
        async fn respond(&self, _request: &QuestionRequest) -> QuestionOutcome {
            self.0.clone()
        }
    }

    /// The shipped policy, under `goal`. What a deck turn gets with no
    /// `plan_review` block anywhere in the settings chain and no `--plan`.
    fn setup(goal: &str) -> PlanSetup {
        PlanSetup {
            goal: goal.to_string(),
            policy: crate::settings::PlanReviewPolicy::default(),
        }
    }

    fn picked(label: &str, note: Option<&str>) -> QuestionOutcome {
        QuestionOutcome::Answered {
            answers: vec![Answer {
                header: "Plan".into(),
                question: String::new(),
                chosen: vec![label.to_string()],
                note: note.map(str::to_string),
            }],
        }
    }

    /// Build a tap over a board of `steps` pending rows, with `answer` as the
    /// driver's reply. Returns the tap's parts so a test can drive it.
    fn gated(
        steps: usize,
        answer: Option<QuestionOutcome>,
    ) -> (
        ToolRegistry,
        Arc<AtomicBool>,
        tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
        tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    ) {
        let registry = ToolRegistry::new(std::path::PathBuf::from("."));
        if let Some(answer) = answer {
            registry.attach_question_responder(
                Arc::new(Scripted(answer)),
                std::time::Duration::from_secs(30),
            );
        }
        {
            let board = registry.task_board();
            let mut guard = board.lock().unwrap_or_else(|p| p.into_inner());
            for i in 0..steps {
                guard.create(format!("step {}", i + 1), None, None);
            }
        }
        let (events, rx) = tokio::sync::mpsc::unbounded_channel();
        (registry, Arc::new(AtomicBool::new(false)), rx, events)
    }

    fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    /// **The #4594 witness.** `AgentEvent::ScopeReview` had no producer at all
    /// after #3865: every `ScopeProposal` in the tree was a fixture, while the
    /// deck's plan card, plan rail, fleet dashboard and board seeding all
    /// branched on it. The board is the plan, so the board is what the driver
    /// is asked to approve — and the card goes up BEFORE the first step runs.
    #[tokio::test]
    async fn starting_a_plan_puts_the_board_to_the_driver_as_a_scope_review() {
        let (registry, ran, mut rx, events) = gated(3, Some(picked("Start work", None)));
        let inner = Ran(ran.clone());
        let tap = TaskTap::new(&inner, events, &registry, None, setup("fix the router"));

        let out = tap
            .execute(stella_tools::tasks::START, &serde_json::json!({"id": "1"}))
            .await;
        assert!(matches!(out, ToolOutput::Ok { .. }), "approved: {out:?}");
        assert!(ran.load(Ordering::SeqCst), "an approved plan runs its step");

        let events = drain(&mut rx);
        let proposal = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ScopeReview { proposal } => Some(proposal),
                _ => None,
            })
            .expect("the plan reached the driver as a scope review");
        assert_eq!(proposal.summary, "fix the router");
        assert_eq!(proposal.steps, vec!["step 1", "step 2", "step 3"]);
        assert_eq!(proposal.revision, Some(1), "the first plan of the turn");

        // The deck reads approval off the next run-scoped stage that is not
        // the gate (`stella_tui::model`, #3398) — without it the proposal
        // stays latched and the lane reads `waiting input` while it works.
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::Stage { name, scope }
                    if name.kind() == Some(StageKind::Execute) && *scope == StageScope::Run
            )),
            "approval must be signalled on the wire: {events:?}"
        );
    }

    /// A refused plan does not run, and the driver's words reach the model —
    /// otherwise it re-proposes the plan that was just turned down.
    #[tokio::test]
    async fn a_refused_plan_stops_the_step_and_carries_the_reason() {
        let (registry, ran, mut rx, events) = gated(
            3,
            Some(picked("Change it first", Some("do the tests first"))),
        );
        let inner = Ran(ran.clone());
        let tap = TaskTap::new(&inner, events, &registry, None, setup("fix the router"));

        let out = tap
            .execute(stella_tools::tasks::START, &serde_json::json!({"id": "1"}))
            .await;
        let ToolOutput::Error { message, .. } = &out else {
            panic!("a refused plan must not report success: {out:?}");
        };
        assert!(message.contains("do the tests first"), "{message}");
        assert!(
            !ran.load(Ordering::SeqCst),
            "the step must not run once the driver asked for a change"
        );
        let events = drain(&mut rx);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::Stage { scope, .. } if *scope == StageScope::Run)),
            "a refusal is not an approval: {events:?}"
        );
    }

    /// With nobody attached to answer, the gate is not installed — it never
    /// asks and never emits. A non-interactive run must not record a plan as
    /// `waiting input` when nothing is waiting, which is what
    /// `AgentEvent::HunkReview`'s doc states for the sibling gate.
    #[tokio::test]
    async fn no_driver_means_no_gate_rather_than_a_gate_that_answers_itself() {
        let (registry, ran, mut rx, events) = gated(5, None);
        let inner = Ran(ran.clone());
        let tap = TaskTap::new(&inner, events, &registry, None, setup("fix the router"));

        let out = tap
            .execute(stella_tools::tasks::START, &serde_json::json!({"id": "1"}))
            .await;
        assert!(matches!(out, ToolOutput::Ok { .. }), "{out:?}");
        assert!(ran.load(Ordering::SeqCst), "unattended work is not gated");
        assert!(
            !drain(&mut rx)
                .iter()
                .any(|e| matches!(e, AgentEvent::ScopeReview { .. })),
            "nobody was asked, so nothing may claim a decision is pending"
        );
    }

    /// A plan small enough to read as it happens is not worth stopping a turn
    /// for — the threshold is what keeps the gate from firing on every
    /// two-step errand.
    #[tokio::test]
    async fn a_short_plan_is_not_worth_a_card() {
        let (registry, ran, mut rx, events) = gated(2, Some(picked("Start work", None)));
        let inner = Ran(ran.clone());
        let tap = TaskTap::new(&inner, events, &registry, None, setup("tidy up"));

        tap.execute(stella_tools::tasks::START, &serde_json::json!({"id": "1"}))
            .await;
        assert!(ran.load(Ordering::SeqCst));
        assert!(
            !drain(&mut rx)
                .iter()
                .any(|e| matches!(e, AgentEvent::ScopeReview { .. })),
            "two steps must not raise a card"
        );
    }

    /// One card per plan, not one per step: the driver agreed to the board, so
    /// the rest of its steps run without asking again.
    #[tokio::test]
    async fn an_approved_plan_is_asked_about_once() {
        let (registry, ran, mut rx, events) = gated(3, Some(picked("Start work", None)));
        let inner = Ran(ran.clone());
        let tap = TaskTap::new(&inner, events, &registry, None, setup("fix the router"));

        for id in ["1", "2", "3"] {
            tap.execute(stella_tools::tasks::START, &serde_json::json!({ "id": id }))
                .await;
        }
        let reviews = drain(&mut rx)
            .iter()
            .filter(|e| matches!(e, AgentEvent::ScopeReview { .. }))
            .count();
        assert_eq!(
            reviews, 1,
            "the plan was approved once, so it is asked once"
        );
    }

    /// A refusal does not count as a decision, so the model's revised plan is
    /// put to the driver again — and says which revision it is (#4333).
    #[tokio::test]
    async fn a_revised_plan_is_asked_about_again_and_numbered() {
        let (registry, ran, mut rx, events) =
            gated(3, Some(picked("Change it first", Some("smaller"))));
        let inner = Ran(ran.clone());
        let tap = TaskTap::new(&inner, events, &registry, None, setup("fix the router"));

        for _ in 0..2 {
            tap.execute(stella_tools::tasks::START, &serde_json::json!({"id": "1"}))
                .await;
        }
        let revisions: Vec<Option<u32>> = drain(&mut rx)
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ScopeReview { proposal } => Some(proposal.revision),
                _ => None,
            })
            .collect();
        assert_eq!(
            revisions,
            vec![Some(1), Some(2)],
            "a re-proposal after a refusal is r2, not a second r1"
        );
    }

    /// The gate reads the board it is about to work, not the whole history:
    /// steps already finished are not what the driver is being asked to agree
    /// to, and a board mostly done must not raise a card over its last row.
    #[tokio::test]
    async fn finished_steps_are_not_part_of_the_proposal() {
        let (registry, ran, mut rx, events) = gated(4, Some(picked("Start work", None)));
        {
            let board = registry.task_board();
            let mut guard = board.lock().unwrap_or_else(|p| p.into_inner());
            guard
                .set_status("1", TaskStatus::Completed)
                .expect("the board accepts a completion");
        }
        let inner = Ran(ran.clone());
        let tap = TaskTap::new(&inner, events, &registry, None, setup("fix the router"));

        tap.execute(stella_tools::tasks::START, &serde_json::json!({"id": "2"}))
            .await;
        let proposal = drain(&mut rx)
            .into_iter()
            .find_map(|e| match e {
                AgentEvent::ScopeReview { proposal } => Some(proposal),
                _ => None,
            })
            .expect("three steps are left, so a card goes up");
        assert_eq!(proposal.steps, vec!["step 2", "step 3", "step 4"]);
    }

    /// **The #4611 witness, half one.** The threshold used to be a `const`, so
    /// a two-step plan could never raise a card and a driver who wanted one had
    /// no way to ask. `plan_review.min_steps` is that number, and moving it
    /// changes whether the card goes up.
    #[tokio::test]
    async fn a_configured_threshold_decides_whether_a_card_is_raised() {
        let (registry, ran, mut rx, events) = gated(2, Some(picked("Start work", None)));
        let inner = Ran(ran.clone());
        let tap = TaskTap::new(
            &inner,
            events,
            &registry,
            None,
            PlanSetup {
                goal: "tidy up".into(),
                policy: crate::settings::PlanReviewPolicy {
                    enabled: true,
                    min_steps: 2,
                },
            },
        );

        tap.execute(stella_tools::tasks::START, &serde_json::json!({"id": "1"}))
            .await;
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, AgentEvent::ScopeReview { .. })),
            "two steps must raise a card once the workspace asked for two"
        );
        assert!(ran.load(Ordering::SeqCst), "and the approved step runs");
    }

    /// **The #4611 witness, half two.** A driver who never wants the card had
    /// to answer one per plan, forever. `plan_review.enabled = "off"` is the
    /// one more reason `install` returns `None`, so an attached driver is not
    /// asked and nothing claims a decision is pending.
    #[tokio::test]
    async fn the_gate_can_be_switched_off_with_a_driver_attached() {
        let (registry, ran, mut rx, events) = gated(5, Some(picked("Change it first", None)));
        let inner = Ran(ran.clone());
        let tap = TaskTap::new(
            &inner,
            events,
            &registry,
            None,
            PlanSetup {
                goal: "fix the router".into(),
                policy: crate::settings::PlanReviewPolicy {
                    enabled: false,
                    min_steps: 3,
                },
            },
        );

        let out = tap
            .execute(stella_tools::tasks::START, &serde_json::json!({"id": "1"}))
            .await;
        assert!(matches!(out, ToolOutput::Ok { .. }), "{out:?}");
        assert!(
            ran.load(Ordering::SeqCst),
            "a withheld gate cannot refuse a step — the scripted driver would have"
        );
        assert!(
            !drain(&mut rx)
                .iter()
                .any(|e| matches!(e, AgentEvent::ScopeReview { .. })),
            "nobody was asked, so nothing may claim a decision is pending"
        );
    }

    /// `--plan` (#1264) asks about every plan. The flag has been stamped onto
    /// `Config::plan_mode` and read by nothing since the staged pipeline left
    /// the workspace; `PlanReviewPolicy::for_run` is what it now does, and this
    /// is that policy reaching the board.
    #[tokio::test]
    async fn plan_mode_puts_even_a_one_step_plan_to_the_driver() {
        let plan = PlanSetup {
            goal: "fix the router".into(),
            policy: crate::settings::PlanReviewPolicy {
                enabled: false,
                min_steps: 9,
            }
            .for_run(true),
        };
        let (registry, ran, mut rx, events) = gated(1, Some(picked("Start work", None)));
        let inner = Ran(ran.clone());
        let tap = TaskTap::new(&inner, events, &registry, None, plan);
        tap.execute(stella_tools::tasks::START, &serde_json::json!({"id": "1"}))
            .await;
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, AgentEvent::ScopeReview { .. })),
            "a one-step plan is put to the driver under --plan"
        );
    }

    /// Same shape for the invocation plane (#2685): the tap sits above the
    /// discovery mount, so swallowing the live-slug answer would stop active
    /// skill bodies surviving summarization on every deck session.
    #[test]
    fn the_task_tap_forwards_active_skill_slugs() {
        let inner = Claiming;
        let registry = ToolRegistry::new(std::path::PathBuf::from("."));
        let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tap = TaskTap {
            inner: &inner,
            events,
            registry: &registry,
            supervisor: None,
            plan_gate: None,
        };
        assert_eq!(
            tap.active_skill_slugs(),
            vec!["deploy".to_string()],
            "the tap must forward the inner executor's live invocations"
        );
    }
}
