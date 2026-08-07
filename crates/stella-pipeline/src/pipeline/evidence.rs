//! The verifier's trusted evidence summary: the `key=value` account of every
//! deterministic channel that rides ABOVE the untrusted diff in a verdict
//! prompt. Split out of `pipeline.rs` (a god file closed to growth) so the
//! summary can gain channels without growing the pipeline module.
//!
//! Everything here is part of the *trusted zone* of the verifier prompt: the
//! wording is the pipeline speaking in its own voice, never worker-authored
//! text. The one exception is the lint sample, which quotes diagnostic lines —
//! those are tool output about the change, capped upstream to a 3-line sample
//! (#861).

use super::*;

/// The verifier-facing rendering of the touched-tests channel. The `{:?}`
/// this replaces printed Rust's `None`/`Some(true)` into the prompt — and
/// `None` there reads as "no tests", which is exactly the instrument-vs-world
/// confusion the verifier instructions warn about two paragraphs earlier. The
/// instructions define `unobserved` alongside this spelling; the two must
/// stay in agreement.
fn touched_tests_status(observed: Option<bool>) -> &'static str {
    match observed {
        Some(true) => "passed",
        Some(false) => "failed",
        None => "unobserved",
    }
}

impl<'a> Pipeline<'a> {
    /// Assemble the deterministic evidence summary and the witness-stripped
    /// diff for one model-verdict round.
    ///
    /// The summary opens with the four channels the ladder itself reasons
    /// from — including `mutating_actions`, the dispatch tally that cannot go
    /// dark (#1701): without it, a verifier reading `diff_lines=0;
    /// file_change_events=0` had no way to distinguish a turn that did
    /// nothing from a turn whose ten write-capable calls landed outside what
    /// this run collects. Every conditional channel after that names a fact
    /// that was positively observed; silence means the probe had nothing to
    /// say, never that it looked and found nothing.
    ///
    /// Returns the summary together with the stripped diff so the caller's
    /// verdict-reuse digest (#1431) hashes exactly what the prompt will
    /// carry — a change to either is a change to the digest.
    pub(super) fn verifier_evidence_summary(
        state: &CandidateState,
        inputs: &LadderInputs,
        snapshot: &LadderSnapshot,
        test_infra: Option<&'static str>,
        lint_sample: &str,
        witness_paths: &[String],
    ) -> (String, crate::verify::StrippedDiff) {
        let mut evidence_summary = format!(
            "flip_achieved={}; touched_tests={}; mutating_actions={}; diff_lines={} \
             (budget {}); file_change_events={}",
            inputs.flip_achieved,
            touched_tests_status(inputs.touched_tests_passed),
            inputs.mutating_actions,
            inputs.diff_lines,
            inputs.diff_budget,
            inputs.file_change_events,
        );
        if let Some(label) = test_infra {
            // #860: the run ended without observing an assertion. Named so the
            // verifier reads "the suite timed out", not "the suite failed".
            evidence_summary.push_str(&format!("; test_run={label} (no assertion observed)"));
        }
        if state.oracle.is_unstable() {
            // #859: a fail→pass flip WAS observed but could not be reproduced
            // on the same tree — a different fact from "the test never
            // passed", and one the verifier should weigh explicitly.
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
            // it — it stayed green while the changed lines were deliberately
            // broken.
            evidence_summary.push_str(
                "; witness_tautological=true (the witness stayed green under every \
                 trivial mutation of the changed lines)",
            );
        }
        if state.oracle.refused_different_failure() {
            // #867: the suite passed, but its own complete test listing did
            // not contain the baseline's failing tests — the observed failure
            // was not fixed, it disappeared. The verifier should treat the
            // passing exit code accordingly.
            evidence_summary.push_str(
                "; flip_refused=the passing run's test listing does not contain \
                 the test(s) that failed on the baseline (fixed a different \
                 failure, or the failing test was removed)",
            );
        }
        if inputs.new_diag_errors > 0 || inputs.new_diag_warnings > 0 {
            // #861: the regression the veto saw, capped to a 3-line sample so
            // the verifier reads the delta, not the linter's whole opinion.
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
        if let Some(symptom) = state.witness_baseline_symptom {
            // #1790: the flip's arming failure never ran a test. Legitimate
            // for a missing-API goal, indistinguishable by class from
            // two-tree environment drift — so the verifier weighs it rather
            // than the pipeline silently crediting or refusing it.
            evidence_summary.push_str(&format!(
                "; witness_baseline={symptom} (the arming failure ran no test)"
            ));
        }
        if snapshot.witness_intact == Some(true) {
            // #864: the tamper-exclusion result, stated. A tampered witness
            // never reaches a verifier, so what the verifier learns here is
            // that the check RAN and the witness it is weighing is the
            // authored one.
            evidence_summary.push_str("; witness_tamper_check=intact");
        }
        // The witness's own chunks never ride into the paid prompt as
        // "worker-authored data" (#1433): they are the verifier's own
        // artifact, and everything the verdict needs to know about the
        // witness is already in the trusted evidence above. The omission is
        // named HERE — the trusted zone — never in-band in the diff, where
        // the framing says every byte is forgeable worker data.
        let stripped = crate::verify::strip_witness_hunks(&state.diff_text, witness_paths);
        if !stripped.omitted.is_empty() {
            evidence_summary.push_str(&format!(
                "; witness_files_omitted_from_diff=[{}] (verifier-authored test, \
                 not part of the change under review)",
                stripped.omitted.join(", ")
            ));
        }
        (evidence_summary, stripped)
    }
}

#[cfg(test)]
mod tests {
    use super::touched_tests_status;

    #[test]
    fn touched_tests_render_names_the_unobserved_case() {
        // The prompt must never carry Rust's `None`/`Some(..)` debug forms —
        // a verifier read `None` as "no tests exist" rather than "no run was
        // observed" (the same instrument-vs-world confusion the blindness
        // clause exists for).
        assert_eq!(touched_tests_status(Some(true)), "passed");
        assert_eq!(touched_tests_status(Some(false)), "failed");
        assert_eq!(touched_tests_status(None), "unobserved");
    }
}
