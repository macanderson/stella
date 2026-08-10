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

impl<'a> Pipeline<'a> {
    /// Assemble one verification round's [`LadderInputs`] from the candidate's
    /// accumulated state — the evidence record `ladder_decision` and the
    /// fallback verdict reason over. Lives here rather than in
    /// `verify_candidate` for the same reason the summary below does:
    /// `pipeline.rs` is closed to growth, and this literal is the seam that
    /// grows whenever the ladder learns a channel (#2129 added two).
    ///
    /// The diagnostics, tautology, and coverage fields start at their
    /// "nothing observed" values on purpose: the pre-submit audit
    /// (#861, #870, #1291) fills them only when a fast-submit is imminent.
    pub(super) fn ladder_inputs(
        &self,
        state: &CandidateState,
        touched_tests_passed: Option<bool>,
        no_test_surface: bool,
    ) -> LadderInputs {
        LadderInputs {
            flip_achieved: state.oracle.is_flipped(),
            touched_tests_passed,
            diff_lines: state.diff_lines,
            diff_budget: self.config.diff_budget_lines,
            diff_available: state.diff_available,
            file_change_events: state.signals.file_changes,
            mutating_actions: state.signals.mutating_actions,
            new_diag_errors: 0,
            new_diag_warnings: 0,
            veto_warnings: self.config.diagnostics_veto_warnings,
            witness_mutation: MutationAudit::Unmeasured,
            diff_coverage: DiffCoverage::Unmeasured,
            require_diff_coverage: self.config.require_diff_coverage,
            verify_done_flip: state.signals.verify_done_confirmations > 0,
            no_test_surface,
            errored_commands: state.signals.errored_commands,
        }
    }
}
