//! Post-execution observation probes for [`super::Pipeline`] — the
//! touched-tests observation that feeds the flip oracle (#860 typed
//! outcomes) and the lint delta behind the regression veto (#861).
//! A child module of `pipeline` so the private candidate types stay
//! reachable, split out to keep the orchestrator under the size gate.

use super::*;

impl<'a> Pipeline<'a> {
    /// Run one test invocation, retrying an out-of-memory kill (#1294).
    ///
    /// **The** entry point for running a test command anywhere in this crate —
    /// the baseline, the witness baseline, the post-execute observation and
    /// the pre-submit confirmation all go through here, so "an OOM kill is
    /// retried" is one fact rather than four call sites that have to remember
    /// it.
    ///
    /// Retry is the correct response precisely because an OOM kill is *not*
    /// evidence: the run observed no assertion, so there is nothing to feed
    /// back to a worker and nothing to revise. It is also the one
    /// unobservable outcome where a second attempt can plausibly differ — a
    /// timeout re-times-out on the same deadline, a missing toolchain stays
    /// missing, but memory pressure is a property of the moment.
    ///
    /// Bounded by `PipelineConfig::test_oom_retries` and *only* retried while
    /// the outcome is still an OOM: a retry that completes (pass or fail), or
    /// that fails some other way, is returned immediately. The last attempt's
    /// outcome is what the caller sees, so a run that OOMs every time still
    /// reports `out_of_memory` — never a fabricated pass or fail.
    pub(super) async fn run_test_observed(
        &self,
        tests: &dyn TestRunner,
        invocation: &TestInvocation,
    ) -> CmdOutcome {
        let mut outcome = tests.run_test(invocation).await;
        let mut retries = self.config.test_oom_retries;
        while outcome.is_out_of_memory() && retries > 0 {
            retries -= 1;
            outcome = tests.run_test(invocation).await;
        }
        outcome
    }

    /// The regression veto's evidence (#861): `(new errors, new warnings,
    /// sample)` of lint/typecheck records the candidate introduced over its
    /// pre-execution baseline. `(0, 0, "")` whenever either snapshot is
    /// unavailable — the veto can only withhold a fast-submit, so it always
    /// degrades open rather than fabricating a regression.
    ///
    /// The baseline comes from [`CandidateState::lint_baseline`] on the
    /// in-place path (whose pre-execution tree no longer exists) and from a
    /// lazy read of the still-pristine session tree for isolated candidates.
    /// Cost: called only from the pre-submit audit, so a run that never
    /// reaches a fast-submit runs no lint at all.
    pub(super) async fn lint_delta(
        &self,
        surface: CandidateSurface<'_>,
        state: &CandidateState,
    ) -> (u32, u32, String) {
        let Some(probe) = surface.lint else {
            return (0, 0, String::new());
        };
        let Some(after) = probe.snapshot(surface.cwd).await else {
            return (0, 0, String::new());
        };
        let baseline_owned;
        let baseline: &[LintRecord] = match (&state.lint_baseline, surface.workspace) {
            (Some(records), _) => records,
            // Isolated candidate: the session tree is still pre-execution,
            // so its diagnostics ARE the baseline, read only now that a
            // fast-submit hangs on the answer.
            (None, Some(_)) => match probe.snapshot(None).await {
                Some(records) => {
                    baseline_owned = records;
                    &baseline_owned
                }
                None => return (0, 0, String::new()),
            },
            (None, None) => return (0, 0, String::new()),
        };
        let known: HashSet<&LintRecord> = baseline.iter().collect();
        let mut new_errors = 0u32;
        let mut new_warnings = 0u32;
        let mut sample = String::new();
        for record in &after {
            if known.contains(record) {
                continue;
            }
            if record.error {
                new_errors += 1;
                // Token discipline: the verifier gets at most three new-error
                // lines, each capped — enough to see WHAT regressed without
                // shipping a linter's whole opinion into the prompt.
                if sample.lines().count() < 3 {
                    let code = record.code.as_deref().unwrap_or("-");
                    let mut line = format!("{}: {} [{}]", record.file, record.message, code);
                    line.truncate(200);
                    sample.push_str(&line);
                    sample.push('\n');
                }
            } else {
                new_warnings += 1;
            }
        }
        (new_errors, new_warnings, sample)
    }

