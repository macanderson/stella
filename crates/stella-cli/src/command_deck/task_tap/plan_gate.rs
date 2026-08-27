// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The plan gate: the driver approves the task board before its first step
//! runs, `AgentEvent::ScopeReview` says so on the wire (#4594), and the turn's
//! [`PlanGraph`] records what was agreed and what happened (#5037).
//!
//! # Why the board is the scope
//!
//! `ScopeReview` had a complete consumer chain and no producer at all after
//! the staged pipeline left the workspace (#3865): the deck's plan card, the
//! fleet dashboard's `Blocked` row, the transcript fold and
//! the forwarder's board seeding all branch on it, and every `ScopeProposal`
//! in the tree was a fixture. The pipeline's own emitter had asked a planner
//! model to write a plan and then gated on it — but the raw step loop that
//! ships today already has a plan, in the one place a plan belongs: the task
//! board the `task_*` tools write. So the scope is not something to ask a
//! second model for. It is `TaskBoard::items()`.
//!
//! # Why it asks rather than only announcing
//!
//! Emitting the event without a decision pending would be a lie in four
//! places at once, all of them live: `stella_tui::deck::classify` reads
//! `ScopeReview` as `AgentStatus::WaitingInput`,
//! `stella_tui::fleet_dashboard` sets the lane `Blocked`,
//! `stella_tui::plan::Plan::propose` moves the rail to `pending approval`,
//! and `deck_ui::dispatch` releases a sticky dispatch card on the strength
//! of it. Each of those is right when somebody is being asked and wrong when
//! nobody is. So the gate asks.
//!
//! It asks through the question plane (#4212/#4220) rather than a card of its
//! own. The deck's [`QuestionResponder`][q] is already attached, already
//! bounded by a TTL, already withdrawn when its wait is abandoned, and its
//! overlay is a pure fold with a note editor and a "talk it through" way out
//! — which is exactly the shape a plan needs, because the useful answer to a
//! plan is usually "yes, but not that third step" rather than yes or no. The
//! modal scope dialog #3861 deleted is deliberately not rebuilt.
//!
//! # Not installed when nobody can answer, or when nobody wants to be asked
//!
//! [`QuestionBroker::is_attached`] decides whether the gate exists at all.
//! With no driver — a non-interactive run, a worker lane — the gate is not installed
//! rather than installed and auto-approving, which is the rule
//! [`AgentEvent::HunkReview`]'s own doc states for the sibling gate. Such a
//! run therefore emits no `ScopeReview` and parks on nothing.
//!
//! The second reason to return `None` is that somebody said so:
//! [`PlanReviewPolicy`] carries the `plan_review` settings block (#4611), and
//! `enabled: false` withholds the gate from an attached driver too. Its
//! `min_steps` is the other half — the threshold below was a `const` chosen by
//! judgement, and a judgement number a person cannot change is one they answer
//! once per plan forever.
//!
//! # The failure direction, and why it differs from an approval
//!
//! `crate::command_deck::mid_turn_ask`'s approval responder denies on every
//! path that is not an explicit yes, because it guards a call a policy
//! already flagged. This gate proceeds on every path that is not an explicit
//! change request, because it guards nothing: it is the driver's chance to
//! redirect work before it starts, and a closed deck or an expired card must
//! not wedge a turn that nobody objected to. A refusal here comes from a
//! person choosing one, never from silence.
//!
//! [q]: stella_tools::registry::question::QuestionResponder

use std::sync::Mutex;

use stella_core::plan_graph::PlanGraph;
use stella_protocol::{
    AgentEvent, CompletionMessage, DivergenceCause, MessageRole, PlanRevision, Question,
    QuestionOption, QuestionOutcome, QuestionRequest, ScopeProposal, StageKind, StageScope,
    TaskItem, TaskNode, ToolOutput,
};
use stella_tools::registry::question::QuestionBroker;

