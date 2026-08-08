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
//! - [`goal_declared_complete`] is the close-out channel the prompt offers: a
//!   reply that opens or closes with an affirmative [`PLAN_COMPLETE_MARKER`]
//!   line ends the step loop.
//!
//! **The close-out is screened here, not downstream, because the backstop this
//! module originally leaned on does not always exist.** The first version
//! reasoned that a false claim is refuted by the verify stage, which runs on
//! whatever the tree actually holds. That holds only for work that lands *in
//! the workspace tree*. A task whose subject is `/etc`, `/git` or a system
//! service leaves the tree unchanged, so the diff probe finds nothing, verify
//! returns `UNVERIFIABLE`, and the verdict passes — which is how a single
//! negated echo of the marker skipped nine of ten steps for reward 0.0
//! (#2104). Most of Terminal-Bench is that shape, so the screen is the
//! backstop.

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

/// Words that turn the declaration into its own denial (#2104).
///
/// Compared against the payload's first word after punctuation is stripped and
/// case is folded, never as a prefix: `nothing left to do` is a genuine
/// completion and must not be read as `not`.
const NEGATION_OPENERS: &[&str] = &[
    "no",
    "not",
    "nope",
    "negative",
    "never",
    "false",
    "incomplete",
    "unfinished",
    "partially",
    "partial",
];

/// Whether a completed step turn declared the whole goal finished.
///
/// A declaration must **open or close the reply** and must not deny itself.
/// Both halves are load-bearing, and both come from #2104: on Terminal-Bench
/// `git-multibranch` a worker answering step 1 of 10 replied
///
/// ```text
/// All required packages are already installed.
///
/// PLAN COMPLETE: no — this is just step 1; only confirming this step is done.
///
/// Step 1/10 is already complete: ...
/// ```
///
/// and the old predicate — any line opening with the marker — closed the plan
/// and skipped steps 2–10, the entire actual task, for reward 0.0.
///
/// The position rule keeps #1702's "short report, then declare" allowance
/// exactly (the declaration is then the last significant line) while rejecting
/// a marker buried mid-reply with argument still running after it. The
/// negation rule catches the same denial when it *does* land last.
///
/// Still deliberately strict past that — no case folding of the marker, no
/// mid-line match. The asymmetry the original weighed has since been measured
/// and runs the other way: a missed declaration costs the redundant walk this
/// module exists to avoid, while a spurious match cost a whole trial's reward,
/// because the documented backstop ("verify runs on whatever the tree holds")
/// does not hold for tasks that mutate `/etc`, `/git` or system services —
/// there the diff probe sees an unchanged tree and returns `UNVERIFIABLE`.
pub(super) fn goal_declared_complete(text: &str) -> bool {
    let mut significant = text.lines().filter(|line| !line.trim().is_empty());
    let Some(first) = significant.next() else {
        return false;
    };
    let last = significant.next_back().unwrap_or(first);
    declares_completion(first) || declares_completion(last)
}

/// Whether one line is an affirmative close-out declaration.
fn declares_completion(line: &str) -> bool {
    let Some(payload) = line.trim_start().strip_prefix(PLAN_COMPLETE_MARKER) else {
        return false;
    };
    !opens_with_negation(payload)
}

/// Whether a declaration's payload begins by denying it.
///
/// A bare `PLAN COMPLETE:` is the contract [`step_prompt`] actually asks for,
/// so an empty payload is affirmative.
fn opens_with_negation(payload: &str) -> bool {
    let payload = payload.trim_start().trim_start_matches(':');
    let Some(word) = payload.split_whitespace().next() else {
        return false;
    };
    let word: String = word
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .flat_map(char::to_lowercase)
        .collect();
    // `isn't`, `doesn't`, `hasn't` — the contraction carries the negation.
    word.ends_with("n't") || NEGATION_OPENERS.contains(&word.as_str())
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
        assert!(!goal_declared_complete(
            "Step 3 done; the plan is complete."
        ));
        assert!(!goal_declared_complete(""));
    }

    /// The witness for #2104, verbatim from Terminal-Bench `git-multibranch`
    /// (match `61e01ca06fc7`, worker `claude-sonnet-5`). The old predicate
    /// closed the plan here and skipped steps 2–10 for reward 0.0.
    #[test]
    fn a_negated_echo_of_the_marker_does_not_close_the_plan() {
        let observed = "All required packages (openssh-server, git, nginx, openssl) \
                        are already installed.\n\n\
                        PLAN COMPLETE: no — this is just step 1; only confirming \
                        this step is done.\n\n\
                        Step 1/10 is already complete: the packages ship in the image.\n";
        assert!(
            !goal_declared_complete(observed),
            "a worker denying completion must not close the plan (#2104)"
        );
    }

    #[test]
    fn a_denial_is_rejected_even_when_it_is_the_last_line() {
        for denial in [
            "PLAN COMPLETE: no, steps 2-10 remain.",
            "PLAN COMPLETE: not yet — nginx is still unconfigured.",
            "PLAN COMPLETE: Not complete.",
            "PLAN COMPLETE: partially; the cert is missing.",
            "PLAN COMPLETE: isn't done, one step left.",
        ] {
            assert!(
                !goal_declared_complete(denial),
                "should not close the plan: {denial}"
            );
        }
    }

    /// The negation screen matches whole words, so a completion whose first
    /// word merely *starts* with one still closes the plan.
    #[test]
    fn an_affirmative_payload_that_looks_like_a_negation_still_closes() {
        assert!(goal_declared_complete("PLAN COMPLETE: nothing left to do."));
        assert!(goal_declared_complete(
            "PLAN COMPLETE: noted, all eight done."
        ));
        assert!(goal_declared_complete("PLAN COMPLETE:"));
        assert!(goal_declared_complete("PLAN COMPLETE"));
    }

    /// A marker buried mid-reply, with argument still running after it, is not
    /// a declaration — but the same marker opening or closing the reply is.
    #[test]
    fn the_declaration_must_open_or_close_the_reply() {
        assert!(!goal_declared_complete(
            "Step 1 is done.\nPLAN COMPLETE: everything is finished.\nBut steps 2-10 remain."
        ));
        assert!(goal_declared_complete(
            "PLAN COMPLETE: everything is finished.\n\nDetails: nginx serves TLS on 443."
        ));
        assert!(goal_declared_complete(
            "Ran the setup script.\n\n  PLAN COMPLETE: all eight steps covered.  "
        ));
    }

    #[test]
    fn the_prompt_numbers_from_one_and_carries_the_close_out_offer() {
        let prompt = step_prompt(0, 8, "Install nginx");
        assert!(prompt.starts_with("Step 1/8: Install nginx"));
        assert!(prompt.contains(PLAN_COMPLETE_MARKER));
    }
}
