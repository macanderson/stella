// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Re-enter the staged pipeline over a checkpoint-restored turn (#1671) —
//! the restoration half of #1615's honesty work.
//!
//! A killed supervised run leaves two durable records in the same
//! work-journal commit: the engine's turn checkpoint and the pipeline's
//! frame. The frame used to carry only *configuration* (which stages the run
//! had armed); this module adds the **progress** record — the facts the
//! pipeline learns while running that a resume cannot re-derive — and the
//! seam that spends them: [`Pipeline::resume`] finishes the interrupted
//! turn, walks the plan steps the crash never reached, and then flows into
//! the same witness → verify → verdict tail every ordinary candidate takes,
//! so a resumed run ends with a real [`Verdict`] instead of an honest shrug.
//!
//! # What still does not come back (and why that is stated, not hidden)
//!
//! - **A candidate worktree.** `FrameProgress::isolated` records that execute
//!   ran inside an isolated workspace; that workspace died with the process,
//!   so [`PipelineResume::from_progress`] declines and the caller keeps the
//!   honest bare-turn path. Reattachment/recreation is the tracked follow-up.
//! - **An authored witness.** Authoring requires a pristine baseline snapshot
//!   of the *pre-execution* tree, which no post-crash process can take. The
//!   resumed tail runs with `authoring = None`, exactly the degradation the
//!   unauthored ladder already handles — the verdict escalates to the model
//!   verifier rather than crediting an unprovable flip.
//! - **The lint baseline (#861).** Same reason: it was a pre-execution
//!   observation held in process memory. The regression veto sits out a
//!   resumed run.
//!
//! # Signal honesty
//!
//! The crashed process's [`ChangeSignals`] tallies died with it, so a resumed
//! candidate's counters begin at the resume point. The diff, by contrast, is
//! whole — it reads the tree. When the diff shows work the tallies missed,
//! the resumed run credits one mutating action so the warrant and the no-op
//! rung err toward *more* verification, never toward waving through a change
//! nothing appears to have made.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use stella_core::step::Checkpoint;

use super::*;

/// The facts the pipeline learns mid-run that a resume needs and cannot
/// re-derive, reported through [`crate::ports::ResumeFrameSink`] as each one
/// settles and stored beside the engine checkpoint by the host.
///
/// Additive by construction: every field defaults, so a frame written by an
/// older build simply reads as "progress unknown" and the resume declines to
/// restore rather than guessing ([`PipelineResume::from_progress`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameProgress {
    /// Triage's settled class, or `None` before triage ran. A resume without
    /// it cannot know whether the run verifies, so it declines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_class: Option<TaskClass>,
    /// The full plan. The transcript cannot substitute: step prompts are
    /// pushed one turn at a time, so at a mid-plan kill the unreached steps
    /// exist nowhere else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Vec<PlanStep>>,
    /// Index of the plan step whose turn was in flight, so the resumed run
    /// continues *after* it rather than replaying the plan from the top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_step: Option<usize>,
    /// Whether the execute stage had begun. A kill before execution has
    /// nothing to resume into — the ordinary path re-runs from the start.
    #[serde(default)]
    pub executing: bool,
    /// Whether execution ran inside an isolated candidate workspace, which
    /// died with the process. `true` declines restoration (see module docs).
    #[serde(default)]
    pub isolated: bool,
    /// Pre-execution untracked-file fingerprints — the zero-diff guard's
    /// before-image, observed on a tree that no longer exists.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub untracked_before: HashMap<String, String>,
    /// The configured test command's pre-execution baseline observation,
    /// when one completed. This is what lets a resumed run's flip oracle
    /// keep its `Failing` precondition — see [`RecordedBaseline`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<RecordedBaseline>,
}

/// A completed pre-execution baseline run of the configured test command —
/// an observation of a tree state that cannot be re-observed after the
/// crash, because the tree now holds the turn's partial work.
///
/// Carrying it forward is faithful, not lenient: the observation really was
/// made, by the same run, on the genuinely pristine tree. Re-running the
/// command *now* and calling it a baseline would be the dishonest version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedBaseline {
    /// The normalized command the oracle tracks.
    pub command: String,
    /// Whether the baseline passed. Only a failing baseline can arm a flip.
    pub passed: bool,
    /// The run's output tail, kept for the same-failure rule (#867): a flip
    /// must fix the tests the baseline actually named.
    #[serde(default)]
    pub output_tail: String,
}

