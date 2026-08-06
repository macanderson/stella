//! Candidate-local witness authoring for the isolated typed-execution
//! boundary: the `Pipeline::witness_stage` turn (author → one bounded repair
//! → artifact/invocation/identity acceptance) plus the candidate-bound hook
//! runner it shares with the execute engine. Split out of `pipeline.rs`
//! because it is the one stage that runs against the *witness* tool executor
//! rather than the worker's.

use super::*;

use crate::witness::{RUNNER_VOCABULARY, WITNESS_SYSTEM_PROMPT, runner_probe};

/// Which of the closed vocabulary's runners this workspace can actually
/// spawn (#1539) — the fact the capability-minimal author cannot discover
/// itself. One version-style probe per vocabulary program, through the
/// baseline's own [`TestRunner`] port; a port that cannot check answers
/// `true` per its default, so this only ever narrows on real evidence.
///
/// Probed concurrently: the vocabulary is thirteen programs, each an awaited
/// process spawn, and a best-of-N fan-out pays this per candidate — serially
/// that was thirteen spawn round-trips of pure latency before any authoring
/// could start. Order is restored from the vocabulary, not the completion
/// order, so the prompt's listing stays deterministic (invariant 7).
async fn runner_availability(tests: &dyn TestRunner) -> Vec<String> {
    let probes = RUNNER_VOCABULARY.iter().filter_map(|program| {
        let probe = runner_probe(program)?;
        Some(async move { (*program, tests.runner_available(&probe).await) })
    });
    futures_util::future::join_all(probes)
        .await
        .into_iter()
        .filter(|(_, available)| *available)
        .map(|(program, _)| program.to_string())
        .collect()
}

/// Overlay one role's request shaping onto an engine config — the pure half
/// of [`Pipeline::witness_engine_config`], so the field-by-field flow is
/// testable without a pipeline. Only the knobs `EngineConfig` itself carries
/// participate; `RoleCallOverrides::prompt` is a raw-call concern and is
/// deliberately absent (see the caller's doc for why).
fn apply_role_shaping(mut config: EngineConfig, overrides: &RoleCallOverrides) -> EngineConfig {
    if let Some(effort) = overrides.effort {
        config.effort = Some(effort);
    }
    if let Some(reasoning) = overrides.reasoning {
        config.reasoning = Some(reasoning);
    }
    if let Some(temperature) = overrides.temperature {
        config.temperature = Some(temperature);
    }
    if let Some(max_output_tokens) = overrides.max_output_tokens {
        config.max_output_tokens = Some(max_output_tokens);
    }
    if let Some(params) = &overrides.params {
        config.params = Some(*params);
    }
    config
}

/// Candidate-bound hook execution: both the hook process and the engine's
/// payload use the isolated root, never the session root.
pub(super) struct BoundHookRunner<'a> {
    pub(super) inner: &'a dyn HookRunner,
    pub(super) cwd: &'a str,
}

#[async_trait::async_trait]
impl HookRunner for BoundHookRunner<'_> {
    async fn run(
        &self,
        action: &stella_core::hooks::HookAction,
        payload_json: &str,
        _cwd: &str,
    ) -> Result<stella_core::hooks::HookExecResult, stella_core::hooks::HookExecError> {
        self.inner.run(action, payload_json, self.cwd).await
    }
}

/// Why an authored witness could not be produced or accepted, and whether the
/// pipeline may degrade past it.
///
/// `degradable` = the witness simply couldn't be AUTHORED (no test command,
/// an unusable command, a test that proves nothing, the author engine getting
/// stuck). The task needs no witness to proceed, so the run degrades to a
/// bare worker turn rather than dying. NOT degradable = a resource limit
/// (budget) or an artifact-INTEGRITY violation (the author modified tracked
/// files, produced a non-single-file / symlink artifact, or a runner/identity
/// mismatch) — fail-closed decisions that surface the problem rather than
/// silently completing unverified.
pub(super) struct WitnessAbort {
    pub(super) reason: String,
    pub(super) degradable: bool,
}

impl WitnessAbort {
    pub(super) fn degradable(reason: String) -> Self {
        Self {
            reason,
            degradable: true,
        }
    }
    pub(super) fn rejected(reason: String) -> Self {
        Self {
            reason,
            degradable: false,
        }
    }
}

