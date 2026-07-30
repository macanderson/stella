// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Sub-agents: a bounded child turn with its own budget and its own
//! transcript, run to completion, returning only its summary to the parent
//! (#922).
//!
//! # The value is context economy, not parallelism
//!
//! `stella fleet` already fans work across processes. What this adds is
//! different and compounds with everything else in the loop: a parent that
//! needs to answer "which of these 40 files defines the retry policy"
//! currently reads all 40 results into its own history, where they stay for
//! the rest of the session and are re-sent on *every* subsequent step. A
//! sub-agent absorbs that cost in a transcript that is discarded and hands
//! back a paragraph.
//!
//! Compaction exists because transcripts grow. The cheapest growth is the
//! growth that never happens.
//!
//! # Why the engine barely needed changing
//!
//! [`Engine`] holds no conversation state, so a child is a struct literal
//! over the parent's fields with four of them replaced. [`BudgetGuard`] is
//! `Copy` with pure arithmetic, so a child allowance is a *value*
//! ([`BudgetGuard::carve`]) rather than shared mutable state. Every I/O seam
//! is already a trait object behind a shared reference, so a restricted tool
//! set is [`ReadOnlyTools`] and nothing else.
//!
//! # The five contracts
//!
//! **Budget is carved, never separate.** The child's ceiling is
//! `min(requested, parent's remaining headroom)` — see
//! [`BudgetGuard::carve`]. A child therefore cannot spend a parent past a
//! configured cap even when the caller asks for more than is left, which is
//! what keeps `--budget` a *hard* ceiling once turns nest. Spend settles
//! back into the parent exactly once, on every path including failure.
//! A carve with no room under an enforced cap refuses the spawn outright
//! rather than paying for the one model call that budget-checking-between-
//! steps would always let through.
//!
//! **Tools are read-only by default.** Write access is opt-in per spawn.
//! Beyond the obvious safety argument this is also the *fast* default: the
//! engine's speculation pump (`crate::speculation`) overlaps read-only tool
//! calls with the response still streaming, so a read-only child hides its
//! I/O behind generation on every step.
//!
//! **Failure is data.** [`SubAgentOutcome`] is a value the parent reasons
//! about, never an error that kills the parent turn. A child that aborts on
//! its carve, its step cap, or loop detection still returns the last text it
//! produced — salvaged from the transcript being discarded — so paid work is
//! not thrown away with the context it lived in.
//!
//! **The report is capped, and the cap is enforced.** A summary that could
//! be arbitrarily long defeats the entire premise; intent is not a
//! mechanism. [`SubAgentSpec::max_report_chars`] clamps what crosses back,
//! and `truncated` says so rather than hiding it.
//!
//! **Every seam the parent has, the child has** — except the one it must
//! not. [`Engine::with_sleeper`] cannot carry `gate`/`steering`/`hooks`
//! (they are builder-set private fields), which is precisely why
//! `goal.rs::assess` silently dropped all three when it hand-rolled a judge
//! engine. Constructing the child here, in the same crate, carries them:
//!
//! - The pause gate propagates. A child that ignored it would keep spending
//!   through a pause.
//! - The **soft stop** propagates, so "end this turn" ends the child too.
//! - Steering messages **do not**. [`TurnSteering::drain_steering`] is
//!   destructive by contract, so a child that inherited it would silently
//!   eat a message the user addressed to the parent. [`ChildSteering`] is
//!   the view that resolves this: stoppable, but never a message thief.
//!
//! # Event plane
//!
//! A child's events must be attributable without being mistaken for the
//! parent's stage boundaries. Rather than adding an agent field to all 36
//! [`AgentEvent`] variants, one
//! [`SubAgentPhase::Started`]/[`Finished`](SubAgentPhase::Finished) bracket
//! carries the attribution and the child's own `Stage`/`Complete` (plus its
//! narration and its carve-scoped `BudgetTick`) are dropped at the boundary.
//! `stella_protocol::subagent_event` documents each drop and why;
//! [`forwards_to_parent`] is the executable form. `StepUsage` is forwarded —
//! it is the metering record, and dropping it is exactly how child cost
//! would vanish from `stella stats` and quietly falsify `$/resolved task`.
//!
//! # Nesting
//!
//! [`SubAgentSpec::depth`] is checked against [`MAX_SUB_AGENT_DEPTH`] before
//! the first model call. There is deliberately no engine-level depth
//! counter: nothing a *model* can call spawns a sub-agent today, so recursion
//! is not reachable from a prompt. A future spawn tool MUST thread
//! `parent_depth + 1` into the spec it builds — that is the contract this
//! cap enforces, and `depth` rides the `Started` event so a violation is
//! visible in any journal.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use stella_protocol::{
    AgentEvent, CompletionMessage, MessageRole, ModelCallRole, Provider, SubAgentPhase,
    SubAgentStatus,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::budget::BudgetGuard;
