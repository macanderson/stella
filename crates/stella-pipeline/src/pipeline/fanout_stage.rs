//! Dispatching a best-of-N fan-out: `width` isolated candidates executing at
//! once over one turn's budget and one event stream (#1215).
//!
//! Split out of `pipeline.rs` for the same reason `witness_stage` is — this is
//! the one stage that runs several candidate pipelines against each other
//! rather than one after the other, and its concurrency concerns (who creates
//! worktrees, who narrates, who spends) do not belong inside the sequential
//! read of `run_best_of_n`. The *decisions* it makes about width and money
//! live in [`crate::candidate_fanout`]; this file is the I/O sequencing.
//!
//! # Three things had to be settled before candidates could overlap
//!
//! **Money.** [`FanOutBudget`] — a pre-dispatch gate plus a per-candidate
//! carve, bounding the aggregate at the cap plus one in-flight window. Its
//! module doc is the normative statement of that window.
//!
//! **Git.** Isolation is created in index order even when the candidates then
//! run together, and a candidate's *second* `create` — the pristine snapshot
//! its witness author works blind in — is funnelled through the same lock by
//! [`SerialCreates`]. `create` is a `git worktree add` plus a snapshot commit
//! against ONE repository; it costs milliseconds against a candidate's
//! minutes, so serializing it keeps a whole class of concurrent-git failures
//! out of the fan-out at no measurable wall-clock cost. Concurrency is worth
//! having on the model calls, which is where the time actually goes.
//!
//! **The event stream.** N candidates share one [`stella_core::EventSender`],
//! and the events that garble when they interleave are exactly the ones the
//! wire contract already marks as droppable: `TextDelta` is "strictly a
//! best-effort preview" whose authoritative twin is the `Text` event that
//! follows it, and `Reasoning` is an accumulate-as-you-go display stream. A
//! concurrent fan-out mutes those two and lets everything else through live,
//! so the transcript keeps every durable event — each candidate's full `Text`,
//! its tool calls, its file changes, its proof steps — and no consumer is
//! handed three models' sentences spliced into one paragraph. A fan-out of
//! width 1 mutes nothing, so the single-shot and authored-witness paths stream
//! exactly as they always did.
//!
//! # Two shared surfaces this deliberately does not fence
//!
//! **The file-touch tally.** [`Pipeline::observed_mutations`] reads a delta off
//! one session-wide [`crate::ports::FileTouchPort`], and concurrent candidates
//! read overlapping windows of it. It is not cross-talk *here* because an
//! isolated candidate executes against its workspace's own tool surface
//! (`CandidateWorkspace::tools`), which the session recorder never sees — the
//! delta stays at zero and the count falls through to the per-candidate
//! `FileChange` tally, which is private to one turn's engine channel. A host
//! that ever wires a recorder observing candidate workspaces too would need a
//! per-candidate port, not a shared one.
//!
//! **`EngineConfig::turn_instance`.** Every candidate runs under the turn's
//! one instance, so their step manifests key alike and the last writer wins.
//! That was already true when candidates ran in sequence — concurrency only
//! makes which one writes last unordered rather than "the highest index".
//! Giving each candidate its own instance is the real fix and is not attempted
//! here: the value is allocated by the caller (`stella-core::goal` walks it by
//! round), so minting offsets inside the pipeline would collide with the next
//! turn's.

use super::*;

use crate::candidate_fanout::FanOutBudget;

/// One candidate's finished work, tagged with the index that produced it.
///
/// Completion order is not index order once candidates overlap, and three
/// things downstream care about index: selection tie-breaks on it
/// ([`select_best_candidate`]), adoption pairs `candidates[i]` with
/// `workspaces[i]`, and the reported cost is summed in it so a run's total
/// does not depend on who finished first.
struct FinishedCandidate {
    index: usize,
    result: CandidateResult,
    cost_usd: f64,
}

/// One candidate's identity in the fan-out: its 1-based ordinal — the number
/// every candidate-facing surface speaks (`candidate_start_notice`,
/// [`ProofStep::VerdictDegraded`]) — and the isolated workspace it runs in.
struct CandidateSlot<'w> {
    ordinal: u32,
    ws: &'w dyn CandidateWorkspace,
}

