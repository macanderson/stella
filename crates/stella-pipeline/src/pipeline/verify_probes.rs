//! Post-execution observation probes for [`super::Pipeline`] — the
//! touched-tests observation that feeds the flip oracle (#860 typed
//! outcomes), the lint delta behind the regression veto (#861), and the
//! working-tree diff observation (`gather_diff`/`absorb_probe`) with the
//! honest-empty-diff discipline around it.
//! A child module of `pipeline` so the private candidate types stay
//! reachable, split out to keep the orchestrator under the size gate.

use futures_util::StreamExt as _;

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
            // Stamped by the model-verdict arm alone
            // (`LadderSnapshot::with_verifier_independence`): on every other
            // rung nothing graded, so independence is not a fact about them.
            verifier_independent: None,
        }
    }

    /// The real added-line count of an untracked file, via a no-index diff
    /// numstat (`<added>\t<deleted>\t<path>`). A binary file numstats as `-`
    /// and counts as one changed line (a change the ladder must see, but
    /// unmeasurable in lines). Counting real lines — not a flat 1 per file —
    /// is what keeps a large untracked file from slipping under the diff budget
    /// and taking `SubmitFast`. A single file's numstat is one line, so this is
    /// safe against the diagnostic runner's output truncation.
    async fn untracked_added_lines(&self, surface: CandidateSurface<'_>, path: &str) -> u32 {
        let out = surface
            .diagnostics
            .run_diagnostic(&DiagnosticInvocation::UntrackedNumstat {
                path: path.to_string(),
            })
            .await;
        out.stdout_tail
            .lines()
            .find(|l| !l.trim().is_empty())
            .and_then(|l| l.split('\t').next())
            .and_then(|added| added.trim().parse::<u32>().ok())
            .unwrap_or(1)
    }

    /// The reviewable body of one untracked file's patch — its hunks, with
    /// git's file headers removed.
    ///
    /// The headers are dropped rather than passed through because
    /// [`crate::witness::warrant::changed_paths`] reads `+++ `/`--- ` lines to
    /// decide which paths a change touched, and an untracked path already
    /// reaches it through the marker line this body sits under. Letting both
    /// channels name the file would move untracked-only changes off the
    /// `paths.is_empty()` branch that keeps them
    /// [`WitnessWarrant::Required`](crate::witness::warrant::WitnessWarrant),
    /// turning "the verifier can now read the file" into a silent relaxation
    /// of when a witness is owed. One change, one effect.
    ///
    /// A binary file keeps git's `Binary files … differ` sentence instead of
    /// its bytes: that sentence is the useful evidence (there is nothing here
    /// a reviewer can read), and it is also what stops a database sidecar
    /// from pouring escape bytes into a prompt.
    async fn untracked_patch_body(&self, surface: CandidateSurface<'_>, path: &str) -> String {
        let out = surface
            .diagnostics
            .run_diagnostic(&DiagnosticInvocation::UntrackedPatch {
                path: path.to_string(),
            })
            .await;
        patch_body(&out.stdout_tail)
    }

    /// Run the diff command and return `(changed_line_count, raw_diff)`.
    ///
    /// `git diff` cannot see untracked files, so a turn whose entire change is
    /// a NEW (or edited-untracked) file would read as "no diff" — skipping
    /// verification via the zero-diff guard and showing the verifier an empty
    /// diff. When the configured command is git's, untracked files that were
    /// **created or modified this turn** — present now with either no
    /// `untracked_before` entry or a changed fingerprint — are appended with
    /// their real added-line counts. Untouched dirty files (same fingerprint)
    /// are excluded, so pre-existing state is never attributed to the turn, and
    /// a large untracked file cannot slip under the diff-size budget.
    pub(super) async fn gather_diff(
        &self,
        surface: CandidateSurface<'_>,
        untracked_before: &HashMap<String, String>,
    ) -> DiffProbe {
        let Some(diagnostic) = &self.config.diff_diagnostic else {
            // No probe configured is not "nothing changed" either: this host
            // simply has no diff channel, so it reports one fewer way to see.
            return DiffProbe {
                lines: 0,
                text: String::new(),
                available: false,
            };
        };
        let out = surface.diagnostics.run_diagnostic(diagnostic).await;
        // `git diff` exits 0 on a clean tree, so a non-zero exit with nothing on
        // stdout is the machinery failing to READ the tree, never a report that
        // the tree is unchanged. Observed in the wild as a candidate shadow
        // whose worktree registration was gone by the time it was probed
        // (`git add -A` → "fatal: not a git repository"). See
        // [`DIFF_PROBE_FAILED`] for why the empty string could not be left to
        // `verification_honest_diff` to catch. Scoped to `GitDiff` because
        // `--no-index --numstat` exits 1 whenever the files differ, which is
        // that probe's ordinary success.
        if matches!(diagnostic, DiagnosticInvocation::GitDiff)
            && !out.passed()
            && out.stdout_tail.trim().is_empty()
        {
            // git's own words, matched case-insensitively because the phrasing
            // is stable across versions but its capitalization is not.
            let not_a_repo = out
                .stderr_tail
                .to_ascii_lowercase()
                .contains("not a git repository");
            return DiffProbe {
                lines: 0,
                text: if not_a_repo {
                    DIFF_PROBE_NOT_A_REPO.to_string()
                } else {
                    DIFF_PROBE_FAILED.to_string()
                },
                available: false,
            };
        }
        let mut lines = count_diff_lines(&out.stdout_tail);
        let mut text = out.stdout_tail;
        if matches!(diagnostic, DiagnosticInvocation::GitDiff) {
            let after = surface.repo_status.untracked_fingerprints().await;
            // Created (absent before) OR modified (fingerprint changed) this
            // turn — never an untouched dirty file.
            let mut fresh: Vec<&str> = after
                .iter()
                .filter(|(path, fp)| untracked_before.get(*path) != Some(*fp))
                .map(|(path, _)| path.as_str())
                .collect();
            fresh.sort(); // deterministic order for the appended evidence
            // Each of these is a `git diff --no-index --numstat` subprocess.
            // Run sequentially, a turn that creates many untracked files paid
            // one full process round-trip per file — on every verification
            // observation and again after every revision, so the cost is
            // repaid per revision per candidate.
            //
            // Bounded concurrency rather than a truncating cap: this count
            // feeds the zero-diff guard and the diff-size budget, so dropping
            // the tail would let a large untracked change slip under a budget
            // it should have tripped. `buffered` preserves input order, so
            // the appended evidence stays deterministic.
            let counted: Vec<(&str, u32, String)> =
                futures_util::stream::iter(fresh.into_iter().map(|path| async move {
                    let (added, body) = futures_util::join!(
                        self.untracked_added_lines(surface, path),
                        self.untracked_patch_body(surface, path),
                    );
                    (path, added, body)
                }))
                .buffered(UNTRACKED_NUMSTAT_CONCURRENCY)
                .collect()
                .await;
            for (path, added, body) in counted {
                lines += added;
                // The shared builder, so `witness::warrant` can parse these
                // paths back out and hold them to the same path rules as
                // tracked changes — a hand-rolled format here silently
                // blinded the warrant to every untracked file.
                text.push('\n');
                text.push_str(&crate::witness::warrant::untracked_change_line(path, added));
                // …and the content underneath it, so a verifier grades the
                // change rather than the filename (#2027). The marker alone
                // is a line count: a run whose entire deliverable was one
                // untracked file was graded PASS by a verifier that said so
                // in as many words — "the unseen content cannot itself
                // justify a FAIL".
                if !body.is_empty() {
                    text.push('\n');
                    text.push_str(&body);
                }
            }
        }
        DiffProbe {
            lines,
            text,
            available: true,
        }
    }

    /// Mutating file touches this candidate has made, read from the recorder
    /// that emitted the `FileChange` events rather than from a wrapper around
    /// the engine's sender.
    ///
    /// This is the fix for the counter that read `0` while six `file_change`
    /// events sat in the very stream the verifier's run produced (#973). The two
    /// numbers came from different wires: `ToolRegistry::record_touch` sends to
    /// the channel the *host* attached, and the tally lived on a sender the
    /// pipeline handed the *engine*, which no file tool ever uses.
    ///
    /// `max` of the two, because they are independent lower bounds rather than
    /// a sum: the recorder's delta is authoritative wherever a `FileTouchPort`
    /// is wired, and the event tally is the only signal for a host with no
    /// recorder at all. Neither can double-count the other — the sender the
    /// tally wraps is built per-turn inside this pipeline and never handed out.
    fn observed_mutations(&self, state: &CandidateState) -> u32 {
        let delta = self
            .touches
            .mutations_recorded()
            .saturating_sub(state.touch_baseline);
        u32::try_from(delta)
            .unwrap_or(u32::MAX)
            .max(state.signals.file_changes)
    }

    /// Fold one working-tree observation into `state`, refreshing every
    /// evidence channel from its own source at the same instant.
    ///
    /// One function, so the channels cannot be updated apart and disagree about
    /// which round they describe — the honest-diff text in particular is built
    /// from the touch count, and reading a stale one is how a real change gets
    /// rendered as an empty diff.
    pub(super) fn absorb_probe(&self, state: &mut CandidateState, probe: DiffProbe) {
        // Attribute shell-driven changes BEFORE the count is read. The
        // recorder only sees what a tool's input declared, and `bash` declares
        // nothing, so without this the count below is blind to the tool that
        // does most of the work on Terminal-Bench. Settling first is what
        // makes `file_changes` an honest answer rather than a CRUD-tool tally.
        // Settling also re-arms, from the same walk, so a revision's changes
        // are bracketed exactly the way this one's were without paying for a
        // second walk of an identical tree.
        self.touches.settle_workspace_probe();
        state.signals.file_changes = self.observed_mutations(state);
        // The authored channel is read AFTER settling, for the same reason the
        // count is: the probe's attributions are recorded through the same
        // emitter, so reading first would describe the turn before its opaque
        // calls had been accounted for.
        let authored = self.touches.authored_diff();
        // `max`, not a sum — these are two independent LOWER BOUNDS on one
        // number, not two disjoint tallies. A tracked file edited through
        // `write_file` is counted by both, and adding them would double it.
        // (The same reasoning as `observed_mutations` above.)
        state.diff_lines = probe.lines.max(authored.lines);
        // Deliberately NOT widened by the authored channel. `diff_available`
        // means "the configured probe could read the working tree", and the
        // ladder's blind-evidence rung is built on that exact claim. An
        // authored diff proves what was written, never what survived to sit on
        // disk at the end of the turn — so it informs the verifier without
        // asserting that the tree was successfully re-read.
        state.diff_available = probe.available;
        state.diff_text = verification_honest_diff(
            authored::splice_authored(probe.text, &authored),
            state.signals.file_changes,
        );
    }
}