use crate::bus::HookBus;
use crate::driver::{Engine, EngineConfig, TurnOutcome};
use crate::event_sender::EventSender;
use crate::ports::{ReadOnlyTools, ToolExecutor, TurnControls, TurnSteering};

/// How deep sub-agents may nest. A child of the top-level turn is depth 1.
///
/// Two, not one: a coordinator child that farms out its own searches is a
/// real shape. Beyond that the context saving is swamped by the per-child
/// prompt overhead, and the failure modes stop being debuggable.
pub const MAX_SUB_AGENT_DEPTH: u8 = 2;

/// Runs a sub-agent on behalf of a *tool*, which cannot build an [`Engine`]
/// of its own.
///
/// [`Engine::run_sub_agent`] needs a provider, a sleeper, a tool set and a
/// budget — all of which the engine holds by short-lived reference during the
/// turn a tool executes inside. This port is the inversion: the host (which
/// owns all four for real) implements it, a tool holds one behind an `Arc`,
/// and the two never have to name each other's lifetimes.
///
/// # Budget
///
/// An implementation carves the child from a **pool** it owns, not from the
/// caller's guard — the engine has that borrowed mutably for the whole turn.
/// The spend it charges must land in a [`SubAgentSpendLedger`] the executor
/// drains via [`ToolExecutor::drain_sub_agent_spend_usd`], which is what
/// folds it back into the parent at the next step boundary.
///
/// # Failure
///
/// Returns [`SubAgentOutcome`], never an error: a dispatcher that cannot run
/// anything at all (its host is gone, no provider is configured) reports
/// [`SubAgentOutcome::Refused`] so the model gets a tool result it can reason
/// about rather than the turn dying under it.
/// # Interruption
///
/// A dispatcher is session-scoped and the seams that stop a turn are
/// turn-scoped, so the two must be joined explicitly: an implementation is
/// expected to read the *current* turn's [`TurnControls`] at dispatch time
/// and hand them to the engine it builds, via [`Engine::with_turn_controls`].
/// Without that a paused session keeps spending inside a child, and a soft
/// stop ends the parent while the child runs on.
#[async_trait]
pub trait SubAgentDispatcher: Send + Sync {
    async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome;
}

impl<'a> Engine<'a> {
    /// Attach a turn's boundary controls in their owned form.
    ///
    /// The `&'a` on `controls` rather than on each port is what makes this
    /// usable from a session-scoped host: it holds the `Arc`s, hands out a
    /// borrow of the pair for as long as the engine it just built lives, and
    /// never has to name the lifetime of the turn the seams came from.
    ///
    /// Set on the *parent* engine, not the child's — [`Self::run_sub_agent`]
    /// propagates the gate as-is and narrows the steering to
    /// [`ChildSteering`], so this one call is what gives a child both the
    /// pause and the stop while keeping it from stealing the parent's queued
    /// messages.
    ///
    /// Absent seams are left absent rather than overwritten: an engine
    /// already carrying a per-turn `&dyn` gate keeps it if `controls` has
    /// none, so this composes with [`Engine::with_gate`] instead of racing
    /// it.
    #[must_use]
    pub fn with_turn_controls(mut self, controls: &'a TurnControls) -> Self {
        if let Some(gate) = controls.gate.as_deref() {
            self.gate = Some(gate);
        }
        if let Some(steering) = controls.steering.as_deref() {
            self.steering = Some(steering);
        }
        self
    }
}