/// A [`CandidateWorkspacePort`] that lets exactly one `create` run at a time.
///
/// Wrapping the port rather than serializing at the call sites is what makes
/// this cover the witness author's snapshot too: that `create` happens deep
/// inside a candidate, at a point the fan-out has no visibility into, and it
/// is a `git worktree add` against the same repository as everybody else's.
pub(super) struct SerialCreates<'p> {
    inner: &'p dyn CandidateWorkspacePort,
    /// A `tokio` mutex, not a `std` one: the guard is held across the
    /// `create().await` it exists to serialize.
    gate: tokio::sync::Mutex<()>,
}

impl<'p> SerialCreates<'p> {
    pub(super) fn new(inner: &'p dyn CandidateWorkspacePort) -> Self {
        Self {
            inner,
            gate: tokio::sync::Mutex::new(()),
        }
    }
}

#[async_trait::async_trait]
impl CandidateWorkspacePort for SerialCreates<'_> {
    async fn create(&self) -> Result<Box<dyn CandidateWorkspace>, WorkspaceError> {
        let _serialized = self.gate.lock().await;
        self.inner.create().await
    }
}

impl<'a> Pipeline<'a> {
    /// Snapshot one isolated workspace per candidate, in index order.
    ///
    /// `Err` is a slot, not an early return: isolation was *promised*, so a
    /// candidate that cannot be isolated is never run in the shared tree
    /// instead — it scores as a candidate that generated nothing and the rest
    /// of the fan-out goes on. The reason travels with the slot so the
    /// dispatcher can score it without re-deriving it.
    pub(super) async fn create_candidate_workspaces(
        &self,
        port: &dyn CandidateWorkspacePort,
        n: u32,
    ) -> Vec<Result<Box<dyn CandidateWorkspace>, String>> {
        let mut workspaces = Vec::with_capacity(n as usize);
        for i in 0..n {
            self.emit_text(candidate_start_notice(i, n, true));
            match port.create().await {
                Ok(ws) => workspaces.push(Ok(ws)),
                Err(e) => {
                    self.warn(format!("candidate {}/{n} skipped: {e}", i + 1));
                    workspaces.push(Err(format!("candidate isolation failed: {e}")));
                }
            }
        }
        workspaces
    }