/// Everything one candidate needs to buy itself a witness *after* it has
/// executed and the warrant has read the resulting diff.
///
/// Carried as one optional value rather than three loose parameters so the
/// invariant is structural: a candidate either can author (isolation port,
/// resolved independent author, and the frames to prompt with — all present)
/// or cannot (`None`). There is no state where it holds a port but no author.
#[derive(Clone, Copy)]
pub(super) struct WitnessAuthoring<'c> {
    /// Snapshots the *pre-execution* tree for the author to work blind in.
    /// The same port that made the executing candidate, called a second time.
    pub(super) port: &'c dyn CandidateWorkspacePort,
    /// The independent author — resolved as a different model from the worker.
    pub(super) author: &'c ResolvedRole<'c>,
    pub(super) frames: &'c [RecalledFrame],
}

impl<'a> Pipeline<'a> {
    /// Record that a warranted witness could not be produced: the work stands
    /// and is **unproven**.
    ///
    /// Both channels, deliberately. The warning keeps the transcript's prose
    /// account of a degradation the user should see; the proof step puts the
    /// same fact on the rail, where "warranted, and not obtained" is a row that
    /// stays visible instead of a line that scrolls. A caller that emitted only
    /// the warning would leave the rail with no statement of its own, falling
    /// back to "not reported" when the reason was known all along.
    ///
    /// Use this, never a bare [`Self::warn`], for any path that declines or
    /// fails to produce a witness the turn asked for. The rail's backstop will
    /// keep such a turn honest either way — but "not reported" is what the
    /// surface says when it has nothing better, and a reason it could have
    /// named is strictly better.
    pub(super) fn unproven(&self, reason: String) {
        self.warn(format!("continuing without an authored witness: {reason}"));
        self.emit_proof(stella_protocol::ProofStep::WitnessUnavailable { reason });
    }

    /// Record that verification could not be *performed*: every evidence
    /// channel was blind, so the ladder abstained (#973).
    ///
    /// Both channels, for the same reason [`Self::unproven`] uses both. The
    /// warning is the prose account; the proof step is what keeps the rail's
    /// verdict row from reading `✓ passed` on a turn nothing observed — the
    /// abstention still emits a `Verdict`, and its `passed: true` means
    /// only "nothing failed this", never "something proved it".
    pub(super) fn unverifiable(&self, reason: &str) {
        self.warn(format!("verification could not be performed: {reason}"));
        self.emit_proof(stella_protocol::ProofStep::VerificationUnavailable {
            reason: reason.to_string(),
        });
    }

    /// The witness author/repair engine tuning (#1785): the worker's engine
    /// config rebased into the authoring snapshot, with the VERIFIER's
    /// request shaping applied on top. The witness author rides the
    /// verifier's model (`Role::Verifier`, independence-filtered), so an
    /// operator tuning `agents.verifier.effort` or `.temperature` reasonably
    /// expects the witness author to honor it — before this, those knobs
    /// reached the verdict and guidance calls while the author silently ran
    /// on the worker's tuning.
    ///
    /// `RoleCallOverrides::prompt` is deliberately NOT applied here: it is
    /// operator prose written against the reviewer instructions, and
    /// injecting it into a test-authoring engine would steer the wrong role.
    /// It stays scoped to the raw verdict/guidance calls
    /// (`metered_raw_call`).
    pub(super) fn witness_engine_config(&self, surface: CandidateSurface<'_>) -> EngineConfig {
        apply_role_shaping(
            self.engine_config_for(surface),
            &self.config.role_overrides.verifier,
        )
    }

    pub(super) fn engine_config_for(&self, surface: CandidateSurface<'_>) -> EngineConfig {
        let mut config = self.config.engine.clone();
        if let Some(cwd) = surface.cwd {
            config.cwd = cwd.to_string();
            // An ISOLATED candidate does not write the session's resume point.
            //
            // The sink is attached once, for every role at once, in
            // `stella-cli`'s engine tuning — which is right for the session's
            // own turns and wrong for a candidate running in a throwaway
            // snapshot. Its transcript describes edits at paths inside that
            // snapshot, and every candidate but the winner has its workspace
            // removed, so resuming from one would hand the model a conversation
            // asserting it edited files that no longer exist. A losing
            // candidate's transcript is not this session's resume point at all.
            //
            // Only isolated candidates are cleared. `run_shared_candidates`
            // works the session tree with no workspace of its own (`cwd: None`)
            // and is the ordinary single-shot path, so it keeps checkpointing —
            // which is the case that actually needs to survive a crash.
            config.checkpoint_sink = None;
        }
        config
    }