/// Sub-agent spend awaiting fold-in to a parent budget, in USD.
///
/// Written by whatever dispatches children, drained once per step-boundary
/// budget check through [`ToolExecutor::drain_sub_agent_spend_usd`]. Mirrors
/// [`crate::mcp_usage::McpUsageLedger`]'s shape and its drain-once discipline
/// for the same reason: charging the same dollars twice is worse than
/// charging them late.
pub type SubAgentSpendLedger = Arc<Mutex<f64>>;

/// Add a finished child's spend to the ledger, tolerating a poisoned lock —
/// accounting must never take down a tool call. (It may, however, arrive
/// late; that is what the drain-at-step-boundary contract is for.)
pub fn push_sub_agent_spend(ledger: &SubAgentSpendLedger, cost_usd: f64) {
    let mut total = ledger.lock().unwrap_or_else(|p| p.into_inner());
    *total += cost_usd;
}

/// Take everything recorded so far, leaving the ledger at zero.
pub fn drain_sub_agent_spend(ledger: &SubAgentSpendLedger) -> f64 {
    let mut total = ledger.lock().unwrap_or_else(|p| p.into_inner());
    std::mem::replace(&mut *total, 0.0)
}

/// What a parent asks a child to do, and the bounds it must respect.
///
/// Every bound has a finite default: a spec built with [`Default`] and only
/// `instruction` set is already safe to run. Nothing here is `Option` in the
/// "unbounded" sense except `budget_usd`, and that one is clamped by the
/// parent's headroom regardless (see [`BudgetGuard::carve`]).
#[derive(Debug, Clone)]
pub struct SubAgentSpec {
    /// Stable id for this child, unique within the parent turn. Rides both
    /// lifecycle events and the extension bus's ambient attribution.
    pub agent_id: String,
    /// The child's system prompt. `None` seeds no system message — the
    /// child then inherits nothing but its instruction, which is the right
    /// default for a task the parent describes in full.
    pub system_prompt: Option<String>,
    /// The task, seeded as the child's first user message.
    pub instruction: String,
    /// Hard cap on the child's model calls.
    pub max_steps: usize,
    /// Output-token cap per child call.
    pub max_output_tokens: Option<u32>,
    /// Sampling temperature for the child. `Some(0.0)` for anything whose
    /// output the parent will parse.
    pub temperature: Option<f32>,
    /// Hard cap on the characters that may cross back into the parent.
    /// This is the context-economy guarantee in mechanism form — see the
    /// module docs.
    pub max_report_chars: usize,
    /// Requested USD carve, clamped to the parent's remaining headroom.
    /// `None` requests the whole headroom, which is still bounded whenever
    /// the parent is.
    pub budget_usd: Option<f64>,
    /// Whether the child may mutate the workspace. `false` runs it behind
    /// [`ReadOnlyTools`], enforced at execution time rather than by prompt.
    pub write_access: bool,
    /// Attribution role for the child's model calls.
    pub role: ModelCallRole,
    /// Receipt turn slot. Context receipts key on
    /// `(execution_id, turn_instance, step, call_seq)` and every turn
    /// restarts `step` at 0, so a child sharing the parent's slot would
    /// silently overwrite the parent's manifests in the store.
    pub turn_instance: u32,
    /// Nesting depth of the child being spawned; `1` for a child of the
    /// top-level turn. See [`MAX_SUB_AGENT_DEPTH`].
    pub depth: u8,
    /// Compaction budget for the child's private transcript, in tokens.
    /// `None` inherits the parent's — the right default, since a child doing
    /// broad search needs no less room than the turn that spawned it.
    pub compaction_budget_tokens: Option<u64>,
}

impl Default for SubAgentSpec {
    fn default() -> Self {
        Self {
            agent_id: "sub-agent".to_string(),
            system_prompt: None,
            instruction: String::new(),
            // Enough for a real search-and-read task; small enough that a
            // confused child cannot become a work session.
            max_steps: 16,
            max_output_tokens: None,
            temperature: None,
            // ~2k tokens: a substantial paragraph plus a code excerpt, and
            // an order of magnitude below what the transcript it replaces
            // would have cost the parent on every later step.
            max_report_chars: 8_000,
            budget_usd: None,
            write_access: false,
            role: ModelCallRole::Worker,
            turn_instance: 0,
            depth: 1,
            compaction_budget_tokens: None,
        }
    }
}