use crate::settings::PlanReviewPolicy;
use tokio::sync::mpsc::UnboundedSender;

/// The label whose selection means "run this plan". Matched by exact string
/// against [`stella_protocol::Answer::chosen`], so it is named once here
/// rather than written twice.
const APPROVE_LABEL: &str = "Start work";

/// What the model is told when the driver wants the plan changed. The
/// driver's own words are appended when they gave any; this is the frame that
/// makes the refusal actionable rather than a bare no.
const CHANGE_REQUESTED: &str = "the plan was not approved — the person driving wants it changed before any of it runs. \
     Revise the task board (task_create / task_cancel) to match what they asked for, then \
     start the revised plan. Do not run the plan you just proposed.";

/// The cause a revision records when the board grew after the plan was agreed
/// to and nothing failed to explain it. Factual rather than a placeholder: the
/// plan really did change, and a [`DivergenceCause`] may not be blank.
const PLAN_CHANGED: &str = "the plan changed after it was approved";

/// The cause a re-proposal records when the driver declined without saying
/// anything — a card that timed out, an empty submission — and the model put
/// the same plan up again.
const RE_PROPOSED: &str = "the plan was put to the person driving again";

/// How much of a failing tool's message becomes the cause of the revision it
/// provokes. One line, cut, because the cause rides a breadcrumb and a plan
/// card, not a log — the full text is already on the failing tool's own row.
const CAUSE_CHARS: usize = 120;

/// One session's plan gate.
///
/// Lives as long as the [`super::TaskTap`] that owns it, which is one engine
/// turn, so `approved` is per-turn by construction — the same lifetime the
/// deck gives `SessionModel::approved_scope`, which it clears when a new turn
/// opens.
pub(crate) struct PlanGate {
    questions: QuestionBroker,
    events: UnboundedSender<AgentEvent>,
    /// The plan's one-line headline: what the driver asked for.
    goal: String,
    /// How many open steps raise a card, from the settings chain and this
    /// invocation's `--plan-mode` (#4611). Read on every call rather than folded
    /// into `install`, so the number the gate applies is the one a reader can
    /// see beside the comparison.
    min_steps: usize,
    /// The cause a revision records when the plan changed and nothing failed
    /// to explain it, resolved once at install so no write path has to handle
    /// a blank one (a [`DivergenceCause`] may not be blank).
    plan_changed: DivergenceCause,
    /// The same, for a plan put up again unchanged.
    re_proposed: DivergenceCause,
    state: Mutex<GateState>,
}

/// What one turn hands the gate: the plan's headline and the policy that
/// decides whether there is a gate at all.
///
/// One argument rather than two because they are resolved at the same place
/// for the same reason — the deck's lead turn is the only caller that knows
/// both the conversation and the config — and because `TaskTap::new` is
/// already at the width where a fifth positional `usize` stops saying which
/// number it is.
pub(crate) struct PlanSetup {
    /// What the person driving last asked for ([`plan_goal`]).
    pub(crate) goal: String,
    /// The `plan_review` policy this invocation applies.
    pub(crate) policy: PlanReviewPolicy,
}

impl PlanSetup {
    /// The setup for one turn: the headline read off `messages`, and the
    /// settings policy composed with this invocation's `--plan-mode` flag.
    ///
    /// Both sources are joined here, once, so no caller downstream can apply
    /// only one of the two — the same discipline `Config::allowed_write_dirs`
    /// applies to its flag.
    pub(crate) fn for_turn(messages: &[CompletionMessage], cfg: &crate::config::Config) -> Self {
        Self {
            goal: plan_goal(messages),
            policy: cfg.plan_review.for_run(cfg.plan_mode),
        }
    }
}

