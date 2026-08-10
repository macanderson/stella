//! Assemble the deterministic inputs consumed by the verification ladder.

use super::*;

impl Pipeline<'_> {
    pub(super) fn ladder_inputs(
        &self,
        state: &CandidateState,
        touched_tests_passed: Option<bool>,
        verify_done_flip: bool,
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
            // The diagnostics channel is filled in by the pre-submit audit
            // in `run_candidate`, which only runs where a credit is about to
            // be spent. `0` here is "not yet measured", and it is safe as a
            // starting value for the same reason the audit is affordable:
            // lint can only ever *withhold* a fast-submit, never grant one.
            new_diag_errors: 0,
            new_diag_warnings: 0,
            // Configuration, not evidence — and read from the config rather
            // than hardcoded, because an operator who set these got the
            // permissive behaviour silently.
            veto_warnings: self.config.diagnostics_veto_warnings,
            witness_tautological: false,
            diff_coverage: DiffCoverage::Unmeasured,
            require_diff_coverage: self.config.require_diff_coverage,
            verify_done_flip,
            no_test_surface,
            errored_commands: state.signals.errored_commands,
        }
    }
}