/// An empty diff is ambiguous: it can mean the agent genuinely changed
/// nothing, OR that the diff machinery is blind to changes that DID happen
/// (work that was committed, a baseline mismatch, an uncaptured file). Handed
/// a bare empty string, a verifier reads the second case as the first and
/// concludes "no changes were made" — the archetypal verification lie that
/// once drove an agent to reinitialize git to beat the check.
///
/// The pipeline already has the independent signal that resolves the
/// ambiguity: `file_changes`, the count of `FileChange` events the turn
/// emitted. When that is positive but the diff is empty, the honest report is
/// "the tree changed but the diff could not be captured" — a *couldn't
/// verify*, never a *verified nothing*. Every consumer (verifier, guidance,
/// evidence) then sees the truth.
pub(super) fn verification_honest_diff(diff_text: String, file_changes: u32) -> String {
    if file_changes > 0 && diff_text.trim().is_empty() {
        format!(
            "[{file_changes} file-change event(s) were observed this turn, but the diff could \
             not be captured — the change is real; the diff is blind to it. This is NOT evidence \
             that nothing changed; verify the result on its own merits.]"
        )
    } else {
        diff_text
    }
}

/// What [`Pipeline::gather_diff`] reports when the diff probe *failed* rather
/// than found nothing.
///
/// `verification_honest_diff` resolves the same ambiguity from `file_changes`,
/// but that signal is structurally unavailable to an isolated candidate: the
/// engine emits no `FileChange` events (the deck's `FileChangeTap` synthesizes
/// them, and it wraps only the SESSION tool stack), so inside a best-of-N or
/// witness candidate the count is always zero. A candidate whose probe went
/// blind therefore handed an empty string to [`crate::witness::warrant`], which
/// read it as [`crate::NoWitnessReason::NothingChanged`] and completed the run
/// with a PASSING verdict stating no behavior had changed — the exact
/// verification lie the honest-diff guard exists to prevent, reached with no
/// witness stage and no warning. Non-empty text keeps the warrant fail-closed
/// on the one path its own guard could not see.
const DIFF_PROBE_FAILED: &str = "[the diff probe failed; the working tree could not be read. This \
     is NOT evidence that nothing changed — verify the result on its own merits.]";

