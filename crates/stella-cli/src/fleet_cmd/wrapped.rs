// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella fleet --pipeline <variant>`: one worker attempt's turn dispatched
//! through an installed wrapper plugin, bound once per attempt (#3695, fleet
//! half).
//!
//! A submodule of [`crate::fleet_cmd`] rather than a `mod` block inside
//! `fleet_cmd.rs`, for the reason AGENTS.md § "God files" gives for
//! [`crate::wrapper_plugin`] living beside `agent.rs`: this is new logic, and
//! the parent already carries the fan-out, the dashboard tee, the claim tap
//! and the report.
//!
//! # Bound per attempt, resolved from the invocation's workspace
//!
//! A fleet worker runs in the task's own tree — the shared work tree for a
//! [`stella_fleet::Isolation::SharedTree`] task, a git worktree for an
//! isolated one — but a plugin is installed in the workspace the *operator*
//! invoked `stella fleet` from. An isolated task's worktree is a fresh
//! checkout of a branch, so `.stella/plugins/` (untracked in every repository
//! that does not commit it) need not exist there at all: resolving the
//! variant against the worker's own root would make `--pipeline` work on a
//! shared-tree task and fail as "no installed plugin declares…" on an
//! isolated one, for reasons the operator has no way to see. So the roster is
//! read from the invocation root — the same root
//! [`crate::fleet_cmd::run_fleet`]'s own pre-flight resolve reads, which is
//! what makes a typo'd variant fail the command before a single task is
//! dispatched — and the *tree* the plugin is told about is the worker's, via
//! the candidate grant below.
//!
//! # What the grant names, and where its test plan comes from
//!
//! [`crate::wrapper_candidate::grant_shared_tree`] mints the grant over the
//! tree this attempt actually runs in, so a plugin reading or testing it
//! reads the directory the turn is mutating rather than a copy nothing
//! touched — the argument that module makes for `stella run`, applied to a
//! root that is per-task here.
//!
//! Its [`stella_plugin::TestPlan`] is the task's own
//! [`stella_fleet::Task::test_command`] (#3884). The oracle is **per task**,
//! not per run, which is why it is a plan-file field rather than a
//! `--test-command` flag: two tasks touching different subsystems are decided
//! by different commands, and one flag over a fan-out would hand every worker
//! an oracle that answers about somebody else's work. It crosses
//! [`stella_plugin::parse_test_invocation`] — the host's closed runner
//! vocabulary — on the way, so a plugin receives an argv this host would run
//! and never a line to hand to a shell, and a command that vocabulary refuses
//! fails **this task's** dispatch by name rather than the fan-out around it.
//!
//! A task that declares none (the default, and every positional `stella fleet
//! "…"` prompt, which builds [`stella_fleet::Task::new`]) grants no plan:
//! there is nothing for the host to parse, nothing to pin, and
//! [`stella_plugin::TamperFinding::NotChecked`] is the honest finding rather
//! than a `Clean` nobody established. A witness-flavoured plugin then reports
//! `Undecided` for that task — a refusal to credit a flip nobody could
//! observe, exactly what `crate::wrapper_candidate` argues for the same shape
//! on `stella run`.
//!
//! # Two host planes: `recall` and `child_turn`, on one slot rule
//!
//! Every internal round of one attempt shares the **one** execution row that
//! attempt opened, and context receipts key on
//! `(execution_id, turn_instance, step, call_seq)` with `step` restarting at 0
//! each turn — so this door's rounds and any host-made call beside them have
//! to be told apart by `turn_instance` alone.
//!
//! The rule is [`stella_core::turn_slots`]', stated once there and used
//! unchanged by every door that has this shape: a `turn_instance` is a **lane**
//! plus a sequence within that lane. This door's rounds take successive slots
//! of [`stella_core::turn_slots::WORKER_LANE`]
//! (`AttemptDriver::run_turn`); a wrapper plugin's child turns take successive
//! slots of `CHILD_TURN_LANE`, allocated by the plane itself. Neither counter
//! can see the other and neither has to: two lanes never share a slot however
//! far either counts.
//!
//! That is what this door was missing, not a decision (#3882). The plane used
//! to be pinned to one fixed slot — safe on `stella run`, whose every round
//! opens its own execution row, and unsafe here — so this host served `recall`
//! alone and a plugin declaring `[loop] calls = ["child_turn"]` was answered
//! `Unavailable` through the gate. It now assembles through
//! [`crate::wrapper_plugin::round_driver_host`], the same one `stella goal`
//! uses (#3833).
//!
//! The plane needs this session's `SubAgentDispatcher`, which needs the
//! provider, and the roster must be read before the per-worker
//! `cfg.workspace_root` override — so binding is two moments here exactly as
//! it is in `run_raw_one_shot`: [`resolve_for_attempt`] first, then
//! [`ResolvedAttempt::serving`] once the worker has installed its dispatcher.
//!
//! # Arbiter-grade wrappers are **not** refused here
//!
//! Unlike `stella goal` (`crate::wrapper_plugin::reject_arbiter_wrapper_on_goal`,
//! #3832), a fleet attempt has no completion arbiter of its own: its verdict
//! is the turn's own outcome, exactly as `stella run`'s is. So the wrapper's
//! `judge`/`again` hold loop is the only round-holder over this turn, there is
//! no second one to double, and an arbiter-grade plugin runs here for the same
//! reason it runs at its designed home.

