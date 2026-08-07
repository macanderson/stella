// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The execute stage's engine-turn drivers, split out of `pipeline.rs` (which
//! is closed to growth) when the resumed variant joined them (#1671):
//! [`Pipeline::execute_plan`] walks a plan one engine turn per step,
//! [`Pipeline::run_engine_turn`] drives one fresh turn, and
//! [`Pipeline::resume_engine_turn`] drives a checkpoint-restored one. All
//! three share [`Pipeline::filtered_turn_events`], the event filter that
//! keeps the signal tallies and the flip-halt observation identical no matter
//! which driver a turn came through.

use super::*;

/// The per-turn signal counters [`Pipeline::filtered_turn_events`] hands
/// back beside its sender: read after the turn ends, folded into the
/// candidate's [`ChangeSignals`]. Shared `Arc`s because the sender's closure
/// writes them from inside the event stream while the driver awaits the turn.
pub(super) struct TurnTallies {
    file_changes: Arc<AtomicU32>,
    mutating: Arc<AtomicU32>,
    opaque: Arc<AtomicU32>,
}

impl TurnTallies {
    /// Fold this turn's counts into the candidate's running signals.
    fn fold_into(&self, signals: &mut ChangeSignals) {
        signals.file_changes += self.file_changes.load(Ordering::Relaxed);
        signals.mutating_actions += self.mutating.load(Ordering::Relaxed);
        signals.opaque_actions += self.opaque.load(Ordering::Relaxed);
    }
}

impl<'a> Pipeline<'a> {
    /// Execute stage: one turn for simple/single-task; one turn per plan step
    /// for multi-step (each step guides a fresh engine turn). The last turn's
    /// text lands in `state.final_text`; `Err` is the first aborted turn's
    /// reason and kind, kept typed so the driver-side emit is not repeated.
    pub(super) async fn execute_plan(
        &self,
        plan: Option<&[PlanStep]>,
        engine: &Engine<'_>,
        spend: &mut Spend<'_>,
        state: &mut CandidateState,
    ) -> Result<(), TurnAbort> {
        self.emit(AgentEvent::Stage {
            name: StageKind::Execute,
        });
        // Borrowed, not collected: the steps are only read, so materializing a
        // `Vec<&PlanStep>` per candidate bought nothing.
        let steps: &[PlanStep] = plan.unwrap_or_default();
        if steps.is_empty() {
            match self
                .run_engine_turn(
                    engine,
                    &mut state.messages,
                    spend.budget,
                    &mut state.signals,
                    state.flip_halt.clone(),
                )
                .await
            {
                TurnOutcome::Completed { text, cost_usd } => {
                    state.final_text = text;
                    *spend.total += cost_usd;
                }
                TurnOutcome::Aborted {
                    reason,
                    kind,
                    cost_usd,
                } => {
                    *spend.total += cost_usd;
                    return Err(TurnAbort { reason, kind });
                }
            }
            Ok(())
        } else {
            self.run_plan_steps(steps, 0, steps.len(), engine, spend, state)
                .await
        }
    }

    /// Walk `steps` — a plan's tail beginning at `offset` within a plan of
    /// `total` steps — one engine turn per step. Split from
    /// [`Pipeline::execute_plan`] so a resumed run (#1671) can finish the
    /// steps its crashed predecessor never reached while each prompt still
    /// names its true position ("step 4 of 5", not "step 1 of 2").
    pub(super) async fn run_plan_steps(
        &self,
        steps: &[PlanStep],
        offset: usize,
        total: usize,
        engine: &Engine<'_>,
        spend: &mut Spend<'_>,
        state: &mut CandidateState,
    ) -> Result<(), TurnAbort> {
        for (i, step) in steps.iter().enumerate() {
            // The resume frame's cursor: which step's turn is in flight, so a
            // kill during this turn resumes here and not at the plan's top.
            self.record_progress(|p| p.next_step = Some(offset + i));
            state
                .messages
                .push(CompletionMessage::user(plan_steps::step_prompt(
                    offset + i,
                    total,
                    &step.description,
                )));
            match self
                .run_engine_turn(
                    engine,
                    &mut state.messages,
                    spend.budget,
                    &mut state.signals,
                    state.flip_halt.clone(),
                )
                .await
            {
                TurnOutcome::Completed { text, cost_usd } => {
                    *spend.total += cost_usd;
                    // #1702: a worker that declares the whole goal done
                    // ends the walk — the remaining steps could only
                    // re-confirm finished work, and a false declaration
                    // is the verify stage's to refute, not this loop's.
                    let closed_out = plan_steps::goal_declared_complete(&text);
                    state.final_text = text;
                    if closed_out {
                        break;
                    }
                }
                TurnOutcome::Aborted {
                    reason,
                    kind,
                    cost_usd,
                } => {
                    *spend.total += cost_usd;
                    return Err(TurnAbort { reason, kind });
                }
            }
        }
        Ok(())
    }