/// The same report, for the one cause worth naming separately: there is no git
/// repository here, so `git diff` can never answer.
///
/// Terminal-Bench task images are plain directories. `DIFF_PROBE_FAILED` reads
/// as a transient fault a reader might expect to clear on a retry; this one
/// says the probe is structurally inapplicable to this workspace, which is the
/// difference between "try again" and "use another channel" (#973).
const DIFF_PROBE_NOT_A_REPO: &str = "[the working tree is not a git repository, so the diff probe cannot read it at all. This is \
     a permanent property of this workspace, NOT evidence that nothing changed — verify the \
     result on its own merits.]";

/// One observation of the working tree by [`Pipeline::gather_diff`].
///
/// `available` is the field that did not exist and had to: without it, "the
/// probe read the tree and it was unchanged" and "the probe could not read the
/// tree" both arrived as `(0, ...)`, and every consumer downstream was free to
/// read the second as the first. The text has said the right thing for a while;
/// nothing could act on it, because prose is not a signal a ladder can branch
/// on.
pub(super) struct DiffProbe {
    pub(super) lines: u32,
    pub(super) text: String,
    /// Whether the probe could read the working tree AT ALL. Never `false`
    /// merely because the diff came back empty.
    pub(super) available: bool,
}

/// How many `git diff --no-index --numstat` probes [`Pipeline::gather_diff`]
/// keeps in flight at once. High enough that the usual handful of new files
/// costs roughly one round-trip instead of N, low enough that a turn which
/// creates hundreds cannot fork an unbounded burst of git processes.
const UNTRACKED_NUMSTAT_CONCURRENCY: usize = 16;