use async_trait::async_trait;
use stella_core::budget::BudgetGuard;
use stella_core::{Engine, TurnOutcome};
use stella_plugin::TurnOutcome as WrapperTurnOutcome;
use stella_protocol::{AgentEvent, CompletionMessage};
use stella_runtime::wrapper::{DispatchReport, DrivenTurn, RoundInput, TurnDriver, TurnPrelude};
use tokio::sync::mpsc;

use crate::wrapper_candidate::GrantedCandidate;
use crate::wrapper_plugin::BoundWrapper;

/// One installed wrapper, bound for one worker attempt, together with the
/// candidate grant that names the tree that attempt runs in.
///
/// The two travel together for the reason [`GrantedCandidate`] itself gives:
/// a grant whose artifacts nobody pinned and a wrapper that cannot see the
/// tree are each half an answer, and the pair is minted from one fact.
#[derive(Debug)]
pub(super) struct AttemptWrapper {
    bound: BoundWrapper,
    candidate: GrantedCandidate,
    /// The task this attempt is for, carried so every line the plugin's report
    /// prints names it (#3883).
    ///
    /// Held on the binding rather than passed to [`Self::settle`] because a
    /// wrapper is bound per attempt and the attempt is the task: an id supplied
    /// at print time could disagree with the one the binding was made for, and
    /// a wrong attribution on a concurrent run is worse than none.
    task: String,
}

impl AttemptWrapper {
    /// The variant id this attempt's execution row records.
    ///
    /// Owned since #3801: a composition's id is its members' ids joined, so it
    /// is assembled on demand rather than stored.
    pub(super) fn variant(&self) -> String {
        self.bound.variant()
    }

    /// What the wrapper is asked about this attempt.
    ///
    /// `budget_metered` is the worker's own per-child cap rather than the
    /// fleet's aggregate: the guard this turn actually runs under is the one
    /// a plugin's signal condition is about.
    pub(super) fn round_input(&self, prompt: &str, budget_metered: bool) -> RoundInput {
        RoundInput {
            goal: prompt.to_string(),
            // Read off the grant rather than re-derived: the plan on the wire
            // is what a plugin can actually run, so a signal saying a test
            // exists must be the same fact the grant carries.
            signals: crate::wrapper_plugin::pre_turn_signals(
                self.candidate.grant.test.is_some(),
                budget_metered,
            ),
            candidate: Some(self.candidate.grant.clone()),
        }
    }

    /// A driver over `engine`, watching the artifacts this attempt pinned.
    pub(super) fn driver<'r, 'e>(
        &'r self,
        engine: &'r Engine<'e>,
        messages: &'r mut Vec<CompletionMessage>,
        budget: &'r mut BudgetGuard,
        events: &'r mpsc::UnboundedSender<AgentEvent>,
    ) -> AttemptDriver<'r, 'e> {
        AttemptDriver {
            engine,
            messages,
            budget,
            events,
            watch: &self.candidate.watch,
            rounds: 0,
            driven: None,
        }
    }

    /// Drive this attempt's whole round loop through the plugin.
    ///
    /// A method rather than exposing `bound`, for the reason
    /// [`crate::wrapper_plugin::BoundWrapper::report`] is one: the gate and the
    /// child-turn plane behind the dispatch outlive any single round, and a
    /// caller holding a second handle to them could report a different story
    /// from the one [`Self::settle`] prints.
    ///
    /// # Errors
    ///
    /// Whatever [`stella_runtime::wrapper::WrapperDispatch::run`] refuses — a
    /// plugin process that cannot be started, times out, or answers off
    /// contract.
    pub(super) async fn dispatch(
        &self,
        input: RoundInput,
        driver: &mut AttemptDriver<'_, '_>,
    ) -> Result<DispatchReport, stella_runtime::wrapper::WrapperError> {
        self.bound.dispatch.run(input, driver).await
    }

