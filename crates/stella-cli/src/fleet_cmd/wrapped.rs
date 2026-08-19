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
//! # What the grant names, and what it deliberately does not carry
//!
//! [`crate::wrapper_candidate::grant_shared_tree`] mints the grant over the
//! tree this attempt actually runs in, so a plugin reading or testing it
//! reads the directory the turn is mutating rather than a copy nothing
//! touched — the argument that module makes for `stella run`, applied to a
//! root that is per-task here.
//!
//! It carries **no [`stella_plugin::TestPlan`]**: `stella fleet` declares no
//! `--test-command`, so there is no invocation for the host to parse, nothing
//! to pin, and [`stella_plugin::TamperFinding::NotChecked`] is the honest
//! finding rather than a `Clean` nobody established. A witness-flavoured
//! plugin therefore reports `Undecided` on this door, which is a refusal to
//! credit a flip nobody could observe — exactly what `crate::wrapper_candidate`
//! argues for the same shape on `stella run`. Giving fleet its own
//! `--test-command` is a flag decision with its own blast radius (it is
//! per-task, not per-run, so it belongs in the plan file rather than on the
//! command line) and is tracked in #3884.
//!
//! # One host plane: `recall`, and deliberately no `child_turn`
//!
//! `crate::wrapper_plugin`'s `PLUGIN_CHILD_TURN_SLOT` is a fixed
//! `turn_instance`, and this door's own rounds claim consecutive slots from 0
//! (`AttemptDriver::run_turn`) so that a plugin holding an attempt open past
//! its first internal turn cannot collide two rounds' step manifests under the
//! one execution row a fleet attempt opens. A fixed child slot would collide
//! with whichever round lands on it, which is the same hazard the goal door
//! states for its own round math (#3833). So this host serves `recall` only,
//! and a plugin naming a `[roles]` tier is answered `Unavailable` through the
//! gate — a refusal the run reports rather than a silence (#3882).
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
}

impl AttemptWrapper {
    /// The variant id this attempt's execution row records.
    pub(super) fn variant(&self) -> &str {
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
            // No `--test-command` reaches this door — see the module doc.
            signals: crate::wrapper_plugin::pre_turn_signals(false, budget_metered),
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
        // the human's only view of what a plugin concluded about this attempt;
        // they are not attributed to a task id yet (#3883).
        self.bound.report(crate::OutputFormat::Text, &report);
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
        // Each internal round claims its own `turn_instance` under the one
        // execution row this attempt opened. Context receipts key on
        // `(execution_id, turn_instance, step, call_seq)` with `step`
        // restarting at 0 every turn, so an arbiter-grade wrapper's second
        // round would otherwise overwrite the first's step manifests — the
        // collision the goal door avoids by refusing that grade outright,
        // which this door has no reason to refuse (see the module doc).
        let round_engine = self.engine.with_turn_instance(self.rounds);
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

/// Resolve `variant` against what the **invocation** workspace has installed
/// and bind it for one attempt running in `tree`.
///
/// # Errors
///
/// Whatever [`crate::wrapper_plugin::resolve`] refuses (a variant nothing
/// installed declares, a manifest with no `[runtime]` block), a tree the
/// candidate fence will not mint a grant over, or a wrapper whose declared
/// stage order cannot be resolved.
pub(super) fn bind_for_attempt(
    invocation_root: &std::path::Path,
    tree: &std::path::Path,
    variant: &str,
) -> Result<AttemptWrapper, String> {
    // Silent by design, and this is the one place in the crate where that is
    // not a dropped refusal: `run_fleet`'s pre-flight resolves the SAME
    // variant from the SAME root before any task is dispatched and prints
    // every notice this sink would print, once. Repeating them here would put
    // N identical copies of one sentence on the stderr of a run with N
    // workers, interleaved by concurrency and attributed to nothing.
    let resolved = crate::wrapper_plugin::resolve(invocation_root, variant, &mut |_| {})?;
    let candidate = crate::wrapper_candidate::grant_shared_tree(tree, None)?;
    // `recall` reaches the workspace's own context plane — the coordination
    // root, like the claim store beside it — rather than an isolated
    // worktree's (which has none, and would answer every recall with nothing).
    let host = crate::wrapper_plugin::WrapperHost::recalling(Box::new(
        crate::wrapper_recall::SessionRecallHost::open(invocation_root),
    ));
    let bound = resolved.serving(host)?;
    Ok(AttemptWrapper { bound, candidate })
}