impl SubAgentSpec {
    /// A read-only child with the given id and task — the common case.
    #[must_use]
    pub fn read_only(agent_id: impl Into<String>, instruction: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            instruction: instruction.into(),
            ..Self::default()
        }
    }
}

/// The runtime seams a child needs that the parent [`Engine`] does not
/// already carry.
///
/// `provider` is separate because a child is very often served by a
/// *different* model than its parent — a cheap searcher, a cross-family
/// judge — and taking it here keeps that a per-spawn decision instead of a
/// second engine the caller has to assemble.
#[derive(Clone, Copy)]
pub struct SubAgentHost<'a> {
    /// The provider serving the child's calls.
    pub provider: &'a dyn Provider,
    /// The extension bus, when the caller owns one. Its ambient agent id is
    /// set for the child's lifetime and restored to whatever it displaced on
    /// the way out, including on an unwind — see [`AgentAttribution`].
    pub bus: Option<&'a HookBus>,
}

impl<'a> SubAgentHost<'a> {
    /// A host with no extension bus attached.
    #[must_use]
    pub fn new(provider: &'a dyn Provider) -> Self {
        Self {
            provider,
            bus: None,
        }
    }

    /// Attach the extension bus that should attribute the child's events.
    #[must_use]
    pub fn with_bus(mut self, bus: &'a HookBus) -> Self {
        self.bus = Some(bus);
        self
    }
}

/// What the parent gets back. The only thing that may enter the parent's
/// transcript — everything the child read, called, and reasoned through is
/// gone by the time this exists.
#[derive(Debug, Clone, PartialEq)]
pub struct SubAgentReport {
    /// The child's answer, already clamped to
    /// [`SubAgentSpec::max_report_chars`].
    pub summary: String,
    /// Whether the clamp cut it. Never silent.
    pub truncated: bool,
    /// The child's total spend, already settled into the parent's guard.
    pub cost_usd: f64,
    /// Model calls the child made.
    pub steps: usize,
    /// Messages the child's private transcript grew by — the context the
    /// parent did not have to carry, and did not have to re-send on every
    /// step for the rest of the session.
    pub absorbed_messages: usize,
}

/// How a child turn ended. A typed result the parent reasons about — never
/// an `Err` that kills the parent turn.
#[derive(Debug, Clone, PartialEq)]
pub enum SubAgentOutcome {
    /// The child reached a final answer with no further tool calls.
    Completed(SubAgentReport),
    /// The child's turn aborted cleanly at a step boundary — its carve, its
    /// step cap, loop detection, exhausted retries. `report.summary` carries
    /// whatever text it had already produced, which may be empty.
    Incomplete {
        report: SubAgentReport,
        reason: String,
    },
    /// The child never ran: refused before its first model call. Cost is
    /// exactly zero.
    Refused { reason: String },
}

impl SubAgentOutcome {
    /// The report, on the two paths that produced one.
    #[must_use]
    pub fn report(&self) -> Option<&SubAgentReport> {
        match self {
            SubAgentOutcome::Completed(report) | SubAgentOutcome::Incomplete { report, .. } => {
                Some(report)
            }
            SubAgentOutcome::Refused { .. } => None,
        }
    }

    /// What the child spent — zero on a refusal, real on every other path.
    #[must_use]
    pub fn cost_usd(&self) -> f64 {
        self.report().map_or(0.0, |report| report.cost_usd)
    }

    /// The child's answer, or `""` when it never produced one. Convenience
    /// for a caller that treats partial work the same as complete work.
    #[must_use]
    pub fn summary(&self) -> &str {
        self.report().map_or("", |report| report.summary.as_str())
    }

    fn status(&self) -> SubAgentStatus {
        match self {
            SubAgentOutcome::Completed(_) => SubAgentStatus::Completed,
            SubAgentOutcome::Incomplete { .. } => SubAgentStatus::Incomplete,
            SubAgentOutcome::Refused { .. } => SubAgentStatus::Refused,
        }
    }