    /// Run one engine turn, forwarding every event to the consumer **live**
    /// (a concurrent drain task, not a post-hoc flush — an execute turn can
    /// run tool loops for minutes, and buffering froze the renderer for the
    /// whole turn) **except** the engine's `Stage`/`Complete` (the pipeline
    /// owns those), tallying `FileChange`s into `signals.file_changes` for
    /// the zero-diff guard and mutating-capable `ToolStart`s into
    /// `signals.mutating_actions` for the ladder's no-op rung.
    ///
    /// The tallies are deliberately independent. `file_changes` answers
    /// "did the recorder see the tree change", which a shell redirect defeats;
    /// `mutating_actions` answers "was anything even asked to change", which
    /// nothing can defeat, because it is counted off the calls this pipeline
    /// dispatched rather than off any look at the world.
    pub(super) async fn run_engine_turn(
        &self,
        engine: &Engine<'_>,
        messages: &mut Vec<CompletionMessage>,
        budget: &mut BudgetGuard,
        signals: &mut ChangeSignals,
        flip_halt: Option<Arc<FlipHalt>>,
    ) -> TurnOutcome {
        let (filtered, tallies) = self.filtered_turn_events(flip_halt.clone());
        // A halted engine only when there is something to watch, so a turn
        // with no armed flip runs on exactly the engine it always did.
        let halted;
        let engine = match flip_halt {
            Some(halt) => {
                halted = engine.with_turn_halt(halt as Arc<dyn TurnHalt>);
                &halted
            }
            None => engine,
        };
        let outcome = engine
            .run_turn_with_sender(messages, budget, &filtered)
            .await;
        tallies.fold_into(signals);
        outcome
    }

    /// Drive a checkpoint-restored turn to its end under the same event
    /// filter, halt wrapper, and signal tallies as a fresh one — the resumed
    /// execute stage's half of #1671.
    ///
    /// Returns the outcome, the finished transcript (the restored turn owns
    /// its messages, where a fresh turn borrows the caller's), and the turn's
    /// money meter continued from the checkpoint — the budget every later
    /// stage of the resumed run must keep spending from, because the crashed
    /// process's spend already happened and must not be granted twice.
    pub(super) async fn resume_engine_turn(
        &self,
        engine: &Engine<'_>,
        checkpoint: stella_core::step::Checkpoint,
        signals: &mut ChangeSignals,
        flip_halt: Option<Arc<FlipHalt>>,
    ) -> (TurnOutcome, Vec<CompletionMessage>, BudgetGuard) {
        let (filtered, tallies) = self.filtered_turn_events(flip_halt.clone());
        let halted;
        let engine = match flip_halt {
            Some(halt) => {
                halted = engine.with_turn_halt(halt as Arc<dyn TurnHalt>);
                &halted
            }
            None => engine,
        };
        let mut state = stella_core::step::TurnState::from_checkpoint(checkpoint, &self.config.engine);
        let outcome = engine.drive_restored_turn(&mut state, &filtered).await;
        tallies.fold_into(signals);
        let budget = stella_core::step::BudgetSnapshot::of(state.budget()).restore();
        (outcome, state.into_messages(), budget)
    }