    /// Post-execute test observation for the flip oracle + the touched-tests
    /// signal: `(Some(passed), stderr tail, None)` when a test command ran to
    /// completion, `(None, "", None)` when there is nothing to run, and
    /// `(None, tail, Some(label))` when the run was infra noise (#860) — a
    /// timeout or spawn failure observed no assertion, so it is inconclusive
    /// (verifier), not a deterministic red (revise). The label reaches the verifier
    /// evidence so "the suite timed out" is never read as "the suite failed".
    pub(super) async fn observe_touched_tests(
        &self,
        surface: CandidateSurface<'_>,
        cmd: Option<EffectiveTestCommand<'_>>,
        state: &mut CandidateState,
    ) -> (Option<bool>, String, Option<&'static str>) {
        match cmd {
            Some(cmd) => {
                let post = self.run_test_observed(surface.tests, cmd.invocation).await;
                match post.assertion_result() {
                    Some(passed) => {
                        // Output rides along for the same-failure rule
                        // (#867): a pass that names its tests but not the
                        // baseline's failures earns no flip.
                        let output = format!("{}\n{}", post.stdout_tail, post.stderr_tail);
                        state.oracle.observe_run(cmd.command, passed, &output);
                        state.oracle_trace.push(OracleObservation {
                            tree: ProofTree::Candidate,
                            passed,
                        });
                        self.emit_proof(ProofStep::Oracle {
                            command: cmd.command.to_string(),
                            passed,
                            tree: ProofTree::Candidate,
                            run: None,
                            runs_required: None,
                            seed: None,
                        });
                        // The same combined output the oracle saw: this tail
                        // becomes `SealedFailure::output`, the sole input to
                        // the failure fingerprint and the symptom class.
                        // Runners like `cargo test` put the assertion diff on
                        // STDOUT, so a stderr-only tail made two different
                        // failures fingerprint as repeats — tightening
                        // disclosure on a worker that was actually making
                        // progress — and classified plain assertion failures
                        // as bare exit failures.
                        (Some(passed), output, None)
                    }
                    // No proof step: an infra run is not an oracle
                    // observation, and recording one either way would put a
                    // fabricated pass/fail into the proof trace.
                    None => {
                        let label = post.infra_label();
                        (None, post.stderr_tail, label)
                    }
                }
            }
            None => (None, String::new(), None),
        }
    }

    /// The verdict-provenance snapshot (#865): everything the ladder decided
    /// from, frozen at decision time so `replay` answers "why fast-submit /
    /// revise / verifier?" without re-deriving — and so the verifier prompt can
    /// carry it (#864). Pure assembly over already-gathered evidence: no
    /// probe runs here.
    ///
    /// `rung` is seeded with the ladder's own decision over the same `inputs`
    /// ([`ladder_decision`] is pure, so this cannot disagree with the arm the
    /// caller takes). The three arms that resolve *past* that decision — a
    /// verifier that answered, a verifier that was unavailable, a review that was
    /// waived — restamp it with [`LadderSnapshot::with_rung`] before attaching
    /// their evidence.
    pub(super) fn ladder_snapshot(
        inputs: &LadderInputs,
        state: &CandidateState,
        test_infra: Option<&'static str>,
        witness_intact: Option<bool>,
    ) -> LadderSnapshot {
        LadderSnapshot {
            rung: Some(ladder_decision(inputs).into()),
            tracked_command: state.oracle.tracked_command().map(str::to_string),
            oracle_trace: state.oracle_trace.clone(),
            flip_achieved: inputs.flip_achieved,
            unstable_flip: state.oracle.is_unstable(),
            flip_refused_different_failure: state.oracle.refused_different_failure(),
            touched_tests_passed: inputs.touched_tests_passed,
            test_infra: test_infra.map(str::to_string),
            diff_lines: inputs.diff_lines,
            diff_budget: inputs.diff_budget,
            diff_available: inputs.diff_available,
            file_change_events: inputs.file_change_events,
            mutating_actions: inputs.mutating_actions,
            new_diag_errors: inputs.new_diag_errors,
            new_diag_warnings: inputs.new_diag_warnings,
            witness_intact,
            witness_mutation: state.witness_mutation,
            // #1291: always stated, including `unmeasured` — a reader must be
            // able to tell "the test ran the change" from "nobody looked",
            // and the absence of the field would be a third, unintended
            // meaning.
            diff_coverage: Some(inputs.diff_coverage.as_str().to_string()),
        }
    }