/// Everything [`Pipeline::resume`] needs, validated: constructing one proves
/// the frame carried enough to restore, so the seam itself has no
/// half-restorable states to reason about.
#[derive(Debug)]
pub struct PipelineResume {
    /// The interrupted turn, as the engine checkpointed it.
    pub checkpoint: Checkpoint,
    /// Triage's class, restored — gates verification exactly as it did live.
    pub task_class: TaskClass,
    /// The plan and the cursor into it, when the run had one.
    pub plan: Option<Vec<PlanStep>>,
    /// See [`FrameProgress::next_step`].
    pub next_step: Option<usize>,
    /// See [`FrameProgress::untracked_before`].
    pub untracked_before: HashMap<String, String>,
    /// See [`FrameProgress::baseline`].
    pub baseline: Option<RecordedBaseline>,
}

impl PipelineResume {
    /// Validate `progress` against what restoration requires; `None` means
    /// the caller keeps the honest bare-turn resume (with its advisory), it
    /// never means "restore approximately".
    ///
    /// Declines when the frame predates the progress record (`task_class`
    /// absent), when the kill happened before execution began (nothing to
    /// resume *into* — re-running is the ordinary path), or when execution
    /// was isolated in a candidate worktree that died with the process.
    pub fn from_progress(checkpoint: Checkpoint, progress: FrameProgress) -> Option<Self> {
        let task_class = progress.task_class?;
        if !progress.executing || progress.isolated {
            return None;
        }
        Some(Self {
            checkpoint,
            task_class,
            plan: progress.plan,
            next_step: progress.next_step,
            untracked_before: progress.untracked_before,
            baseline: progress.baseline,
        })
    }
}

impl<'a> Pipeline<'a> {
    /// Attach the sink that carries [`FrameProgress`] to the host's durable
    /// frame. Chained by the host's one construction path
    /// (`resume_frame::pipeline` in `stella-cli`) — a pipeline without one
    /// runs exactly as before and simply cannot be resumed into.
    #[must_use]
    pub fn with_frame_sink(mut self, sink: Arc<dyn crate::ports::ResumeFrameSink>) -> Self {
        self.frame_sink = Some(sink);
        self
    }

    /// Record one or more progress facts and push the whole record to the
    /// sink. Free when no sink is attached — the mutex is not even taken —
    /// so the recording sites cost nothing on hosts without durability.
    pub(super) fn record_progress(&self, update: impl FnOnce(&mut FrameProgress)) {
        let Some(sink) = &self.frame_sink else {
            return;
        };
        let snapshot = {
            let mut progress = self.progress.lock().unwrap_or_else(|p| p.into_inner());
            update(&mut progress);
            progress.clone()
        };
        sink.record(&snapshot);
    }