    /// Fold one attempt's dispatch into the outcome the worker reports.
    ///
    /// The **last** round's turn is the attempt's outcome, matching
    /// `crate::wrapper_plugin::run_wrapped`'s rule for `stella run` ("the
    /// run's exit status is the turn's, not the wrapper's verdict") — a
    /// wrapper that held the attempt open to fix something must be judged on
    /// the turn it ended with, not the one it rejected.
    ///
    /// # Errors
    ///
    /// A wrapper whose process could not be driven, or one whose dispatch
    /// returned without driving a turn at all: both are named reasons the
    /// attempt failed, never a silently successful one. `stella-cli` is a
    /// binary, so a `String` here is the finished product (AGENTS.md
    /// invariant 5).
    pub(super) fn settle(
        &self,
        report: Result<DispatchReport, stella_runtime::wrapper::WrapperError>,
        driver: AttemptDriver<'_, '_>,
    ) -> Result<TurnOutcome, String> {
        let report = report
            .map_err(|error| format!("wrapper \"{}\" cannot be driven: {error}", self.variant()))?;
        // Every fault, every host refusal and every child-turn spend, on
        // stderr — stdout carries the fleet's own machine-readable contract.
        // Text rather than the run's `--output-format` because these lines are
        // the human's only view of what a plugin concluded about this attempt,
        // and scoped to this attempt's task because `--max-concurrency > 1`
        // puts N workers' lines on one stream (#3883).
        self.bound
            .report(Some(&self.task), crate::OutputFormat::Text, &report);
        driver.driven.ok_or_else(|| {
            format!(
                "wrapper \"{}\" drove this attempt with no turn",
                self.variant()
            )
        })
    }
}

/// One worker attempt's turn, wrapped so an installed plugin's
/// `before_turn`/`after_turn` see it.
///
/// Unlike [`crate::wrapper_plugin::RawTurnDriver`] (`stella run`'s one-shot
/// driver) this does **not** go through [`crate::agent::run_turn`] — that
/// helper opens its own execution row, and a fleet attempt's row is the one
/// [`crate::fleet_cmd`] already opened, with its own renderer, claim tap and
/// commit observer around it. It drives [`Engine::run_turn_with_sender`]
/// directly instead, on the sender this lane owns (`worker_event_sender`, whose
/// stage pairing is what frames each turn — #3428).
pub(super) struct AttemptDriver<'r, 'e> {
    engine: &'r Engine<'e>,
    messages: &'r mut Vec<CompletionMessage>,
    budget: &'r mut BudgetGuard,
    events: &'r mpsc::UnboundedSender<AgentEvent>,
    watch: &'r crate::wrapper_candidate::TamperWatch,
    /// How many internal turns this dispatch has driven — and therefore the
    /// next one's `turn_instance`. See [`Self::run_turn`].
    rounds: u32,
    /// The last round's outcome, which is this attempt's — see
    /// [`AttemptWrapper::settle`].
    driven: Option<TurnOutcome>,
}

#[async_trait(?Send)]
impl TurnDriver for AttemptDriver<'_, '_> {
    async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn {
        // Invariant 7: `into_messages` hands back user messages, appended
        // after the byte-stable system prefix this conversation opens with.
        self.messages.extend(prelude.into_messages());
        // Each internal round claims its own slot of the worker lane under the
        // one execution row this attempt opened. Context receipts key on
        // `(execution_id, turn_instance, step, call_seq)` with `step`
        // restarting at 0 every turn, so an arbiter-grade wrapper's second
        // round would otherwise overwrite the first's step manifests — the
        // collision the goal door avoids by refusing that grade outright,
        // which this door has no reason to refuse (see the module doc). The
        // lane is what keeps the host's own child turns clear of these
        // (#3882); the sequence within it is this attempt's round count.
        let round_engine = self
            .engine
            .with_turn_instance(stella_core::turn_slots::slot(
                stella_core::turn_slots::WORKER_LANE,
                self.rounds,
            ));
        self.rounds += 1;
        // One observer per round: `tools` is a fact about *this* turn, and a
        // fold shared across rounds would report the first round's tools as
        // the third round's (#3552).
        let facts = crate::turn_facts::TurnFacts::new();
        let sender = facts.observing(super::worker_event_sender(self.events));
        let outcome = round_engine
            .run_turn_with_sender(self.messages, self.budget, &sender)
            .await;
        // `tools` is observed and reported as such in both arms, including the
        // aborted one: a turn that aborted after two tool calls made those two
        // calls. `changed_files` is `None` — "this host does not report it"
        // (#3834) rather than a guess at zero: the tree delta rides
        // `AgentEvent::FileChange`, which only `crate::agent::run_turn` emits
        // (via `crate::turn_files`), and this lane drives the engine directly.
        let observed = |completed: bool, answer: String| WrapperTurnOutcome {
            completed,
            answer,
            tools: Some(facts.tools()),
            changed_files: None,
        };
        let turn = match &outcome {
            TurnOutcome::Completed { text, .. } => observed(true, text.clone()),
            TurnOutcome::Aborted { reason, .. } => observed(false, reason.clone()),
        };
        self.driven = Some(outcome);
        DrivenTurn {
            outcome: turn,
            // The host's own comparison over artifacts it pinned before the
            // attempt, never the plugin's claim about its own witness (#3499).
            // `NotChecked` on this door today — see the module doc.
            tamper: self.watch.finding(),
        }
    }
}