#[derive(Default)]
struct GateState {
    /// This turn's plan graph, from the moment a first plan is put up. `None`
    /// until then: a graph cannot exist before somebody has been asked to
    /// approve a plan.
    graph: Option<PlanGraph>,
    /// The revision the driver last said yes to. Compared against the graph's
    /// current revision, so a plan that has changed since the yes is asked
    /// about again and one that has not is left alone.
    approved: Option<PlanRevision>,
    /// What the driver said when they turned the last draft down. Consumed by
    /// the next revision, because their words are the best cause there is.
    refusal: Option<DivergenceCause>,
    /// The most recent tool failure this turn — SPEC 8.1's "linked cause", the
    /// compiler error or failing gate that a repair step answers. Consumed by
    /// the next revision.
    failure: Option<DivergenceCause>,
}

impl GateState {
    /// Why this revision is being authored, best source first, falling back to
    /// `stated`.
    ///
    /// Taken rather than read: a cause explains one revision, and leaving it
    /// in place would attach the same compiler error to every later draft of
    /// the plan.
    fn take_cause(&mut self, stated: &DivergenceCause) -> DivergenceCause {
        self.refusal
            .take()
            .or_else(|| self.failure.take())
            .unwrap_or_else(|| stated.clone())
    }
}

impl PlanGate {
    /// The gate, or `None` when nobody is attached to answer it — or when the
    /// `plan_review` policy withholds it.
    ///
    /// The switch needed no new plumbing to reach the engine (#4611):
    /// `install` already answered "no" for the unattended case, so `off` is one
    /// more reason to return `None`.
    pub(crate) fn install(
        questions: QuestionBroker,
        events: UnboundedSender<AgentEvent>,
        plan: PlanSetup,
    ) -> Option<Self> {
        // Resolved here rather than at each write: `DivergenceCause` refuses a
        // blank string, and `install` is already the one place that answers
        // "is there a gate at all?" in `Option`, so the two non-empty literals
        // above are checked once and every later use is infallible without a
        // panic (AGENTS.md #5).
        let plan_changed = DivergenceCause::new(PLAN_CHANGED)?;
        let re_proposed = DivergenceCause::new(RE_PROPOSED)?;
        (plan.policy.enabled && questions.is_attached()).then_some(Self {
            questions,
            events,
            goal: plan.goal,
            min_steps: plan.policy.min_steps,
            plan_changed,
            re_proposed,
            state: Mutex::new(GateState::default()),
        })
    }

    /// Put `board` to the driver if this is the first step of an unapproved
    /// plan, or of one that has changed since they agreed to it. `None`
    /// proceeds; `Some` is the refusal the calling tool returns instead of
    /// running.
    ///
    /// Called **before** the tool runs, which is what keeps AGENTS.md rule #6 —
    /// abort at safe boundaries, never mid-tool — true of the park.
    ///
    /// # The plan graph decides which revision this is (#5037)
    ///
    /// The graph, not a counter, answers "which draft is this?". The first
    /// plan of the turn is `r1` and its `[:NEXT]` chain is that approval
    /// written down; every later proposal authors `r{n+1}` beside the plan it
    /// supersedes, so `r1` is still readable at `r4`. A board that changed
    /// since the driver agreed to it goes back to them at the new number,
    /// which is SPEC 8.1's *a proposed plan revision, never a silent fix* —
    /// before #5037 the gate latched on its first yes, so a task inserted
    /// after a gate failure ran unasked and the breadcrumb stayed on `r1`.
    ///
    /// The comparison is against the current revision's whole task list, so a
    /// step *finishing* is not a plan change and nobody is asked about it.
    ///
    /// Every revision after the first carries a [`DivergenceCause`], taken
    /// from the driver's own refusal, then from the turn's most recent tool
    /// failure (SPEC 8.1's linked cause — the compiler error that demanded the
    /// repair step), then from a plain statement of what happened. There is no
    /// fourth source, because a `DivergenceCause` cannot be blank.
    pub(crate) async fn review(&self, board: &[TaskItem]) -> Option<ToolOutput> {
        let steps = plan_steps(board);
        let tasks = plan_tasks(board);
        let revision = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            // Where the plan stands against the board: `None` means no plan
            // has been put up at all this turn.
            let standing = state
                .graph
                .as_ref()
                .map(|graph| (graph.revision(), graph.planned(graph.revision()) != tasks));
            match standing {
                // The first plan of the turn is `r1` — if the board is long
                // enough to be worth asking about at all.
                None => {
                    if steps.len() < self.min_steps {
                        return None;
                    }
                    let graph = PlanGraph::approve(tasks).ok()?;
                    let revision = graph.revision();
                    state.graph = Some(graph);
                    revision
                }
                // A plan is already on the graph. Either it has changed since
                // the driver last saw it, or they have not agreed to this
                // draft yet — both are a new revision, and one worth asking
                // about (SPEC 8.1). A plan they already approved, unchanged,
                // is not asked about again.
                Some((current, changed)) => {
                    if !changed && state.approved == Some(current) {
                        return None;
                    }
                    let stated = if changed {
                        self.plan_changed.clone()
                    } else {
                        self.re_proposed.clone()
                    };
                    let cause = state.take_cause(&stated);
                    state.graph.as_mut()?.revise(tasks, cause).ok()?
                }
            }
        };