    /// Re-enter the pipeline over an interrupted execute-stage turn: finish
    /// the turn, walk any plan steps the crash never reached, then run the
    /// same witness → verify → verdict tail an ordinary candidate takes.
    ///
    /// The returned outcome's `total_cost_usd` includes the interrupted
    /// turn's pre-crash spend (the checkpoint carries it, and the resumed
    /// turn's meter continues from it) plus everything this call spends —
    /// the same accounting the bare resume path reports today.
    pub async fn resume(
        &self,
        goal: &str,
        resume: PipelineResume,
    ) -> Result<PipelineOutcome, PipelineRunError> {
        let mut total_cost = 0.0f64;
        if let Err(error) = &self.configured_test {
            return Err(PipelineRunError::new(
                PipelineError::InvalidTestCommand(error.to_string()),
                total_cost,
            ));
        }
        let task_class = resume.task_class;
        // Progress re-recorded on the resumed run's own frame, so a second
        // kill — mid-resume — restores exactly like the first one did.
        self.record_progress(|p| {
            p.task_class = Some(task_class);
            p.plan = resume.plan.clone();
            p.next_step = resume.next_step;
            p.executing = true;
            p.untracked_before = resume.untracked_before.clone();
            p.baseline = resume.baseline.clone();
        });

        let worker = match self.resolve_provider(Role::Worker) {
            Ok(worker) => worker,
            Err(error) => {
                return Err(PipelineRunError::new(
                    error.into_pipeline_error(),
                    total_cost,
                ));
            }
        };
        if let Some(fallback) = &worker.fallback {
            self.emit_fallback(fallback);
        }
        let worker_label = worker.model_ref.to_string();
        let mut engine = Engine::with_sleeper(
            worker.provider,
            self.tools,
            self.config.engine.clone(),
            self.sleeper,
        );
        if let Some((hooks, runner)) = self.hooks {
            engine = engine.with_hooks(hooks, runner);
        }
        if let Some(gate) = self.turn_gate {
            engine = engine.with_gate(gate);
        }
        // No steering view: a resume is headless by construction (the
        // interactive session it might have belonged to died with the
        // process), matching the goal and fleet pipelines.

        // The oracle keeps its recorded pre-execution baseline (see
        // `RecordedBaseline`) — matched against the *currently* configured
        // command, so a `--test-command` changed across the crash discards
        // the stale observation instead of crediting a flip on the wrong
        // test. With no usable baseline the oracle stays unarmed and the
        // ladder escalates to the model verifier rather than fast-submitting.
        let mut oracle = FlipOracle::new();
        let mut oracle_trace = Vec::new();
        let mut flip_halt: Option<Arc<FlipHalt>> = None;
        if task_class.verifies_unconditionally()
            && let Some(cmd) = self.effective_test_command(None)
            && let Some(recorded) = &resume.baseline
            && recorded.command == cmd.command
        {
            oracle.observe_run(cmd.command, recorded.passed, &recorded.output_tail);
            oracle_trace.push(OracleObservation {
                tree: ProofTree::Baseline,
                passed: recorded.passed,
            });
            self.emit_proof(ProofStep::Oracle {
                command: cmd.command.to_string(),
                passed: recorded.passed,
                tree: ProofTree::Baseline,
                run: None,
                runs_required: None,
                seed: None,
            });
            if !recorded.passed {
                flip_halt = Some(Arc::new(FlipHalt::new(cmd.command)));
            }
        }

        // --- Execute, re-entered mid-turn. ---------------------------------
        self.emit(AgentEvent::Stage {
            name: StageKind::Execute,
        });
        let mut signals = ChangeSignals::default();
        let (outcome, messages, mut budget) = self
            .resume_engine_turn(&engine, resume.checkpoint, &mut signals, flip_halt.clone())
            .await;

        let mut state = CandidateState {
            messages,
            final_text: String::new(),
            signals,
            flip_halt,
            oracle,
            oracle_trace,
            untracked_before: resume.untracked_before,
            diff_lines: 0,
            diff_text: String::new(),
            diff_available: true,
            touch_baseline: {
                // A fresh recorder counts from zero in this process, which is
                // exactly the resume point the signal-honesty note describes.
                self.touches.begin_workspace_probe();
                self.touches.mutations_recorded()
            },
            // The pre-execution lint snapshot died with the process; the
            // regression veto (#861) sits out a resumed run (module docs).
            lint_baseline: None,
            witness_mutation: None,
            diff_coverage: DiffCoverage::Unmeasured,
            revisions: 0,
            evidence_demands: 0,
            witness_paths: Vec::new(),
            witness_baseline_symptom: None,
            failures: Vec::new(),
            last_verdict: None,
            last_verdict_diff: None,
        };
        match outcome {
            TurnOutcome::Completed { text, cost_usd } => {
                state.final_text = text;
                total_cost += cost_usd;
            }
            TurnOutcome::Aborted {
                reason,
                kind,
                cost_usd,
            } => {
                total_cost += cost_usd;
                let best = CandidateResult::turn_aborted(state.messages, TurnAbort { reason, kind });
                return Ok(self.settle_outcome(
                    best,
                    task_class,
                    total_cost,
                    Some(worker_label),
                    1,
                ));
            }
        }

        // --- The plan steps the crash never reached. -----------------------
        if let (Some(plan), Some(next)) = (resume.plan.as_deref(), resume.next_step)
            && !plan_steps::goal_declared_complete(&state.final_text)
        {
            let remaining = plan.get(next + 1..).unwrap_or_default();
            if !remaining.is_empty() {
                let mut spend = Spend {
                    budget: &mut budget,
                    total: &mut total_cost,
                };
                if let Err(abort) = self
                    .run_plan_steps(remaining, next + 1, plan.len(), &engine, &mut spend, &mut state)
                    .await
                {
                    let best = CandidateResult::turn_aborted(state.messages, abort);
                    return Ok(self.settle_outcome(
                        best,
                        task_class,
                        total_cost,
                        Some(worker_label),
                        1,
                    ));
                }
            }
        }

        // --- The ordinary post-execute tail (mirrors `run_candidate`). -----
        let surface = CandidateSurface {
            diagnostics: self.diagnostics,
            tests: self.tests,
            lint: self.lint,
            mutation: self.mutation,
            coverage: self.coverage,
            repo_status: self.repo_status,
            cwd: None,
            hook_runner: None,
            workspace: None,
        };
        let probe = self.gather_diff(surface, &state.untracked_before).await;
        self.absorb_probe(&mut state, probe);
        // Signal honesty (module docs): the diff is whole, the tallies are
        // not — when the diff shows work, credit at least one mutating
        // action so the warrant and the no-op rung lean toward verifying.
        if !state.diff_text.trim().is_empty() && state.signals.mutating_actions == 0 {
            state.signals.mutating_actions = 1;
        }
        let files_touched = state.signals.file_changes > 0
            || (state.signals.mutating_actions > 0 && !state.diff_text.trim().is_empty());
        let should_verify = task_class.verifies_unconditionally()
            || (task_class == TaskClass::SimpleLookup && files_touched);
        if !should_verify {
            let best = state.into_unverified();
            return Ok(self.settle_outcome(best, task_class, total_cost, Some(worker_label), 1));
        }

        let warrant = warrant(&state.diff_text, state.signals);
        self.emit_proof(ProofStep::Warrant {
            required: warrant.is_required(),
            reason: warrant.reason().map(|r| r.sentence().to_string()),
            diff_lines: state.diff_lines,
        });

        let base_messages: Vec<CompletionMessage> = Vec::new();
        let frame = TaskFrame {
            goal,
            base_messages: &base_messages,
            plan: resume.plan.as_deref(),
            assessment: TaskAssessment::from_class(task_class),
        };
        let best = {
            let mut spend = Spend {
                budget: &mut budget,
                total: &mut total_cost,
            };
            // `authoring: None` — no pristine pre-execution snapshot exists
            // to author in (module docs), the same degradation every
            // shared-tree candidate already takes.
            let witness = match self
                .witness_on_demand(goal, None, surface, &mut state, &mut spend)
                .await
            {
                Ok(witness) => witness,
                Err(reason) => {
                    let best = CandidateResult::aborted(
                        state.messages,
                        reason,
                        AbortKind::DeliberateStop,
                    );
                    return Ok(self.settle_outcome(
                        best,
                        task_class,
                        total_cost,
                        Some(worker_label),
                        1,
                    ));
                }
            };
            self.verify_candidate(frame, witness.as_ref(), &engine, surface, &mut spend, state)
                .await
        };
        Ok(self.settle_outcome(best, task_class, total_cost, Some(worker_label), 1))
    }

