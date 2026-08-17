// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The execute stage's engine-turn drivers, split out of `pipeline.rs` (which
//! is closed to growth) when the resumed variant joined them (#1671):
//! [`Pipeline::execute_plan`] walks a plan one engine turn per step,
//! [`Pipeline::run_engine_turn`] drives one fresh turn, and
//! [`Pipeline::resume_engine_turn`] drives a checkpoint-restored one. All
//! three share [`Pipeline::filtered_turn_events`], the wrapping sender that
//! keeps the signal tallies and the flip-halt observation identical no matter
//! which driver a turn came through.
//!
//! It is named for what it once did. Since #3379 it is overwhelmingly an
//! *observer*: it counts file changes, mutating and opaque calls, errored
//! commands, tool dispatches and turn endings as they stream past, and feeds
//! the flip halt. It drops exactly two things — the best-effort previews of a
//! concurrent fan-out, and the engine's `Stage` boundary — and the engine's
//! ending is no longer one of them.

use super::*;

use stella_protocol::TaskStatus;

/// The per-turn signal counters [`Pipeline::filtered_turn_events`] hands
/// back beside its sender: read after the turn ends, folded into the
/// candidate's [`ChangeSignals`]. Shared `Arc`s because the sender's closure
/// writes them from inside the event stream while the driver awaits the turn.
pub(super) struct TurnTallies {
    file_changes: Arc<AtomicU32>,
    mutating: Arc<AtomicU32>,
    opaque: Arc<AtomicU32>,
    errored_commands: Arc<AtomicU32>,
    /// Every `ToolStart` this turn dispatched, read-only calls included —
    /// unlike `mutating`, which excludes them on purpose (#2933). Not folded
    /// into `ChangeSignals`: that struct is the candidate's running total,
    /// and this is read once per turn, by the plan-step walk alone, to tell
    /// a turn that acted from one that only answered.
    tool_calls: Arc<AtomicU32>,
}

impl TurnTallies {
    /// Fold this turn's counts into the candidate's running signals.
    fn fold_into(&self, signals: &mut ChangeSignals) {
        signals.file_changes += self.file_changes.load(Ordering::Relaxed);
        signals.mutating_actions += self.mutating.load(Ordering::Relaxed);
        signals.opaque_actions += self.opaque.load(Ordering::Relaxed);
        signals.errored_commands += self.errored_commands.load(Ordering::Relaxed);
    }

    /// How many tool calls this one turn dispatched, mutating or not.
    pub(super) fn tool_call_count(&self) -> u32 {
        self.tool_calls.load(Ordering::Relaxed)
    }
}