    fn reason(&self) -> Option<&str> {
        match self {
            SubAgentOutcome::Completed(_) => None,
            SubAgentOutcome::Incomplete { reason, .. } | SubAgentOutcome::Refused { reason } => {
                Some(reason.as_str())
            }
        }
    }
}

/// A child's view of the parent's [`TurnSteering`]: honors the soft stop,
/// never consumes the parent's queued messages.
///
/// [`TurnSteering::drain_steering`] is destructive by contract — "whatever
/// this returns WILL be injected, so the implementation owns dedup" — so a
/// child that inherited the parent's steering directly would swallow a
/// message the user wrote to the parent, with no way to get it back. The
/// stop flag is latched and non-destructive, so forwarding it is free and
/// keeps a child interruptible.
pub struct ChildSteering<'a> {
    parent: &'a dyn TurnSteering,
}

impl<'a> ChildSteering<'a> {
    #[must_use]
    pub fn new(parent: &'a dyn TurnSteering) -> Self {
        Self { parent }
    }
}

impl TurnSteering for ChildSteering<'_> {
    fn drain_steering(&self) -> Vec<String> {
        Vec::new()
    }

    fn soft_stop_requested(&self) -> bool {
        self.parent.soft_stop_requested()
    }
}

/// Sets the extension bus's ambient agent id for a child's lifetime and
/// restores what it displaced on drop.
///
/// A guard rather than a set/clear pair because the restore must survive the
/// child's turn future being dropped mid-flight — a hard cancel, a panic in
/// a tool — otherwise the parent's remaining events are attributed to a
/// child that is no longer running.
pub struct AgentAttribution<'a> {
    bus: Option<&'a HookBus>,
    previous: Option<String>,
}

impl<'a> AgentAttribution<'a> {
    /// Enter attribution for `agent_id`. A `None` bus makes this a no-op,
    /// so callers without an extension bus pay nothing.
    #[must_use]
    pub fn enter(bus: Option<&'a HookBus>, agent_id: &str) -> Self {
        let previous = bus.and_then(|bus| bus.set_agent(Some(agent_id.to_string())));
        Self { bus, previous }
    }
}

impl Drop for AgentAttribution<'_> {
    fn drop(&mut self) {
        if let Some(bus) = self.bus {
            bus.set_agent(self.previous.take());
        }
    }
}

/// Whether one of a child's events crosses into the parent's stream.
///
/// The drop set is small and each entry is justified in
/// `stella_protocol::subagent_event`'s module docs. Note the direction of
/// the default: this is a `matches!` deny-list, so a *new* [`AgentEvent`]
/// variant forwards. That is deliberate — the failure mode of forwarding
/// something cosmetic is a redundant line in a HUD, while the failure mode
/// of dropping something is child spend silently vanishing from the
/// accounting. Fail toward visible.
#[must_use]
pub fn forwards_to_parent(event: &AgentEvent) -> bool {
    !matches!(
        event,
        // The parent (or the pipeline above it) is the sole authority for
        // stage boundaries and for the terminal event of a run.
        AgentEvent::Stage { .. }
            | AgentEvent::Complete { .. }
            // The child's narration is a draft of its report; the report is
            // delivered exactly once, on `Finished`.
            | AgentEvent::Text { .. }
            | AgentEvent::TextDelta { .. }
            | AgentEvent::Reasoning { .. }
            // A tick reports the emitting guard's numbers, and the child's
            // guard holds the carve, not the session.
            | AgentEvent::BudgetTick { .. }
    )
}

/// Truncate to `max` characters on a char boundary, appending `…` when cut.
/// Returns whether it cut, because a clamped report must never look
/// exhaustive.
pub(crate) fn truncate_marked(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        return (s.to_string(), false);
    }
    let cut: String = s.chars().take(max).collect();
    (format!("{cut}…"), true)
}

/// Truncate to `max` characters on a char boundary, appending `…` when cut.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    truncate_marked(s, max).0
}

/// Characters of the instruction carried on the `Started` event. The event
/// stream is journaled, so this is a display preview, never the prompt.
const INSTRUCTION_PREVIEW_CHARS: usize = 200;