        // Emit before parking, the same ordering rule the approval flow
        // states: a park-first flow leaves the stream silent exactly while
        // the driver is being asked, so no surface could render the card.
        let proposal = self.proposal(steps, revision);
        let _ = self.events.send(AgentEvent::ScopeReview {
            proposal: proposal.clone(),
        });

        let outcome = self.questions.ask(&request(&proposal)).await;
        match refusal(&outcome) {
            Some(refusal) => {
                // The driver's own words are the best cause a plan revision
                // can carry, so the next draft records them rather than the
                // gate's own flat statement that something changed.
                self.state.lock().unwrap_or_else(|p| p.into_inner()).refusal =
                    DivergenceCause::new(refusal.clone());
                Some(ToolOutput::classified_error(
                    stella_protocol::ErrorClass::InvalidInput,
                    refusal,
                ))
            }
            None => {
                self.state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .approved = Some(revision);
                // The deck infers approval from the next run-scoped stage
                // that is not the gate itself (`stella_tui::model`, #3398) —
                // there is no decision event, by design. Without this the
                // proposal would stay latched as pending for the rest of the
                // turn, and the lane would keep reading `waiting input`
                // while it worked.
                let _ = self.events.send(AgentEvent::Stage {
                    name: StageKind::Execute.into(),
                    scope: StageScope::Run,
                });
                None
            }
        }
    }

    /// Record a `[:THEN]` edge: this task actually ran.
    ///
    /// Best-effort and silent. A refusal here would mean the graph and the
    /// board had drifted apart — structurally unreachable, because
    /// [`Self::review`] synchronises the plan with the board immediately
    /// before the step runs — and a bookkeeping slip must not fail a tool
    /// call the driver approved.
    pub(crate) fn record_run(&self, task_id: &str) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(graph) = state.graph.as_mut() {
            let _ = graph.ran(task_id);
        }
    }

    /// Remember a failing tool call as the cause of whatever plan revision it
    /// provokes — SPEC 8.1's "linked cause".
    ///
    /// Any tool, not just the board's: the compiler error that demands a
    /// repair step comes from `bash` or `edit_file`, never from `task_create`.
    pub(crate) fn observe(&self, output: &ToolOutput) {
        let ToolOutput::Error { message, .. } = output else {
            return;
        };
        let Some(cause) = DivergenceCause::new(cause_line(message)) else {
            return;
        };
        self.state.lock().unwrap_or_else(|p| p.into_inner()).failure = Some(cause);
    }

    /// This turn's plan graph, for the store. `None` when no plan was ever put
    /// up, which is every turn whose board never reached the threshold.
    pub(crate) fn plan_graph(&self) -> Option<PlanGraph> {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .graph
            .clone()
    }

    /// The board as a proposal. `estimated_files` is `0` — *not stated* — and
    /// deliberately not guessed: the board says what the work is, never how
    /// many files it lands in, and a fabricated magnitude would be compared
    /// against thresholds as though somebody had measured it.
    fn proposal(&self, steps: Vec<String>, revision: PlanRevision) -> ScopeProposal {
        ScopeProposal {
            summary: self.goal.clone(),
            steps,
            estimated_files: 0,
            revision: Some(revision.get()),
            ..ScopeProposal::default()
        }
    }
}