    /// The event filter every execute-stage turn runs behind — see
    /// [`Pipeline::run_engine_turn`]'s doc for what it drops and what it
    /// tallies. Split out so the fresh and resumed drivers cannot drift: a
    /// filter the resumed path lacked would un-count its file changes and
    /// blind its flip halt.
    fn filtered_turn_events(
        &self,
        flip_halt: Option<Arc<FlipHalt>>,
    ) -> (EventSender, TurnTallies) {
        // The filtered sender is SYNCHRONOUS on purpose: when the outer
        // sender carries a durability boundary, a paid StepUsage cannot
        // return to the engine before append+flush completes. Draining a
        // channel from a spawned forwarder instead would let the engine make
        // another paid call before the previous one's metering row is durable.
        let seen_file_changes = Arc::new(AtomicU32::new(0));
        let count = seen_file_changes.clone();
        let seen_mutating = Arc::new(AtomicU32::new(0));
        let mutating = seen_mutating.clone();
        let seen_opaque = Arc::new(AtomicU32::new(0));
        let opaque = seen_opaque.clone();
        let read_only = self.read_only_tool_names();
        let consumer = self.events.clone();
        // Correlate a shell call's command line (carried on `ToolStart`) with
        // its exit status (carried in the `ToolResult` content), because
        // neither event has both. Keyed by `call_id` rather than a
        // last-command slot: a step dispatches up to eight calls
        // concurrently, so "the most recent command" is genuinely ambiguous.
        let pending_commands: Arc<Mutex<HashMap<String, String>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let halt_for_events = flip_halt;
        let commands = pending_commands.clone();
        // Read once per turn, not per event: a turn belongs to one candidate,
        // and the fan-out sets this before dispatching any of them.
        let shared_lane = self.shared_event_lane.load(Ordering::Relaxed);
        let filtered = EventSender::from_fn(move |event| {
            match &event {
                // The pipeline is the sole authority for stage boundaries and
                // the terminal event of an outcome-producing run — drop the
                // engine's per-turn copies.
                AgentEvent::Stage { .. } | AgentEvent::Complete { .. } => Ok(()),
                // Concurrent candidates share this stream, and these two are
                // the only events whose meaning depends on arriving
                // uninterrupted: `TextDelta` is a preview its own `Text` event
                // supersedes, and `Reasoning` is accumulated by the consumer.
                // Three models' fragments spliced together is not a preview of
                // anything. Everything durable still goes out live — see
                // `Pipeline::shared_event_lane`.
                AgentEvent::TextDelta { .. } | AgentEvent::Reasoning { .. } if shared_lane => {
                    Ok(())
                }
                AgentEvent::FileChange { kind, .. } => {
                    // Reads ride the same event for the files panel but are
                    // not changes — counting them would defeat the zero-diff
                    // guard on read-only turns.
                    if kind.is_mutation() {
                        count.fetch_add(1, Ordering::Relaxed);
                    }
                    consumer.send(event)
                }
                AgentEvent::ToolStart { call } => {
                    // Counted at dispatch, not at result: a call that errored
                    // or timed out still means the turn *tried* to act, and
                    // the no-op rung is about the attempt. Only a name the
                    // registry positively advertises as read-only is excluded.
                    if !read_only.contains(&call.name) {
                        mutating.fetch_add(1, Ordering::Relaxed);
                        // The warrant's premise check: a mutating call whose
                        // effects the diff cannot fully account for (the
                        // shell, processes, MCP, anything unrecognized)
                        // forfeits every path-classified waiver (#1701).
                        if !crate::witness::warrant::diff_accountable_mutator(&call.name) {
                            opaque.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    // Remember the command line so its result can be scored
                    // against the tracked test. Only when a halt is armed —
                    // otherwise this map is pure overhead on every turn.
                    if halt_for_events.is_some()
                        && let Some(command) = command_of(&call.input)
                        && let Ok(mut pending) = commands.lock()
                    {
                        pending.insert(call.call_id.clone(), command.to_string());
                    }
                    consumer.send(event)
                }
                AgentEvent::ToolResult {
                    call_id, output, ..
                } => {
                    // The agent running the tracked test itself is the
                    // earliest moment anyone can know the goal is met — and
                    // before this, it was the one observation the oracle
                    // never saw (it watched only a pre-execute baseline and
                    // post-execute verification). Feeding it here is what
                    // lets the engine stop at the next step boundary instead
                    // of running until a limit fires.
                    if let Some(halt) = halt_for_events.as_ref() {
                        let command = commands
                            .lock()
                            .ok()
                            .and_then(|mut pending| pending.remove(call_id));
                        if let Some(command) = command
                            && let ToolOutput::Ok { content } = output
                        {
                            // Nothing is emitted on the transition: this is a
                            // success, and the only event available in this
                            // closure is `Error`, which a TUI renders as a
                            // failure. The reason reaches the transcript as
                            // the halted turn's own text (`TurnHalt`).
                            halt.observe(&command, content);
                        }
                    }
                    consumer.send(event)
                }
                _ => consumer.send(event),
            }
        });
        (
            filtered,
            TurnTallies {
                file_changes: seen_file_changes,
                mutating: seen_mutating,
                opaque: seen_opaque,
            },
        )
    }

    /// Tool names the registry advertises as `read_only` — the calls that
    /// structurally cannot have changed the workspace.
    ///
    /// Membership is the *only* thing that lets a call be discounted, so the
    /// direction of every uncertainty is fixed: a name this set has never
    /// heard of (an MCP server attached mid-run, a host's own extension, a
    /// tool added since) counts as mutating, and the ladder declines to call
    /// the turn a no-op. Getting that backwards would let an unrecognized
    /// tool's real work be reported as nothing attempted.
    fn read_only_tool_names(&self) -> HashSet<String> {
        self.tools
            .schemas()
            .into_iter()
            .filter(|schema| schema.read_only)
            .map(|schema| schema.name)
            .collect()
    }
}