impl Engine<'_> {
    /// Run a bounded child turn and return only its summary.
    ///
    /// See the module docs for the five contracts this upholds. `budget` is
    /// `&mut` because the child's spend settles into it before this returns —
    /// on every path, including a child that aborted.
    pub async fn run_sub_agent(
        &self,
        host: SubAgentHost<'_>,
        spec: &SubAgentSpec,
        budget: &mut BudgetGuard,
        events: &UnboundedSender<AgentEvent>,
    ) -> SubAgentOutcome {
        let events = EventSender::new(events.clone());
        self.run_sub_agent_with_sender(host, spec, budget, &events)
            .await
    }

    /// [`Self::run_sub_agent`] with a caller-supplied ordered event
    /// boundary, mirroring [`Engine::run_turn_with_sender`]: a paid
    /// `StepUsage` from the child cannot return to its engine before the
    /// caller's durability boundary has completed.
    pub async fn run_sub_agent_with_sender(
        &self,
        host: SubAgentHost<'_>,
        spec: &SubAgentSpec,
        budget: &mut BudgetGuard,
        events: &EventSender,
    ) -> SubAgentOutcome {
        let carve = budget.carve(spec.budget_usd);
        let _ = events.send(AgentEvent::SubAgent {
            phase: SubAgentPhase::Started {
                agent_id: spec.agent_id.clone(),
                instruction_preview: truncate_chars(&spec.instruction, INSTRUCTION_PREVIEW_CHARS),
                budget_usd: carve.session_limit_usd(),
                write_access: spec.write_access,
                depth: spec.depth,
            },
        });

        let outcome = match refusal(spec, &carve) {
            Some(reason) => SubAgentOutcome::Refused { reason },
            None => self.run_child_turn(host, spec, carve, budget, events).await,
        };

        let _ = events.send(AgentEvent::SubAgent {
            phase: SubAgentPhase::Finished {
                agent_id: spec.agent_id.clone(),
                status: outcome.status(),
                summary: outcome.summary().to_string(),
                truncated: outcome.report().is_some_and(|report| report.truncated),
                cost_usd: outcome.cost_usd(),
                steps: outcome.report().map_or(0, |report| report.steps),
                absorbed_messages: outcome
                    .report()
                    .map_or(0, |report| report.absorbed_messages),
                reason: outcome.reason().map(str::to_string),
            },
        });
        outcome
    }

    /// The child turn proper: build it, run it, settle it. Split out so
    /// [`Self::run_sub_agent_with_sender`] reads as the lifecycle it is —
    /// bracket, decide, bracket — with the engine assembly out of the way.
    async fn run_child_turn(
        &self,
        host: SubAgentHost<'_>,
        spec: &SubAgentSpec,
        mut carve: BudgetGuard,
        budget: &mut BudgetGuard,
        events: &EventSender,
    ) -> SubAgentOutcome {
        // Attribution is entered before anything the child could emit and
        // released by drop, so an unwind cannot leave the parent's later
        // events wearing the child's id.
        let _attribution = AgentAttribution::enter(host.bus, &spec.agent_id);

        // Read-only is enforced at execution time by the view, not by the
        // prompt: a child without write access structurally cannot mutate
        // the workspace even if it tries.
        let read_only = (!spec.write_access).then(|| ReadOnlyTools::new(self.tools));
        let tools: &dyn ToolExecutor = match &read_only {
            Some(view) => view,
            None => self.tools,
        };

        let child_steering = self.steering.map(ChildSteering::new);
        let steering = child_steering
            .as_ref()
            .map(|steering| steering as &dyn TurnSteering);

        let child = Engine {
            provider: host.provider,
            tools,
            sleeper: self.sleeper,
            config: EngineConfig {
                max_output_tokens: spec.max_output_tokens,
                temperature: spec.temperature,
                max_steps: spec.max_steps,
                turn_instance: spec.turn_instance,
                compaction_budget_tokens: spec
                    .compaction_budget_tokens
                    .unwrap_or(self.config.compaction_budget_tokens),
                ..self.config.clone()
            },
            call_role: spec.role,
            // Every seam the parent has, the child has — this is the whole
            // reason the child is built here rather than through
            // `Engine::with_sleeper`, which cannot carry these three.
            hooks: self.hooks,
            // The drift map is keyed per model, so a cross-family child
            // learns its own model's drift without blending into the
            // parent's — and starts warm instead of cold.
            calibration: self.calibration,
            gate: self.gate,
            steering,
        };

        // The child's private transcript. It is a local: nothing outside
        // this function can observe it, and it is dropped on return. That
        // is the primitive's whole point, expressed as a scope.
        let mut messages = Vec::with_capacity(2 + spec.max_steps * 2);
        if let Some(system) = &spec.system_prompt {
            messages.push(CompletionMessage::system(system.clone()));
        }
        messages.push(CompletionMessage::user(spec.instruction.clone()));
        let seeded = messages.len();

        let steps = Arc::new(AtomicUsize::new(0));
        let child_events = child_sender(events.clone(), steps.clone());
        let turn = child
            .run_turn_with_sender(&mut messages, &mut carve, &child_events)
            .await;

        // Settle before building the report: the child's money is the
        // parent's the moment the child stops, whatever it stopped for.
        budget.settle_child(&carve);
        // The parent's own post-settlement numbers, since the child's ticks
        // were dropped at the boundary and a HUD would otherwise sit stale
        // for the whole child run.
        let _ = events.send(AgentEvent::BudgetTick {
            spent_usd: budget.spent_usd(),
            limit_usd: budget.turn_limit_usd(),
            mode: budget.mode(),
            session_spent_usd: Some(budget.session_spent_usd()),
            session_limit_usd: budget.session_limit_usd(),
        });

        let absorbed_messages = messages.len().saturating_sub(seeded);
        let steps = steps.load(Ordering::Relaxed);
        let build = |text: &str| {
            let (summary, truncated) = truncate_marked(text.trim(), spec.max_report_chars);
            SubAgentReport {
                summary,
                truncated,
                cost_usd: carve.session_spent_usd(),
                steps,
                absorbed_messages,
            }
        };

        match turn {
            TurnOutcome::Completed { text, .. } => SubAgentOutcome::Completed(build(&text)),
            TurnOutcome::Aborted { reason, .. } => SubAgentOutcome::Incomplete {
                // Salvage: the child's last answer text is real, paid-for
                // work. Discarding it with the transcript would make an
                // abort strictly worse than never having spawned.
                report: build(last_assistant_text(&messages).unwrap_or_default()),
                reason,
            },
        }
    }
}