/// The plan as the graph holds it: **every** board row, in board order.
///
/// Not [`plan_steps`]'s open subset, and the difference matters in both
/// directions. The graph's `[:NEXT]` chain is the plan, and a plan that shed
/// its finished steps would look like a changed plan after every completion —
/// which would put a card up between every pair of steps. Terminal rows also
/// stay on the board forever (a cancelled task keeps its row as an audit
/// trail, and ids are never reused), so a plan only ever grows, and a change
/// to it is an insertion somebody made.
fn plan_tasks(board: &[TaskItem]) -> Vec<TaskNode> {
    board
        .iter()
        .map(|item| TaskNode::new(item.id.clone(), item.subject.clone()))
        .collect()
}

/// A failing tool's message as a plan-revision cause: its first non-empty
/// line, cut to [`CAUSE_CHARS`].
///
/// One line because a cause rides a breadcrumb and a plan card — the full text
/// is already on the failing tool's own row, and a cause that spilled a stack
/// trace into the plan panel would be unreadable exactly when it matters.
fn cause_line(message: &str) -> String {
    let line = message.lines().map(str::trim).find(|l| !l.is_empty());
    line.map_or_else(String::new, |line| {
        match line.char_indices().nth(CAUSE_CHARS) {
            Some((cut, _)) => format!("{}…", line[..cut].trim_end()),
            None => line.to_string(),
        }
    })
}

/// The steps a plan gate presents: the board's open rows, in board order.
///
/// Terminal rows are left out. A plan that already has work behind it is not
/// what the driver is being asked to agree to, and counting finished steps
/// toward [`PlanReviewPolicy::min_steps`] would raise a card over a single
/// remaining task.
fn plan_steps(board: &[TaskItem]) -> Vec<String> {
    board
        .iter()
        .filter(|item| item.status.is_open())
        .map(|item| item.subject.clone())
        .collect()
}

/// The card: the plan as a numbered list, and two ways to answer it.
///
/// The steps ride the question body rather than the option list because they
/// are what is being agreed to, not what is being chosen between — and
/// because the deck's plan card is behind this overlay while it is up, so a
/// driver who could not read the plan here would be approving it blind.
fn request(proposal: &ScopeProposal) -> QuestionRequest {
    let mut body = String::new();
    if !proposal.summary.is_empty() {
        body.push_str(&proposal.summary);
        body.push('\n');
    }
    for (i, step) in proposal.steps.iter().enumerate() {
        body.push_str(&format!("\n{}. {step}", i + 1));
    }
    QuestionRequest {
        asker: None,
        questions: vec![Question {
            header: "Plan".to_string(),
            question: body,
            options: vec![
                QuestionOption {
                    label: APPROVE_LABEL.to_string(),
                    description: "run the plan as it stands".to_string(),
                },
                QuestionOption {
                    label: "Change it first".to_string(),
                    description: "say what to do differently and stella re-plans before \
                                  anything runs"
                        .to_string(),
                },
            ],
            multi_select: false,
        }],
    }
}

