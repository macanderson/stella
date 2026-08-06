//! Plan-step prompts and the worker's early close-out (#1702).
//!
//! `Pipeline::execute_plan` used to walk every plan step unconditionally: a
//! worker that finished the whole task in step 1 — the normal outcome when a
//! plan's steps are one shell script's worth of work — was then marched
//! through the remaining steps re-confirming its own finished work, at two
//! model calls per step over a transcript that grows with every one. On a
//! measured Terminal-Bench-shaped run, 63% of the run's cost bought no work.
//!
//! Two things fix that here, both pure text so the loop in `pipeline.rs`
//! stays a loop:
//!
//! - [`step_prompt`] tells the worker the truth about the plan: earlier steps
//!   may already have covered this one, confirming finished work is a no-op
//!   to report in one line, and step descriptions are sequencing hints rather
//!   than specs (the `e.g.` in a planner's step is illustration, and workers
//!   were observed renaming working identifiers to match it).
//! - [`goal_declared_complete`] is the close-out channel the prompt offers:
//!   a reply line opening with [`PLAN_COMPLETE_MARKER`] ends the step loop.
//!
//! A false claim is not trusted: the verify stage still runs on whatever the
//! tree actually holds, so a worker that closes out early with the goal
//! genuinely unmet is refuted by the same ladder that refutes any other lie.
//! Skipping the walk trades re-confirmation calls, never verification.

/// The line a worker opens with to declare the whole goal finished and end
/// the step loop. Matched at line start (after leading whitespace) so prose
/// that merely mentions the phrase mid-sentence does not close the plan.
pub(super) const PLAN_COMPLETE_MARKER: &str = "PLAN COMPLETE";

/// The user message driving one plan step: the step itself, then the
/// standing truths of #1702 — already-done work is reported, not redone;
/// descriptions are hints, not specs; and a finished goal is declared with
/// [`PLAN_COMPLETE_MARKER`] instead of walked to the end.
pub(super) fn step_prompt(index: usize, total: usize, description: &str) -> String {
    format!(
        "Step {}/{}: {}\n\n\
         Earlier steps may already have covered this one. If this step is \
         already done, say so in one line and make no tool calls — do not \
         re-verify finished work. Step descriptions are sequencing hints, not \
         specs: names and identifiers in them (especially after \"e.g.\") are \
         illustrative, so do not rename working code to match one. If the \
         ENTIRE goal is already complete, reply with a single line beginning \
         `{PLAN_COMPLETE_MARKER}:` and the remaining steps will be skipped.",
        index + 1,
        total,
        description,
    )
}

/// Whether a completed step turn declared the whole goal finished.
///
/// Any line whose first non-whitespace characters are the marker counts, so
/// a worker that prefaces the declaration with a short report still closes
/// the plan. Deliberately strict past that — no case folding, no mid-line
/// match — because the failure modes are asymmetric: a missed declaration
/// costs the redundant walk this module exists to avoid, while a spurious
/// match skips steps the verify stage must then catch.
pub(super) fn goal_declared_complete(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim_start().starts_with(PLAN_COMPLETE_MARKER))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_line_closes_the_plan_wherever_it_sits_in_the_reply() {
        assert!(goal_declared_complete("PLAN COMPLETE: nginx is serving."));
        assert!(goal_declared_complete(
            "All eight steps were covered by the setup script.\n  PLAN COMPLETE: verified."
        ));
    }

    #[test]
    fn prose_mentioning_the_phrase_mid_line_does_not() {
        assert!(!goal_declared_complete(
            "I will reply with PLAN COMPLETE once the config is verified."
        ));
        assert!(!goal_declared_complete("Step 3 done; the plan is complete."));
        assert!(!goal_declared_complete(""));
    }

    #[test]
    fn the_prompt_numbers_from_one_and_carries_the_close_out_offer() {
        let prompt = step_prompt(0, 8, "Install nginx");
        assert!(prompt.starts_with("Step 1/8: Install nginx"));
        assert!(prompt.contains(PLAN_COMPLETE_MARKER));
    }
}
