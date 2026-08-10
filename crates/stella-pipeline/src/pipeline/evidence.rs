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
            new_diag_errors: 0,
            new_diag_warnings: 0,
            veto_warnings: false,
            witness_tautological: false,
            diff_coverage: DiffCoverage::Unmeasured,
            require_diff_coverage: false,
            verify_done_flip,
            no_test_surface,
            errored_commands: state.signals.errored_commands,
        }
    }
}