/// The refusal to hand back, or `None` to proceed.
///
/// Every outcome that is not a person asking for a change proceeds — see the
/// module docs on why this gate's failure direction is the opposite of an
/// approval's.
fn refusal(outcome: &QuestionOutcome) -> Option<String> {
    let note = match outcome {
        QuestionOutcome::Answered { answers } => {
            let answer = answers.first()?;
            if answer.chosen.iter().any(|c| c == APPROVE_LABEL) {
                return None;
            }
            // A free-text answer is the driver typing what they want instead
            // of picking a row, so it is a change request carrying its own
            // instructions. An empty pick is the wizard submitting nothing,
            // which is not a person asking for anything.
            let typed: Vec<&str> = answer
                .chosen
                .iter()
                .map(String::as_str)
                .filter(|c| *c != "Change it first")
                .collect();
            let mut said = typed.join("; ");
            if let Some(extra) = &answer.note {
                if !said.is_empty() {
                    said.push_str(" — ");
                }
                said.push_str(extra);
            }
            if answer.chosen.is_empty() && said.is_empty() {
                return None;
            }
            said
        }
        // "Talk it through" is a change request whose instructions are the
        // conversation the driver just started.
        QuestionOutcome::Deferred { note } => note.clone(),
        // Nobody answered: a closed deck, or the card outlived its TTL.
        QuestionOutcome::Declined { .. } => return None,
    };
    Some(if note.trim().is_empty() {
        CHANGE_REQUESTED.to_string()
    } else {
        format!("{CHANGE_REQUESTED} They said: {}", note.trim())
    })
}