/// Reduce one `git diff --no-index -- /dev/null <path>` patch to the part a
/// reviewer can read: the hunks, or git's binary sentence.
///
/// Everything above the first `@@` is git's file-header preamble
/// (`diff --git`, `new file mode`, `index`, `--- `, `+++ `), which
/// [`Pipeline::untracked_patch_body`] explains must not survive into the
/// diff text. An output with neither a hunk nor a binary sentence — an empty
/// probe, a failed one — contributes nothing rather than a confusing
/// fragment; the marker line above it still states the file changed.
fn patch_body(raw: &str) -> String {
    if let Some(start) = raw.find("\n@@ ") {
        return raw[start + 1..].trim_end().to_string();
    }
    if raw.starts_with("@@ ") {
        return raw.trim_end().to_string();
    }
    raw.lines()
        .find(|line| line.starts_with("Binary files ") && line.ends_with(" differ"))
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod patch_body_tests {
    use super::patch_body;

    /// Exactly what `git diff --no-index -- /dev/null <path>` prints for a
    /// newly created text file.
    const CREATED: &str = "diff --git a/dev/null b/regex.txt\n\
                           new file mode 100644\n\
                           index 0000000..9f2c1d3\n\
                           --- /dev/null\n\
                           +++ b/regex.txt\n\
                           @@ -0,0 +1,2 @@\n\
                           +^\\d{4}-\\d{2}-\\d{2}$\n\
                           +second line\n";

    #[test]
    fn the_content_survives() {
        let body = patch_body(CREATED);
        assert!(body.starts_with("@@ -0,0 +1,2 @@"), "{body}");
        assert!(body.contains("+^\\d{4}-\\d{2}-\\d{2}$"), "{body}");
        assert!(body.contains("+second line"), "{body}");
    }

    /// The restriction that keeps this change from also relaxing the warrant:
    /// no `+++ `/`--- ` line may survive, because
    /// [`crate::witness::warrant::changed_paths`] reads those as the set of
    /// paths a change touched, and the marker line already names this one.
    #[test]
    fn no_header_survives_for_changed_paths_to_read() {
        let body = patch_body(CREATED);
        assert!(
            crate::witness::warrant::changed_paths(&body).is_empty(),
            "the body named a path to the warrant: {body}"
        );
        for line in body.lines() {
            assert!(!line.starts_with("+++ "), "{line}");
            assert!(!line.starts_with("--- "), "{line}");
            assert!(!line.starts_with("diff --git"), "{line}");
        }
    }

    /// A database sidecar is the case that must never spill bytes into a
    /// prompt — git says so itself, and that sentence is the evidence.
    #[test]
    fn binary_content_is_reported_but_never_rendered() {
        let raw = "diff --git a/dev/null b/.stella/private/store.db\n\
                   Binary files /dev/null and b/.stella/private/store.db differ\n";
        assert_eq!(
            patch_body(raw),
            "Binary files /dev/null and b/.stella/private/store.db differ"
        );
    }

    /// A probe that failed, or a host with no patch channel, contributes
    /// nothing rather than a fragment. The marker line above it still states
    /// the file changed.
    #[test]
    fn an_unreadable_probe_contributes_nothing() {
        assert_eq!(patch_body(""), "");
        assert_eq!(patch_body("fatal: not a git repository\n"), "");
    }
}