/// Why this spawn must not start, if it must not. Checked before the first
/// model call so a refusal costs exactly nothing.
fn refusal(spec: &SubAgentSpec, carve: &BudgetGuard) -> Option<String> {
    if spec.depth > MAX_SUB_AGENT_DEPTH {
        return Some(format!(
            "nesting depth {} exceeds the maximum of {MAX_SUB_AGENT_DEPTH}",
            spec.depth
        ));
    }
    if !carve.is_viable_carve() {
        return Some(
            "no budget headroom left in the parent's enforced cap — refused before spending"
                .to_string(),
        );
    }
    None
}

/// The child's event sender: drops what must not cross ([`forwards_to_parent`])
/// and counts committed model calls on the way past.
///
/// Counting here rather than from the turn outcome is what makes `steps`
/// truthful on an abort too — `StepUsage` is emitted per committed call, so
/// a child that died on step 5 of 16 reports 5.
fn child_sender(parent: EventSender, steps: Arc<AtomicUsize>) -> EventSender {
    EventSender::from_fn(move |event| {
        if matches!(event, AgentEvent::StepUsage { .. }) {
            steps.fetch_add(1, Ordering::Relaxed);
        }
        if forwards_to_parent(&event) {
            parent.send(event)
        } else {
            Ok(())
        }
    })
}

/// The last assistant text in a transcript, for salvaging an aborted child's
/// work. Skips empty assistant turns (a step that only called tools).
fn last_assistant_text(messages: &[CompletionMessage]) -> Option<&str> {
    messages
        .iter()
        .rev()
        .filter(|message| message.role == MessageRole::Assistant)
        .map(|message| message.content.trim())
        .find(|content| !content.is_empty())
}

#[cfg(test)]
mod tests;