    /// Author one failing witness in `baseline` — a pristine snapshot of the
    /// tree as it stood *before* execution — and graft the artifact it accepts
    /// into `candidate`, the tree that actually did the work.
    ///
    /// # Why two trees
    ///
    /// This stage runs *after* execution, so that the warrant
    /// ([`crate::witness::warrant`]) can read the diff and skip authoring
    /// entirely when the change has nothing to prove. That ordering creates a
    /// hazard the pre-execution version did not have: by now an implementation
    /// exists, and an author allowed to read it will write a test that restates
    /// it. Such a test still fails on the old code and passes on the new, so the
    /// flip oracle would confirm it — while proving only that the code equals
    /// itself.
    ///
    /// The fix is to keep the author's *input* byte-identical to what it was
    /// when this stage ran first: the goal, the recalled frames, and the
    /// unmodified tree. `baseline` is a second [`CandidateWorkspacePort::create`]
    /// snapshot, which sees the pre-execution tree because a candidate's edits
    /// never leave its own workspace until adoption. So the author is blind to
    /// the implementation, and moving this call later buys cost, never leverage.
    ///
    /// `baseline` is also where the artifact must FAIL, which is what makes the
    /// later pass in `candidate` a genuine fail→pass flip across two code
    /// states rather than one tree observed twice.
    pub(super) async fn witness_stage(
        &self,
        goal: &str,
        authoring: WitnessAuthoring<'_>,
        baseline: CandidateSurface<'_>,
        candidate: CandidateSurface<'_>,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<Option<Witness>, WitnessAbort> {
        // `port` is deliberately not used here: the caller has already spent it
        // to make `baseline`, and a second snapshot from inside this stage would
        // be a tree nothing is proved against.
        let WitnessAuthoring { author, frames, .. } = authoring;
        self.emit(AgentEvent::Stage {
            name: StageKind::Witness,
        });

        // #1539: establish runner availability BEFORE spending a model turn.
        // The author cannot execute anything, so a missing toolchain is
        // invisible to it — it used to guess, and a wrong guess produced an
        // unobservable baseline run that silently cost the run its witness.
        // An empty set means no vocabulary runner is usable here: degrade
        // now, honestly, with zero author spend.
        let available = runner_availability(baseline.tests).await;
        if available.is_empty() {
            return Err(WitnessAbort::degradable(
                "no supported test runner is available in this workspace, so an authored \
                 witness could never be observed"
                    .to_string(),
            ));
        }

        let tracked_before = baseline.repo_status.tracked_fingerprints().await;
        let untracked_before = baseline.repo_status.untracked_fingerprints().await;
        let structure = self.repo.structure_summary().await;
        let baseline_workspace = baseline
            .workspace
            .expect("witness authoring requires a pristine baseline workspace");
        let witness_tools = baseline_workspace.witness_tools();
        let mut engine = Engine::with_sleeper(
            author.provider,
            witness_tools,
            self.witness_engine_config(baseline),
            self.sleeper,
        )
        .with_call_role(stella_protocol::ModelCallRole::WitnessAuthor);
        if let Some(gate) = self.turn_gate {
            engine = engine.with_gate(gate);
        }

        let mut messages = vec![
            CompletionMessage::system(WITNESS_SYSTEM_PROMPT),
            // No project-test-command anchor rides here, structurally: this
            // stage is only reachable when `config.test_command` is `None`
            // (a configured command makes the pipeline its own oracle and
            // authors nothing), so there is never a configured command to
            // offer. The probed availability set is the toolchain fact the
            // author gets instead.
            CompletionMessage::user(witness_prompt(goal, frames, &structure, &available)),
        ];
        // Both tallies are local and discarded, on the same reasoning: they
        // measure the *witness author*, and the ladder's channels are about
        // what the worker did toward the goal. A test file written here is not
        // the change under verification, and counting it as a mutating action
        // would let an authored witness disguise a worker that never acted.
        let mut file_changes = 0u32;
        let mut mutating_actions = 0u32;
        let mut opaque_actions = 0u32;
        let text = match self
            .run_engine_turn(
                &engine,
                &mut messages,
                budget,
                &mut file_changes,
                &mut mutating_actions,
                &mut opaque_actions,
                // No flip halt: this turn is the witness AUTHOR writing the
                // test, not the worker fixing the code. Stopping it because
                // the tracked command went green would abandon the artifact
                // mid-write.
                None,
            )
            .await
        {
            TurnOutcome::Completed { text, cost_usd } => {
                *total += cost_usd;
                text
            }
            TurnOutcome::Aborted {
                reason, cost_usd, ..
            } => {
                *total += cost_usd;
                if let Some(abort) = budget_abort(budget.evaluate()) {
                    return Err(WitnessAbort::rejected(abort.reason));
                }
                return Err(WitnessAbort::degradable(format!(
                    "witness author turn aborted: {reason}"
                )));
            }
        };
        let Some(mut command) = parse_witness_command(&text) else {
            return Err(WitnessAbort::degradable(
                "witness author produced no TEST_COMMAND line".to_string(),
            ));
        };

        let Ok(mut invocation) = parse_test_invocation(&command) else {
            return Err(WitnessAbort::degradable(format!(
                "witness author produced an unsafe or unsupported test command `{command}`"
            )));
        };
        // #1539: the availability constraint is enforced, not advisory. An
        // author that names an unprobed runner anyway degrades HERE, with the
        // real reason — never by spending a baseline run to discover an
        // unobservable command and blaming generic infra.
        if !available.contains(&invocation.program) {
            return Err(WitnessAbort::degradable(format!(
                "witness author chose `{}`, which is not available in this workspace \
                 (available runners: {})",
                invocation.program,
                available.join(", ")
            )));
        }
        // #860: the "must fail first" gate needs a *completed* baseline run.
        // A timed-out or unspawnable run observed no assertion, so it can
        // neither arm the witness (a "failure" that was infra noise would
        // fabricate the oracle's Failing precondition) nor justify a repair
        // turn (the author cannot fix a toolchain). Degrade without a witness
        // and say why — cheaper and honest.
        let first_baseline = self.run_test_observed(baseline.tests, &invocation).await;
        if let Some(label) = first_baseline.infra_label() {
            return Err(WitnessAbort::degradable(format!(
                "witness baseline run was {label}; no assertion was observed, so a failing \
                 baseline cannot be established"
            )));
        }
        // The failing run's output is part of the witness: it carries the
        // failing test names that arm the oracle's same-failure rule (#867)
        // when this observation is transplanted into the candidate's oracle.
        // Combined the same way every other oracle observation is — some
        // runners report through stdout, some through stderr.
        let mut baseline_output = format!(
            "{}\n{}",
            first_baseline.stdout_tail, first_baseline.stderr_tail
        );
        if first_baseline.passed() {
            messages.push(CompletionMessage::user(witness_repair_prompt(&command)));
            let mut repair_engine = Engine::with_sleeper(
                author.provider,
                witness_tools,
                self.witness_engine_config(baseline),
                self.sleeper,
            )
            .with_call_role(stella_protocol::ModelCallRole::WitnessRepair);
            if let Some(gate) = self.turn_gate {
                repair_engine = repair_engine.with_gate(gate);
            }
            let repaired = match self
                .run_engine_turn(
                    &repair_engine,
                    &mut messages,
                    budget,
                    &mut file_changes,
                    &mut mutating_actions,
                    &mut opaque_actions,
                    // Witness repair, same reasoning as the author turn.
                    None,
                )
                .await
            {
                TurnOutcome::Completed { text, cost_usd } => {
                    *total += cost_usd;
                    text
                }
                TurnOutcome::Aborted {
                    reason, cost_usd, ..
                } => {
                    *total += cost_usd;
                    if let Some(abort) = budget_abort(budget.evaluate()) {
                        return Err(WitnessAbort::rejected(abort.reason));
                    }
                    return Err(WitnessAbort::degradable(format!(
                        "witness repair turn aborted: {reason}"
                    )));
                }
            };
            // A repair reply with no marker line used to fall back to the
            // command that had JUST passed — deterministically re-running it,
            // re-observing the pass, and paying a test run to learn nothing.
            // The absent marker already is the answer: degrade now.
            command = match parse_witness_command(&repaired) {
                Some(repaired_command) => repaired_command,
                None => {
                    return Err(WitnessAbort::degradable(
                        "witness repair produced no TEST_COMMAND line".to_string(),
                    ));
                }
            };
            let Ok(repaired_invocation) = parse_test_invocation(&command) else {
                return Err(WitnessAbort::degradable(format!(
                    "witness repair produced an unsafe or unsupported test command `{command}`"
                )));
            };
            // Same #1539 constraint on the repaired command: the repair turn
            // may change the runner, and an unavailable one is just as
            // unobservable the second time.
            if !available.contains(&repaired_invocation.program) {
                return Err(WitnessAbort::degradable(format!(
                    "witness repair chose `{}`, which is not available in this workspace \
                     (available runners: {})",
                    repaired_invocation.program,
                    available.join(", ")
                )));
            }
            invocation = repaired_invocation;
            let repaired_baseline = self.run_test_observed(baseline.tests, &invocation).await;
            if let Some(label) = repaired_baseline.infra_label() {
                return Err(WitnessAbort::degradable(format!(
                    "witness baseline run after repair was {label}; no assertion was observed"
                )));
            }
            if repaired_baseline.passed() {
                return Err(WitnessAbort::degradable(
                    "witness test still passes on the unmodified code after one repair".to_string(),
                ));
            }
            baseline_output = format!(
                "{}\n{}",
                repaired_baseline.stdout_tail, repaired_baseline.stderr_tail
            );
        }

        let tracked_after = baseline.repo_status.tracked_fingerprints().await;
        let untracked_after = baseline.repo_status.untracked_fingerprints().await;
        let fingerprints = validate_witness_artifact(
            &tracked_before,
            &tracked_after,
            &untracked_before,
            &untracked_after,
        )
        .map_err(|error| match error {
            // No file at all is a cannot-author condition, not an integrity
            // violation: the worker's change is real and already done, and
            // this stage's own contract says a witness that cannot be
            // PRODUCED must not throw it away. Everything else — tracked
            // mutations, wrong or multiple files — stays fail-closed.
            crate::witness::WitnessArtifactError::NothingCreated => {
                WitnessAbort::degradable(error.to_string())
            }
            _ => WitnessAbort::rejected(error.to_string()),
        })?;
        let path = fingerprints
            .keys()
            .next()
            .expect("validated witness artifact contains exactly one path");
        validate_witness_invocation(path, &invocation)
            .map_err(|error| WitnessAbort::rejected(error.to_string()))?;
        // Accept the artifact where it was written before copying it anywhere:
        // a symlink or multiply-linked file must be rejected in the snapshot
        // that produced it, never resolved by a copy.
        let authored = baseline.repo_status.artifact_identity(path).await;
        validate_witness_identity(path, &fingerprints[path], authored.as_ref())
            .map_err(|error| WitnessAbort::rejected(error.to_string()))?;

        // Cross into the tree that executed. A failure here is degradable: the
        // candidate's work is untouched and already done, so it falls through
        // to the unauthored ladder rather than being discarded for want of
        // scaffolding.
        candidate
            .workspace
            .expect("witness grafting requires the candidate workspace")
            .graft_witness(baseline_workspace.root(), path)
            .await
            .map_err(|error| WitnessAbort::degradable(error.to_string()))?;

        // Re-pin against the *grafted* copy. Tamper exclusion runs against the
        // candidate's repo status for the rest of the run, so the baseline it
        // compares to has to be the identity of the bytes that will actually
        // execute — not the identity of the original in a snapshot that is
        // about to be deleted.
        let identity = candidate.repo_status.artifact_identity(path).await;
        validate_witness_identity(path, &fingerprints[path], identity.as_ref())
            .map_err(|error| WitnessAbort::rejected(error.to_string()))?;
        let identity = identity.expect("validated witness identity is present");
        // Announced only here, past every integrity check: the rail's "witness
        // authored" row must mean the artifact was accepted AND pinned, never
        // that a model produced a file.
        self.emit_proof(stella_protocol::ProofStep::WitnessAuthored {
            path: path.clone(),
            command: command.clone(),
            fingerprint: identity.fingerprint.clone(),
        });
        let files = HashMap::from([(path.clone(), identity)]);
        Ok(Some(Witness {
            command,
            invocation,
            files,
            baseline_output,
        }))
    }