/// The plan's headline: what the person driving last asked for.
///
/// The board holds steps and no summary, and `ScopeProposal::summary` is the
/// approval prompt's headline — so the one line that carries anything is the
/// request the plan answers, not a sentence synthesized from the steps below.
///
/// Recall rides the transcript as a `User` message (`stella_core::receipts`),
/// so the marked block is skipped: it is context the host inserted, never
/// something a person said.
pub(crate) fn plan_goal(messages: &[CompletionMessage]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| {
            m.role == MessageRole::User
                && !m.content.starts_with(stella_core::receipts::RECALL_MARKER)
        })
        .and_then(|m| m.content.lines().find(|l| !l.trim().is_empty()))
        .map(str::trim)
        .map(|line| {
            // One line, and short enough that the card spends its height on
            // the plan rather than on the prompt that produced it.
            match line.char_indices().nth(120) {
                Some((cut, _)) => format!("{}…", line[..cut].trim_end()),
                None => line.to_string(),
            }
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use stella_protocol::{Answer, TaskStatus};

    use super::*;

    fn item(id: &str, subject: &str, status: TaskStatus) -> TaskItem {
        TaskItem {
            id: id.to_string(),
            subject: subject.to_string(),
            description: None,
            status,
            owner: None,
            contract: None,
        }
    }

    fn board(n: usize) -> Vec<TaskItem> {
        (0..n)
            .map(|i| {
                item(
                    &(i + 1).to_string(),
                    &format!("step {}", i + 1),
                    TaskStatus::Pending,
                )
            })
            .collect()
    }

    fn answered(chosen: &[&str], note: Option<&str>) -> QuestionOutcome {
        QuestionOutcome::Answered {
            answers: vec![Answer {
                header: "Plan".into(),
                question: String::new(),
                chosen: chosen.iter().map(|c| (*c).to_string()).collect(),
                note: note.map(str::to_string),
            }],
        }
    }

    /// Only the open rows are the plan. A board most of the way through its
    /// work is not a proposal, and counting its finished rows toward the
    /// threshold would raise a card over the one step that is left.
    #[test]
    fn a_plan_is_the_board_rows_that_have_not_happened_yet() {
        let mut rows = board(2);
        rows.push(item("3", "already done", TaskStatus::Completed));
        rows.push(item("4", "abandoned", TaskStatus::Cancelled));
        assert_eq!(plan_steps(&rows), vec!["step 1", "step 2"]);
    }

    /// **The failure-direction witness.** This gate's opposite number — the
    /// approval responder in `mid_turn_ask` — denies on every path that is
    /// not an explicit yes. This one proceeds on every path that is not an
    /// explicit change request, because it guards nothing: a closed deck or
    /// an expired card must not wedge a turn nobody objected to.
    #[test]
    fn silence_proceeds_and_only_a_person_refuses() {
        assert!(
            refusal(&QuestionOutcome::Declined {
                reason: "the deck closed".into()
            })
            .is_none(),
            "nobody answered, so nobody objected"
        );
        assert!(
            refusal(&answered(&[APPROVE_LABEL], None)).is_none(),
            "the driver said start"
        );
        assert!(
            refusal(&answered(&[], None)).is_none(),
            "an empty submission is not a person asking for anything"
        );
        assert!(
            refusal(&answered(&["Change it first"], None)).is_some(),
            "the driver asked for a change"
        );
        assert!(
            refusal(&QuestionOutcome::Deferred {
                note: String::new()
            })
            .is_some(),
            "talking it through is a change request"
        );
    }

    /// The driver's own words reach the model, because "no" without them
    /// leaves it to guess what to change — and the guess is usually to
    /// re-propose the same plan.
    #[test]
    fn a_refusal_carries_what_the_driver_said() {
        let picked = refusal(&answered(&["Change it first"], Some("skip the migration")))
            .expect("a change was requested");
        assert!(picked.contains("skip the migration"), "{picked}");
        assert!(
            picked.contains("Do not run the plan you just proposed"),
            "the model must be told the proposed plan is off: {picked}"
        );

        // A typed answer is the driver writing the change instead of picking
        // the row, so those words carry too.
        let typed = refusal(&answered(&["use the staging bucket"], None))
            .expect("free text is a change request");
        assert!(typed.contains("use the staging bucket"), "{typed}");
    }

    /// A `Deferred` note is the conversation the driver started, and it is
    /// the only instruction the model gets — dropping it would turn "let's
    /// talk about step three" into a bare refusal.
    #[test]
    fn deferring_carries_the_note_too() {
        let refusal = refusal(&QuestionOutcome::Deferred {
            note: "what about the tests".into(),
        })
        .expect("deferring refuses");
        assert!(refusal.contains("what about the tests"), "{refusal}");
    }

    /// The card must show the plan. The deck's own plan card is behind this
    /// overlay while it is up, so a driver who cannot read the steps here is
    /// approving them blind.
    #[test]
    fn the_card_carries_every_step_and_the_headline() {
        let proposal = ScopeProposal {
            summary: "rewrite the router".into(),
            steps: vec!["read the routes".into(), "edit them".into()],
            revision: Some(1),
            ..ScopeProposal::default()
        };
        let request = request(&proposal);
        let question = &request.questions[0].question;
        assert!(question.contains("rewrite the router"), "{question}");
        assert!(question.contains("1. read the routes"), "{question}");
        assert!(question.contains("2. edit them"), "{question}");
        assert_eq!(
            request.questions[0].options[0].label, APPROVE_LABEL,
            "the approve row is matched by label, so it must be the one named"
        );
    }

    /// The headline is what the person asked for — not the recall block the
    /// host inserted ahead of it, which is also a `User` message.
    #[test]
    fn the_headline_skips_the_recall_block() {
        let messages = vec![
            CompletionMessage::user("an older request"),
            CompletionMessage::user("fix the auth redirect loop\nand add a test"),
            CompletionMessage::user(format!(
                "{}\n\nRelevant context: …",
                stella_core::receipts::RECALL_MARKER
            )),
        ];
        assert_eq!(plan_goal(&messages), "fix the auth redirect loop");
        assert_eq!(plan_goal(&[]), "", "no request yet is no headline");
    }

    /// A long prompt is cut rather than allowed to spend the card's height on
    /// itself.
    #[test]
    fn a_long_headline_is_cut() {
        let long = "x".repeat(400);
        let goal = plan_goal(&[CompletionMessage::user(&long)]);
        assert!(goal.ends_with('…'), "{goal}");
        assert!(goal.chars().count() <= 121, "{}", goal.chars().count());
    }
}