/// Which slice of which plan [`Pipeline::run_plan_steps`] is walking, and the
/// board it reports onto.
///
/// The three positional fields travel together because they are one fact —
/// where in the plan this walk sits — and separating them is how a resumed
/// run's prompts came to be able to say "step 1 of 2" about the fourth step of
/// five. `board` joins them because every one of its ids is derived from
/// `offset`: the ordinals the scope gate numbered, not the index in `steps`.
pub(super) struct PlanWalk<'p> {
    /// The steps this walk runs — a *tail* of the plan on a resume.
    pub(super) steps: &'p [PlanStep],
    /// Where `steps[0]` sits in the whole plan (0 for a fresh run).
    pub(super) offset: usize,
    /// How many steps the whole plan has.
    pub(super) total: usize,
    /// The candidate whose private board this walk moves, when the run is
    /// isolated. `None` on the shared-tree and resumed paths, where there is
    /// no private board — the session's own tap still mirrors whatever the
    /// worker's `task_*` calls do.
    pub(super) board: Option<&'p dyn CandidateWorkspace>,
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
        board: Option<&dyn CandidateWorkspace>,
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
                .0
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
            self.run_plan_steps(
                PlanWalk {
                    steps,
                    offset: 0,
                    total: steps.len(),
                    board,
                },
                engine,
                spend,
                state,
            )
            .await
        }
    }

    /// Walk a plan's steps — one engine turn each — reporting each one on the
    /// candidate's board as it goes.
    ///
    /// Split from [`Pipeline::execute_plan`] so a resumed run (#1671) can
    /// finish the steps its crashed predecessor never reached; [`PlanWalk`]
    /// is what lets those prompts still name their true position ("step 4 of
    /// 5", not "step 1 of 2").
    pub(super) async fn run_plan_steps(
        &self,
        walk: PlanWalk<'_>,
        engine: &Engine<'_>,
        spend: &mut Spend<'_>,
        state: &mut CandidateState,
    ) -> Result<(), TurnAbort> {
        let PlanWalk {
            steps,
            offset,
            total,
            board,
        } = walk;
        // The board ids the scope gate numbered. `offset + i` is the position
        // in the WHOLE plan, so a resumed run's tail still moves the rows a
        // reader is looking at rather than restarting the checklist at 1.
        let mark = |i: usize, status: TaskStatus| {
            if let Some(ws) = board {
                ws.mark_plan_step(&(offset + i + 1).to_string(), status);
            }
        };
        for (i, step) in steps.iter().enumerate() {
            // The resume frame's cursor: which step's turn is in flight, so a
            // kill during this turn resumes here and not at the plan's top.
            self.record_progress(|p| p.next_step = Some(offset + i));
            // The plan rail's whole job, driven by the one party that knows
            // the answer — see `CandidateWorkspace::mark_plan_step`. Before
            // the turn, so the row is already pulsing while the step runs.
            mark(i, TaskStatus::InProgress);
            state
                .messages
                .push(CompletionMessage::user(plan_steps::step_prompt(
                    offset + i,
                    total,
                    &step.description,
                )));
            let (outcome, tool_calls) = self
                .run_engine_turn(
                    engine,
                    &mut state.messages,
                    spend.budget,
                    &mut state.signals,
                    state.flip_halt.clone(),
                )
                .await;
            match outcome {
                TurnOutcome::Completed { text, cost_usd } => {
                    *spend.total += cost_usd;
                    mark(i, TaskStatus::Completed);
                    // #2941: a process killed between here and the run's
                    // eventual verdict — the harness's wall-clock cap, a
                    // SIGKILL, a crash — must not discard a step that
                    // already finished. `deliver_checkpoint` is a no-op for
                    // every candidate but the sole one in its fan-out (see
                    // its doc), so this costs nothing on the common
                    // multi-candidate path.
                    if let Some(ws) = board {
                        match ws.deliver_checkpoint().await {
                            // A checkpoint's rows are attributed here and not
                            // at the final adoption, because adoption only
                            // ever sends its own remainder — whatever a
                            // checkpoint already delivered is absent from the
                            // list it returns. Attributing only there would
                            // lose exactly the work of a run that delivered
                            // early and then died (#2907/#2941).
                            Ok(delivered) => ws.attribute_adopted(&delivered),
                            Err(error) => {
                                return Err(TurnAbort {
                                    reason: format!(
                                        "candidate could not deliver step {}'s work early: \
                                         {error}",
                                        offset + i + 1
                                    ),
                                    kind: AbortKind::Failure,
                                });
                            }
                        }
                    }
                    // #1702: a worker that declares the whole goal done ends
                    // the walk — the remaining steps could only re-confirm
                    // finished work. The declaration is screened for polarity
                    // and position in `plan_steps`, not left to the verify
                    // stage as this loop originally assumed: a task whose
                    // subject is `/etc` or a system service leaves the diff
                    // probe an unchanged tree, so verify returns
                    // `UNVERIFIABLE` and refutes nothing (#2104).
                    let closed_out = plan_steps::goal_declared_complete(&text);
                    state.final_text = text;
                    let remaining = i + 1 < steps.len();
                    if closed_out {
                        // #1702's early close-out: the remaining steps are not
                        // abandoned, they are covered. Saying so on the rail is
                        // the difference between a plan that reads `6/6 done`
                        // and one that reads `2/6` beside an answer claiming
                        // the work is finished — the second is the report a
                        // reader cannot reconcile, and it is the one the rail
                        // gave before this.
                        for (j, _) in steps.iter().enumerate().skip(i + 1) {
                            mark(j, TaskStatus::Completed);
                        }
                        break;
                    }
                    // #2933: a turn that dispatched no tool call at all
                    // answered rather than worked, and `step_prompt` already
                    // tells the worker to do exactly that when a step is
                    // already covered — which measured true for the whole
                    // remaining plan far more often than not. Walking the
                    // rest one step per turn just repeats the same no-op
                    // answer at one full model call each; ask once instead,
                    // then stop regardless of what it finds.
                    if tool_calls == 0 && remaining {
                        state.messages.push(CompletionMessage::user(
                            plan_steps::outstanding_work_prompt(&steps[i + 1..]),
                        ));
                        let (follow_up, _) = self
                            .run_engine_turn(
                                engine,
                                &mut state.messages,
                                spend.budget,
                                &mut state.signals,
                                state.flip_halt.clone(),
                            )
                            .await;
                        match follow_up {
                            TurnOutcome::Completed { text, cost_usd } => {
                                *spend.total += cost_usd;
                                state.final_text = text;
                                for (j, _) in steps.iter().enumerate().skip(i + 1) {
                                    mark(j, TaskStatus::Completed);
                                }
                            }
                            TurnOutcome::Aborted {
                                reason,
                                kind,
                                cost_usd,
                            } => {
                                *spend.total += cost_usd;
                                for (j, _) in steps.iter().enumerate().skip(i + 1) {
                                    mark(j, TaskStatus::Cancelled);
                                }
                                return Err(TurnAbort { reason, kind });
                            }
                        }
                        break;
                    }
                }
                TurnOutcome::Aborted {
                    reason,
                    kind,
                    cost_usd,
                } => {
                    *spend.total += cost_usd;
                    // A step whose turn aborted did not finish, and a rail
                    // that left it pulsing would keep implying work is in
                    // flight after the run stopped — the same half-invariant
                    // `Plan::finish` holds on the TUI side.
                    mark(i, TaskStatus::Cancelled);
                    // Best-effort, and deliberately not propagated: an
                    // aborted turn can still have left real file writes
                    // behind, and this is the last chance to save them
                    // (#2941). The abort already has its own reason; a
                    // delivery failure on top of it would only obscure why
                    // the turn actually stopped.
                    if let Some(ws) = board
                        && let Ok(delivered) = ws.deliver_checkpoint().await
                    {
                        // Attributed on this path too: an aborted turn's
                        // already-written files are real work, and the record
                        // of what the session changed must not depend on how
                        // the turn ended (#2907).
                        ws.attribute_adopted(&delivered);
                    }
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
    /// "did any event report the tree changing", which a tool surface that
    /// reports no changes leaves silent; `mutating_actions` answers "was
    /// anything even asked to change", which nothing can defeat, because it
    /// is counted off the calls this pipeline dispatched rather than off any
    /// look at the world.
    /// Returns the turn's outcome alongside its own tool-call count — see
    /// [`TurnTallies::tool_call_count`] — so a caller that needs to tell an
    /// acting turn from a purely narrated one (#2933) does not have to infer
    /// it from `ChangeSignals`, which is the candidate's running total, not
    /// this one turn's.
    pub(super) async fn run_engine_turn(
        &self,
        engine: &Engine<'_>,
        messages: &mut Vec<CompletionMessage>,
        budget: &mut BudgetGuard,
        signals: &mut ChangeSignals,
        flip_halt: Option<Arc<FlipHalt>>,
    ) -> (TurnOutcome, u32) {
        // A halt is armed only when `run_candidate` observed a failing
        // baseline for a tracked command (#2661). An unfired latch answers
        // `None` at every consult, so an ordinary turn still runs exactly as
        // it always did; a turn with no tracked command has no deterministic
        // done-signal to watch and gets no latch at all.
        let (filtered, tallies) = self.filtered_turn_events(flip_halt.clone());
        let outcome = match flip_halt {
            Some(halt) => {
                engine
                    .with_turn_halt(halt as Arc<dyn TurnHalt>)
                    .run_turn_with_sender(messages, budget, &filtered)
                    .await
            }
            None => {
                engine
                    .run_turn_with_sender(messages, budget, &filtered)
                    .await
            }
        };
        let tool_calls = tallies.tool_call_count();
        tallies.fold_into(signals);
        (outcome, tool_calls)
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
        // Same halt arming as `run_engine_turn` (#2661) — the resumed path
        // must not be the copy that forgot it.
        let (filtered, tallies) = self.filtered_turn_events(flip_halt.clone());
        let mut state =
            stella_core::step::TurnState::from_checkpoint(checkpoint, &self.config.engine);
        let outcome = match flip_halt {
            Some(halt) => {
                engine
                    .with_turn_halt(halt as Arc<dyn TurnHalt>)
                    .drive(&mut state, &filtered)
                    .await
            }
            None => engine.drive(&mut state, &filtered).await,
        };
        tallies.fold_into(signals);
        let budget = stella_core::step::BudgetSnapshot::of(state.budget()).restore();
        (outcome, state.into_messages(), budget)
    }

    /// The event filter every execute-stage turn runs behind — see
    /// [`Pipeline::run_engine_turn`]'s doc for what it drops and what it
    /// tallies. Split out so the fresh and resumed drivers cannot drift: a
    /// filter the resumed path lacked would un-count its file changes and
    /// blind its flip halt.
    fn filtered_turn_events(&self, flip_halt: Option<Arc<FlipHalt>>) -> (EventSender, TurnTallies) {
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
        // The errored-command census (#2125): unconditional, because the fact
        // it records is invisible to every other channel — a chain that exits
        // 0 with a broken command inside it leaves no trace in the diff or
        // the oracle.
        let seen_command_errors = Arc::new(AtomicU32::new(0));
        let command_errors = seen_command_errors.clone();
        let seen_tool_calls = Arc::new(AtomicU32::new(0));
        let tool_calls = seen_tool_calls.clone();
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
                // The pipeline owns the run's stage vocabulary, and the engine
                // still emits one stage boundary of its own (`Execute`, at
                // `drive`'s entry). Forwarding it would put an `Execute` inside
                // the witness stage, which `replay::validate_stage_ordering`
                // correctly rejects as an illegal Witness -> Execute move. It
                // is dropped here until stage ownership moves the same way the
                // ending just did (#3379 residue).
                //
                // The engine's *ending* is no longer dropped: `TurnComplete`
                // goes straight through to the consumer, counted on the way
                // past and never edited. That is the whole point — this
                // wrapper observes the engine, it does not rewrite it.
                AgentEvent::Stage { .. } => Ok(()),
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
                    // Unconditional, unlike `mutating` below: #2933's replay
                    // guard needs "did this turn act at all", not "did it
                    // mutate", to tell a worker that made a read-only lookup
                    // from one that only narrated in prose.
                    tool_calls.fetch_add(1, Ordering::Relaxed);
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
                    // against the tracked test — only when a halt is armed,
                    // or this map would be pure overhead on every turn.
                    if halt_for_events
                        .as_ref()
                        .is_some_and(|halt| !halt.tracked().is_empty())
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
                            && let ToolOutput::Ok { content, .. } = output
                        {
                            // Nothing is emitted on the transition: this is a
                            // success, and the only event available in this
                            // closure is `Error`, which a TUI renders as a
                            // failure. The reason reaches the transcript as
                            // the halted turn's own text (`TurnHalt`).
                            halt.observe(&command, content);
                        }
                    }
                    // Scored off the result alone (#2125) — no call-id
                    // correlation, because the marker the census reads is the
                    // shell renderer's own and no dispatch record is needed to
                    // recognize it.
                    if crate::verify::command_errors::exited_zero_with_a_failed_command(output) {
                        command_errors.fetch_add(1, Ordering::Relaxed);
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
                errored_commands: seen_command_errors,
                tool_calls: seen_tool_calls,
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