    /// The `--- 6. Complete ---` projection shared by [`Pipeline::run`] and
    /// [`Pipeline::resume`]: fold a settled candidate into the terminal
    /// events and the [`PipelineOutcome`]. `best.messages` is the caller's to
    /// adopt *before* calling — this consumes the rest.
    pub(super) fn settle_outcome(
        &self,
        best: CandidateResult,
        task_class: TaskClass,
        total_cost: f64,
        worker_model_label: Option<String>,
        candidates_run: u32,
    ) -> PipelineOutcome {
        if let Some(abort) = best.aborted {
            // One abort is one `error` event. A turn-originated abort already
            // crossed the bus inside the worker turn (or, for a soft stop /
            // host cancel, deliberately did not — a decision is not a
            // failure); only a pipeline-originated abort still owes the
            // stream its event (#1524).
            if !abort.from_turn {
                self.emit(AgentEvent::Error {
                    message: abort.reason.clone(),
                    retryable: false,
                });
            }
            return PipelineOutcome {
                status: PipelineStatus::Aborted {
                    reason: abort.reason,
                    kind: abort.kind,
                },
                task_class,
                final_text: best.final_text,
                total_cost_usd: total_cost,
                verdict: best.verdict,
                score: Some(best.score),
                revisions: best.revisions,
                candidates_run,
            };
        }

        self.emit(AgentEvent::Stage {
            name: StageKind::Complete,
        });
        let status = match &best.verdict {
            Some(verdict) if !verdict.passed => PipelineStatus::VerificationFailed {
                verdict: verdict.clone(),
            },
            _ => PipelineStatus::Completed,
        };
        match &status {
            PipelineStatus::Completed => self.emit(AgentEvent::Complete {
                // The label is `None` only when the candidate path returned
                // before it resolved a worker (a setup abort that then
                // degraded to a bare run). Re-resolve rather than emit
                // `Complete { model: "" }` — a terminal event that names no
                // model reads to every consumer as "no model ran", which is
                // exactly backwards on a path that did the work.
                model: worker_model_label
                    .or_else(|| {
                        self.resolve_provider(Role::Worker)
                            .ok()
                            .map(|worker| worker.model_ref.to_string())
                    })
                    .unwrap_or_default(),
                cost_usd: total_cost,
            }),
            PipelineStatus::VerificationFailed { verdict } => {
                self.emit(AgentEvent::Error {
                    message: format!("verification failed: {}", verdict.summary),
                    retryable: false,
                });
            }
            PipelineStatus::Aborted { .. } => {
                unreachable!("aborted candidates return before terminal verification")
            }
        }
        PipelineOutcome {
            status,
            task_class,
            final_text: best.final_text,
            total_cost_usd: total_cost,
            verdict: best.verdict,
            score: Some(best.score),
            revisions: best.revisions,
            candidates_run,
        }
    }
}