    /// Author a witness for this candidate only if its diff warrants one.
    ///
    /// The saving this exists for: [`crate::witness::warrant`] answers from the
    /// change itself, so a docs edit, a comment fix, or a turn that touched
    /// nothing returns here having spent zero model calls. Before authoring
    /// moved after execution, that same run paid for an author turn (and
    /// sometimes a repair turn) to produce a test for a change with nothing to
    /// prove, and only afterwards discovered the warrant.
    ///
    /// `Err` is reserved for fail-closed integrity rejections. Everything else
    /// — no isolation port for the baseline, an author that got stuck, an
    /// artifact that could not be grafted — returns `Ok(None)` and lets the
    /// candidate finish on the unauthored ladder. That asymmetry is new and
    /// deliberate: the work is already done by the time this runs, so a witness
    /// that cannot be *produced* must not throw away a real change, while a
    /// witness that cannot be *trusted* must still stop the run.
    pub(super) async fn witness_on_demand(
        &self,
        goal: &str,
        authoring: Option<WitnessAuthoring<'_>>,
        surface: CandidateSurface<'_>,
        state: &mut CandidateState,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<Option<Witness>, String> {
        let Some(authoring) = authoring else {
            return Ok(None);
        };
        if !warrant(&state.diff_text, state.change_signals()).is_required() {
            // No Witness stage is emitted, because none runs. The reason is
            // not lost: `warranted_completion` reads the same warrant during
            // verification and records the sentence on the verdict, which is
            // the run's half of "a witness test, or a stated reason there
            // isn't one".
            return Ok(None);
        }

        // A second snapshot of the same untouched tree. The candidate's edits
        // live only in the candidate's own workspace until adoption, so this
        // sees exactly the pre-execution state the author used to be given.
        let baseline = match authoring.port.create().await {
            Ok(baseline) => baseline,
            Err(e) => {
                self.unproven(format!("no pristine baseline to author against: {e}"));
                return Ok(None);
            }
        };
        let baseline_surface = CandidateSurface {
            diagnostics: baseline.diagnostics(),
            tests: baseline.tests(),
            // The authoring snapshot never fast-submits, so the regression
            // veto has nothing to audit here.
            lint: None,
            mutation: None,
            coverage: None,
            repo_status: baseline.repo_status(),
            cwd: Some(baseline.root()),
            // No hooks in the baseline: nothing there is the user's work, and
            // an authoring snapshot must not fire the project's lifecycle.
            hook_runner: None,
            workspace: Some(baseline.as_ref()),
        };
        let authored = self
            .witness_stage(goal, authoring, baseline_surface, surface, budget, total)
            .await;
        baseline.remove().await;

        let witness = match authored {
            Ok(Some(witness)) => witness,
            Ok(None) => return Ok(None),
            Err(abort) if abort.degradable => {
                self.unproven(abort.reason.clone());
                return Ok(None);
            }
            Err(abort) => return Err(abort.reason),
        };

        // The artifact failed in the baseline — `witness_stage` cannot return
        // it otherwise — and that snapshot was created for this candidate from
        // this candidate's own pre-execution tree. So this records an
        // observation that was made, not one that is assumed. `observe_run`
        // rather than `observe`: the failing tail carries the test names that
        // arm the same-failure rule (#867), which the name-blind `observe`
        // left permanently inert for authored witnesses — a flip could be
        // credited to a pass whose listing never contained the baseline's
        // failing test.
        state
            .oracle
            .observe_run(&witness.command, false, &witness.baseline_output);
        // The graft added an untracked file after the pre-execution snapshot
        // was taken. Enrolling it as pre-existing keeps every later diff
        // gather (each revision re-gathers) from reading the scaffolding as
        // the worker's work — the same exclusion authoring-before-execute got
        // for free by writing the file before this map was built.
        for (path, identity) in &witness.files {
            state
                .untracked_before
                .insert(path.clone(), identity.fingerprint.clone());
        }
        state.witness_paths = witness.files.keys().cloned().collect();
        Ok(Some(witness))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1785's witness: the verifier's request shaping reaches the witness
    /// engines field by field, `None` overrides leave the worker's values
    /// standing, and `prompt` has no channel here at all (it is not an
    /// `EngineConfig` knob — the type system keeps it raw-call-scoped).
    #[test]
    fn verifier_shaping_overlays_the_worker_engine_config() {
        let mut worker = EngineConfig::default();
        worker.temperature = Some(0.7);
        worker.max_output_tokens = Some(1000);
        worker.effort = None;

        let shaped = apply_role_shaping(
            worker.clone(),
            &RoleCallOverrides {
                temperature: Some(0.1),
                effort: Some(stella_protocol::ReasoningEffort::High),
                ..RoleCallOverrides::default()
            },
        );
        assert_eq!(shaped.temperature, Some(0.1));
        assert_eq!(shaped.effort, Some(stella_protocol::ReasoningEffort::High));
        assert_eq!(
            shaped.max_output_tokens,
            Some(1000),
            "an absent override must leave the worker's value standing"
        );

        let untouched = apply_role_shaping(worker.clone(), &RoleCallOverrides::default());
        assert_eq!(untouched.temperature, worker.temperature);
        assert_eq!(untouched.max_output_tokens, worker.max_output_tokens);
    }
}