/// An installed wrapper found for one attempt, and the grant over the tree
/// that attempt runs in — everything binding needs before the worker has a
/// provider.
///
/// The half that must run early, and for two reasons rather than one: a
/// `--pipeline` naming nothing installed has to fail as a typo before a paid
/// call, and the roster is read from the invocation root, which `run_task`
/// overwrites with the worker's own tree a few lines later. The other half
/// needs this attempt's `SubAgentDispatcher`, which needs the provider built
/// after that override — see [`Self::serving`].
pub(super) struct ResolvedAttempt {
    resolved: crate::wrapper_plugin::ResolvedWrapper,
    candidate: GrantedCandidate,
    task: String,
}

impl ResolvedAttempt {
    /// Bind the plugin's process to what this attempt will do for it.
    ///
    /// `recall` reaches the **invocation** workspace's context plane — the
    /// coordination root, like the claim store beside it — rather than an
    /// isolated worktree's, which has none and would answer every recall with
    /// nothing. `child_turn` reaches this attempt's own dispatcher, so the
    /// child runs in the worker's tree with the worker's tool policy and is
    /// paid for out of the worker's guard.
    ///
    /// # Errors
    ///
    /// A wrapper whose declared stage order cannot be resolved — which a
    /// validated manifest cannot hit, since the variant was found by its
    /// `[wrapper] id`.
    pub(super) fn serving(
        self,
        invocation_root: &std::path::Path,
        dispatcher: std::sync::Arc<crate::subagent::SessionSubAgents>,
    ) -> Result<AttemptWrapper, String> {
        // One host per member of the selection, built from that member's own
        // manifest: the child-turn plane reads the manifest's `[roles]` and
        // `[loop] max_calls`, so a shared one would let a second plugin name
        // the first's role intents (`ResolvedWrapper::serving`, #4094).
        let bound = self.resolved.serving(|manifest| {
            crate::wrapper_plugin::round_driver_host(
                invocation_root,
                manifest,
                std::sync::Arc::clone(&dispatcher)
                    as std::sync::Arc<dyn stella_core::subagent::SubAgentDispatcher>,
            )
        })?;
        Ok(AttemptWrapper {
            bound,
            candidate: self.candidate,
            task: self.task,
        })
    }
}

/// Resolve `variant` against what the **invocation** workspace has installed
/// and pin the tree one attempt runs in, with `test_command` as that attempt's
/// oracle.
///
/// # Errors
///
/// Whatever [`crate::wrapper_plugin::resolve`] refuses (a variant nothing
/// installed declares, a manifest with no `[runtime]` block), a tree the
/// candidate fence will not mint a grant over, or a `test_command` the host's
/// runner vocabulary refuses. Every one of them fails this attempt's dispatch
/// by name; the fan-out's other tasks are unaffected.
pub(super) fn resolve_for_attempt(
    invocation_root: &std::path::Path,
    tree: &std::path::Path,
    variant: &str,
    task: &stella_fleet::TaskId,
    test_command: Option<&str>,
) -> Result<ResolvedAttempt, String> {
    // Silent by design, and this is the one place in the crate where that is
    // not a dropped refusal: `run_fleet`'s pre-flight resolves the SAME
    // variant from the SAME root before any task is dispatched and prints
    // every notice this sink would print, once. Repeating them here would put
    // N identical copies of one sentence on the stderr of a run with N
    // workers, interleaved by concurrency and attributed to nothing.
    Ok(ResolvedAttempt {
        resolved: crate::wrapper_plugin::resolve(invocation_root, variant, &mut |_| {})?,
        candidate: crate::wrapper_candidate::grant_shared_tree(tree, test_command)?,
        task: task.to_string(),
    })
}
