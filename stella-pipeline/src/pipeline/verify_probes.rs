//! Post-execution observation probes for [`super::Pipeline`] — the
//! touched-tests observation that feeds the flip oracle (#860 typed
//! outcomes) and the lint delta behind the regression veto (#861).
//! A child module of `pipeline` so the private candidate types stay
//! reachable, split out to keep the orchestrator under the size gate.

use super::*;

impl<'a> Pipeline<'a> {
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
                // Token discipline: the judge gets at most three new-error
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
    /// (judge), not a deterministic red (revise). The label reaches the judge
    /// evidence so "the suite timed out" is never read as "the suite failed".
    pub(super) async fn observe_touched_tests(
        &self,
        surface: CandidateSurface<'_>,
        cmd: Option<EffectiveTestCommand<'_>>,
        state: &mut CandidateState,
    ) -> (Option<bool>, String, Option<&'static str>) {
        match cmd {
            Some(cmd) => {
                let post = surface.tests.run_test(cmd.invocation).await;
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
                        });
                        (Some(passed), post.stderr_tail, None)
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
    /// revise / judge?" without re-deriving — and so the judge prompt can
    /// carry it (#864). Pure assembly over already-gathered evidence: no
    /// probe runs here.
    pub(super) fn ladder_snapshot(
        inputs: &LadderInputs,
        state: &CandidateState,
        test_infra: Option<&'static str>,
        witness_intact: Option<bool>,
    ) -> LadderSnapshot {
        LadderSnapshot {
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
        }
    }
}