    /// Execute one candidate per prepared workspace, `width` at a time, and
    /// return the results **in index order**.
    ///
    /// The workspaces arrive already created (in index order, by the caller)
    /// so that a slot which failed to isolate is a value here rather than a
    /// branch: it scores as a candidate that generated nothing and the rest of
    /// the fan-out goes on, exactly as it did when candidates ran in sequence.
    ///
    /// `budget` and `total` are the turn's, and both are settled before this
    /// returns: `budget` gains every candidate's spend through
    /// [`FanOutBudget`], `total` gains the same money summed by index.
    pub(super) async fn dispatch_isolated_candidates(
        &self,
        frame: TaskFrame<'_>,
        worker: &ResolvedRole<'_>,
        authoring: Option<WitnessAuthoring<'_>>,
        workspaces: &[Result<Box<dyn CandidateWorkspace>, String>],
        width: u32,
        spend: &mut Spend<'_>,
    ) -> Vec<CandidateResult> {
        // One mirror, one cursor per candidate — see `candidate_steering`.
        // Every candidate observes every steer exactly once whether they run
        // in sequence or together; the cursor is what makes that true, and it
        // was already there.
        let fan = self.steering.map(SteeringFanOut::new);
        let budgets = FanOutBudget::new(*spend.budget, width);
        // Set for the whole fan-out rather than per candidate: the question a
        // preview has to answer is "is anyone else writing to this stream",
        // and while a wide fan-out is in flight the answer is yes for all of
        // them. Cleared unconditionally below, including on the paths where a
        // candidate aborted.
        self.shared_event_lane.store(width > 1, Ordering::Relaxed);

        let mut finished: Vec<FinishedCandidate> =
            futures_util::stream::iter(workspaces.iter().enumerate().map(|(index, slot)| {
                let fan = fan.as_ref();
                let budgets = &budgets;
                async move {
                    let ws = match slot {
                        Ok(ws) => ws.as_ref(),
                        Err(reason) => {
                            return FinishedCandidate {
                                index,
                                result: CandidateResult::setup_aborted(
                                    frame.base_messages.to_vec(),
                                    reason.clone(),
                                ),
                                cost_usd: 0.0,
                            };
                        }
                    };
                    // A candidate the fan-out cannot fund never reaches a
                    // model, so it is the same shape of result as one that
                    // could not be isolated: nothing was generated, and the
                    // siblings that were funded still decide the turn.
                    let Some(mut allowance) = budgets.claim() else {
                        return FinishedCandidate {
                            index,
                            result: CandidateResult::setup_aborted(
                                frame.base_messages.to_vec(),
                                "candidate skipped: the turn's budget was already spent"
                                    .to_string(),
                            ),
                            cost_usd: 0.0,
                        };
                    };
                    let mut cost_usd = 0.0;
                    let result = self
                        .run_isolated_candidate(
                            CandidateSlot {
                                // `n` is a `u32` config knob, so the index
                                // always fits.
                                ordinal: index as u32 + 1,
                                ws,
                            },
                            frame,
                            worker,
                            authoring,
                            fan,
                            &mut Spend {
                                budget: &mut allowance,
                                total: &mut cost_usd,
                            },
                        )
                        .await;
                    // Settled here, in completion order, so the next candidate
                    // to claim a slot gates against money that is really gone.
                    budgets.settle(&allowance);
                    FinishedCandidate {
                        index,
                        result,
                        cost_usd,
                    }
                }
            }))
            .buffer_unordered(width.max(1) as usize)
            .collect()
            .await;

        self.shared_event_lane.store(false, Ordering::Relaxed);
        finished.sort_by_key(|candidate| candidate.index);
        *spend.budget = budgets.into_parent();
        // Summed in index order, not completion order: float addition is not
        // associative, and a run's reported cost must not depend on which
        // candidate happened to finish first.
        for candidate in &finished {
            *spend.total += candidate.cost_usd;
        }
        finished
            .into_iter()
            .map(|candidate| candidate.result)
            .collect()
    }

    /// Build one candidate's engine + surface over its own workspace and run
    /// it. The body is what used to sit inside `run_best_of_n`'s loop; it is a
    /// function now because every local in it — the engine, the steering view,
    /// the bound hook runner — has to live in the candidate's own frame rather
    /// than in a loop iteration shared with nobody.
    async fn run_isolated_candidate(
        &self,
        slot: CandidateSlot<'_>,
        frame: TaskFrame<'_>,
        worker: &ResolvedRole<'_>,
        authoring: Option<WitnessAuthoring<'_>>,
        fan: Option<&SteeringFanOut<'_>>,
        spend: &mut Spend<'_>,
    ) -> CandidateResult {
        let CandidateSlot { ordinal, ws } = slot;
        let bound_hook_runner = self.hooks.map(|(_, runner)| BoundHookRunner {
            inner: runner,
            cwd: ws.root(),
        });
        let surface = CandidateSurface {
            diagnostics: ws.diagnostics(),
            tests: ws.tests(),
            lint: self.lint,
            mutation: self.mutation,
            coverage: self.coverage,
            repo_status: ws.repo_status(),
            cwd: Some(ws.root()),
            hook_runner: bound_hook_runner
                .as_ref()
                .map(|runner| runner as &dyn HookRunner),
            workspace: Some(ws),
        };
        let mut engine = Engine::with_sleeper(
            worker.provider,
            ws.tools(),
            self.engine_config_for(surface),
            self.sleeper,
        );
        if let Some((hooks, runner)) = self.hooks {
            engine = engine.with_hooks(hooks, surface.hook_runner.unwrap_or(runner));
        }
        if let Some(gate) = self.turn_gate {
            engine = engine.with_gate(gate);
        }
        let view = fan.map(SteeringFanOut::candidate);
        if let Some(view) = view.as_ref() {
            engine = engine.with_steering(view);
        }
        self.run_candidate(ordinal, frame, authoring, &engine, surface, spend)
            .await
    }
}