    /// The textual evidence summary handed to the escalated model verifier —
    /// every deterministic signal the ladder collected before deciding
    /// "inconclusive, ask a model" (#859, #860, #861, #864, #867, #870,
    /// #1291). Split out alongside [`Self::ladder_snapshot`] for the same
    /// reason: pure assembly over the same probe results, not orchestration.
    pub(super) fn model_verdict_evidence_summary(
        &self,
        inputs: &LadderInputs,
        state: &CandidateState,
        test_infra: Option<&'static str>,
        lint_sample: &str,
        snapshot: &LadderSnapshot,
    ) -> String {
        let mut evidence_summary = format!(
            "flip_achieved={}; touched_tests={:?}; diff_lines={} (budget {}); \
             file_change_events={}",
            inputs.flip_achieved,
            inputs.touched_tests_passed,
            state.diff_lines,
            self.config.diff_budget_lines,
            state.file_changes,
        );
        if let Some(label) = test_infra {
            // #860: the run ended without observing an assertion. Named so
            // the verifier reads "the suite timed out", not "the suite
            // failed".
            evidence_summary.push_str(&format!("; test_run={label} (no assertion observed)"));
        }
        if state.oracle.is_unstable() {
            // #859: a fail→pass flip WAS observed but could not be
            // reproduced on the same tree — a different fact from "the test
            // never passed", and one the verifier should weigh explicitly.
            evidence_summary
                .push_str("; unstable_flip=true (the flip's confirmation re-run did not pass)");
        }
        if inputs.diff_coverage != DiffCoverage::Unmeasured {
            // #1291: stated only when it was actually measured. An
            // `unmeasured` line here would be pure noise in a verifier
            // prompt — the ladder already escalated for some other reason,
            // and "nobody looked" adds nothing to reason from. It stays on
            // the snapshot either way.
            evidence_summary.push_str("; ");
            evidence_summary.push_str(inputs.diff_coverage.explain());
        }
        if state.witness_mutation == Some(false) {
            // #870: the witness reacted to the change without constraining
            // it — it stayed green while the changed lines were
            // deliberately broken.
            evidence_summary.push_str(
                "; witness_tautological=true (the witness stayed green under every trivial \
                 mutation of the changed lines)",
            );
        }
        if state.oracle.refused_different_failure() {
            // #867: the suite passed, but its own complete test listing did
            // not contain the baseline's failing tests — the observed
            // failure was not fixed, it disappeared. The verifier should
            // treat the passing exit code accordingly.
            evidence_summary.push_str(
                "; flip_refused=the passing run's test listing does not contain the test(s) \
                 that failed on the baseline (fixed a different failure, or the failing test \
                 was removed)",
            );
        }
        if inputs.new_diag_errors > 0 || inputs.new_diag_warnings > 0 {
            // #861: the regression the veto saw, capped to a 3-line sample
            // so the verifier reads the delta, not the linter's whole
            // opinion.
            evidence_summary.push_str(&format!(
                "; new_diagnostics={} error(s), {} warning(s) vs baseline",
                inputs.new_diag_errors, inputs.new_diag_warnings
            ));
            if !lint_sample.is_empty() {
                evidence_summary.push('\n');
                evidence_summary.push_str(lint_sample.trim_end());
            }
        }
        if !snapshot.oracle_trace.is_empty() {
            // #864: the oracle trace, rendered compactly. The verifier sees
            // WHY the ladder was inconclusive — which runs happened, in
            // order, and what each observed — instead of a diff cold.
            evidence_summary.push_str(&format!(
                "; oracle_trace=[{}]",
                crate::replay::render_oracle_trace(&snapshot.oracle_trace)
            ));
        }
        if snapshot.witness_intact == Some(true) {
            // #864: the tamper-exclusion result, stated. A tampered witness
            // never reaches a verifier, so what the verifier learns here is
            // that the check RAN and the witness it is weighing is the
            // authored one.
            evidence_summary.push_str("; witness_tamper_check=intact");
        }
        evidence_summary
    }
}
